//! Bitcoin-адаптер на `rust-bitcoin` + Esplora HTTP (testnet).
//!
//! Кошельки у нас legacy P2PKH. UTXO-модель ложится на трейт так: по одному `SigningRequest`
//! на каждый вход (legacy sighash). scriptSig собирается из подписи — публичный ключ
//! восстанавливается из recoverable ECDSA-подписи, так что сам ключ адаптеру не нужен.

use std::collections::BTreeMap;
use std::str::FromStr;

use bitcoin::hashes::Hash;
use bitcoin::script::Builder;
use bitcoin::secp256k1::{ecdsa::RecoverableSignature, ecdsa::RecoveryId, Message, Secp256k1};
use bitcoin::sighash::{EcdsaSighashType, SighashCache};
use bitcoin::{
    absolute::LockTime, transaction::Version, Address, Amount as BtcAmount, Network, OutPoint,
    Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use core_domain::{Amount, Chain, U256};
use serde::Deserialize;

use blockchain::{
    BalanceView, BlockchainClient, ChainError, SignScheme, SignedTransaction, SigningRequest,
    TxObservation, UnsignedTransaction, WithdrawRequest,
};

/// Оценка vsize простого перевода (1-in / 2-out, legacy P2PKH) — для комиссии.
const APPROX_VSIZE: u64 = 226;

#[derive(Deserialize)]
struct EsploraUtxo {
    txid: String,
    vout: u32,
    value: u64,
}

#[derive(Deserialize)]
struct EsploraTxStatus {
    confirmed: bool,
    block_height: Option<u64>,
}

/// Esplora-клиент (blockstream/mempool testnet API).
pub struct BtcClient {
    esplora_url: String,
    http: reqwest::Client,
    config: blockchain::ChainConfig,
}

#[derive(Deserialize)]
struct EsploraStats {
    funded_txo_sum: u64,
    spent_txo_sum: u64,
}

#[derive(Deserialize)]
struct EsploraAddress {
    chain_stats: EsploraStats,
}

impl BtcClient {
    pub fn new(esplora_url: &str, config: blockchain::ChainConfig) -> Self {
        Self {
            esplora_url: esplora_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            config,
        }
    }

    /// Валидация адреса: формат + сеть (только testnet).
    pub fn parse_testnet_address(&self, address: &str) -> Result<Address, ChainError> {
        address
            .parse::<Address<_>>()
            .map_err(|_| ChainError::InvalidAddress(Chain::Bitcoin))?
            .require_network(Network::Testnet)
            .map_err(|_| ChainError::InvalidAddress(Chain::Bitcoin))
    }

    pub async fn balance(&self, address: &str) -> Result<U256, ChainError> {
        self.parse_testnet_address(address)?;
        let url = format!("{}/address/{address}", self.esplora_url);
        let stats: EsploraAddress = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .json()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        // Баланс = поступившее − потраченное (в сатоши).
        let sats = stats
            .chain_stats
            .funded_txo_sum
            .saturating_sub(stats.chain_stats.spent_txo_sum);
        Ok(U256::from(sats))
    }

    /// Непотраченные выходы адреса (Esplora `GET /address/{addr}/utxo`).
    async fn fetch_utxos(&self, address: &str) -> Result<Vec<EsploraUtxo>, ChainError> {
        let url = format!("{}/address/{address}/utxo", self.esplora_url);
        self.http
            .get(&url)
            .send()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .json()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))
    }

    pub fn balance_view(&self, total: U256) -> BalanceView {
        BalanceView {
            total: Amount::new(Chain::Bitcoin, total),
            reserved: Amount::new(Chain::Bitcoin, U256::ZERO),
            spendable: Amount::new(Chain::Bitcoin, total),
        }
    }

    pub fn config(&self) -> &blockchain::ChainConfig {
        &self.config
    }
}

#[async_trait::async_trait]
impl BlockchainClient for BtcClient {
    fn chain(&self) -> Chain {
        Chain::Bitcoin
    }

    fn config(&self) -> &blockchain::ChainConfig {
        &self.config
    }

    fn validate_address(&self, address: &str) -> Result<(), ChainError> {
        self.parse_testnet_address(address).map(|_| ())
    }

    async fn get_balance(&self, address: &str) -> Result<BalanceView, ChainError> {
        let total = self.balance(address).await?;
        Ok(self.balance_view(total))
    }

    async fn estimate_fee(&self, _req: &WithdrawRequest) -> Result<Amount, ChainError> {
        // Esplora /fee-estimates → sat/vB по целям подтверждения; берём цель «6 блоков».
        let url = format!("{}/fee-estimates", self.esplora_url);
        let rates: BTreeMap<String, f64> = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .json()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        let sat_per_vb = rates.get("6").copied().unwrap_or(1.0).max(1.0);
        let fee_sats = (sat_per_vb * APPROX_VSIZE as f64).ceil() as u64;
        Ok(Amount::new(Chain::Bitcoin, U256::from(fee_sats)))
    }

    async fn build_unsigned(
        &self,
        req: &WithdrawRequest,
        fee: &Amount,
    ) -> Result<UnsignedTransaction, ChainError> {
        let from = self.parse_testnet_address(&req.from_address)?;
        let to = self.parse_testnet_address(&req.to_address)?;

        let amount_sat = u64::try_from(req.amount.raw)
            .map_err(|_| ChainError::Rpc("amount exceeds u64 sats".into()))?;
        let fee_sat =
            u64::try_from(fee.raw).map_err(|_| ChainError::Rpc("fee exceeds u64 sats".into()))?;
        if amount_sat < u64::try_from(self.config.dust_limit).unwrap_or(u64::MAX) {
            return Err(ChainError::BelowDust);
        }

        // Жадный coin-selection: набираем UTXO, пока не покроем сумму + комиссию.
        let utxos = self.fetch_utxos(&req.from_address).await?;
        let target = amount_sat
            .checked_add(fee_sat)
            .ok_or_else(|| ChainError::Rpc("amount+fee overflow".into()))?;
        let mut selected = Vec::new();
        let mut input_sum = 0u64;
        for u in utxos {
            input_sum = input_sum.saturating_add(u.value);
            selected.push(u);
            if input_sum >= target {
                break;
            }
        }
        if input_sum < target {
            return Err(ChainError::InsufficientFunds);
        }

        // Все входы тратим как legacy P2PKH (наши кошельки именно такие): scriptCode для
        // sighash — это scriptPubKey адреса-владельца.
        let prev_script = from.script_pubkey();
        let inputs: Vec<TxIn> = selected
            .iter()
            .map(|u| {
                let txid =
                    Txid::from_str(&u.txid).map_err(|_| ChainError::Rpc("bad utxo txid".into()))?;
                Ok(TxIn {
                    previous_output: OutPoint { txid, vout: u.vout },
                    script_sig: bitcoin::ScriptBuf::new(), // заполняется в assemble_signed
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: Witness::new(),
                })
            })
            .collect::<Result<_, ChainError>>()?;

        let mut outputs = vec![TxOut {
            value: BtcAmount::from_sat(amount_sat),
            script_pubkey: to.script_pubkey(),
        }];
        // Сдача обратно на адрес-владелец, если выше dust.
        let change = input_sum - target;
        let dust = u64::try_from(self.config.dust_limit).unwrap_or(546);
        if change >= dust {
            outputs.push(TxOut {
                value: BtcAmount::from_sat(change),
                script_pubkey: prev_script.clone(),
            });
        }

        let tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: inputs,
            output: outputs,
        };

        // Legacy sighash (SIGHASH_ALL) на каждый вход → по запросу на подпись.
        let cache = SighashCache::new(&tx);
        let mut requests = Vec::with_capacity(tx.input.len());
        for i in 0..tx.input.len() {
            let sighash = cache
                .legacy_signature_hash(i, &prev_script, EcdsaSighashType::All.to_u32())
                .map_err(|e| ChainError::Rpc(format!("sighash: {e}")))?;
            requests.push(SigningRequest {
                scheme: SignScheme::EcdsaPrehash,
                derivation_path: req.derivation_path.clone(),
                payload: sighash.to_byte_array().to_vec(),
            });
        }

        let context = bitcoin::consensus::encode::serialize(&tx);
        Ok(UnsignedTransaction {
            chain: Chain::Bitcoin,
            requests,
            context,
            // UTXO-модель: ни nonce, ни blockhash — отслеживать по hash достаточно.
            tracking: None,
        })
    }

    fn assemble_signed(
        &self,
        unsigned: &UnsignedTransaction,
        signatures: &[Vec<u8>],
    ) -> Result<SignedTransaction, ChainError> {
        let mut tx: Transaction = bitcoin::consensus::encode::deserialize(&unsigned.context)
            .map_err(|e| ChainError::Rpc(format!("deserialize tx: {e}")))?;
        if signatures.len() != tx.input.len() || signatures.len() != unsigned.requests.len() {
            return Err(ChainError::Rpc(format!(
                "expected {} signatures, got {}",
                tx.input.len(),
                signatures.len()
            )));
        }

        let secp = Secp256k1::verification_only();
        for (i, sig) in signatures.iter().enumerate() {
            if sig.len() != 65 {
                return Err(ChainError::Rpc(
                    "signature must be 65 bytes (r||s||v)".into(),
                ));
            }
            // Восстанавливаем pubkey из recoverable-подписи: адаптер ключей не держит,
            // а legacy scriptSig обязан содержать pubkey владельца.
            let recid = RecoveryId::from_i32(sig[64] as i32)
                .map_err(|e| ChainError::Rpc(format!("recovery id: {e}")))?;
            let rec_sig = RecoverableSignature::from_compact(&sig[0..64], recid)
                .map_err(|e| ChainError::Rpc(format!("recoverable sig: {e}")))?;
            let msg = Message::from_digest_slice(&unsigned.requests[i].payload)
                .map_err(|e| ChainError::Rpc(format!("sighash msg: {e}")))?;
            let pubkey = secp
                .recover_ecdsa(&msg, &rec_sig)
                .map_err(|e| ChainError::Rpc(format!("recover pubkey: {e}")))?;

            // scriptSig = <DER-подпись + SIGHASH_ALL> <compressed pubkey>.
            let mut der = rec_sig.to_standard().serialize_der().to_vec();
            der.push(EcdsaSighashType::All as u8);
            let der_push = bitcoin::script::PushBytesBuf::try_from(der)
                .map_err(|_| ChainError::Rpc("der too long".into()))?;
            let key_push = bitcoin::script::PushBytesBuf::try_from(pubkey.serialize().to_vec())
                .map_err(|_| ChainError::Rpc("pubkey too long".into()))?;
            tx.input[i].script_sig = Builder::new()
                .push_slice(der_push)
                .push_slice(key_push)
                .into_script();
        }

        let raw = bitcoin::consensus::encode::serialize(&tx);
        Ok(SignedTransaction {
            chain: Chain::Bitcoin,
            raw,
        })
    }

    fn txid(&self, signed: &SignedTransaction) -> Result<String, ChainError> {
        // txid = двойной SHA256 сериализованной транзакции; rust-bitcoin считает его сам.
        let tx: Transaction = bitcoin::consensus::encode::deserialize(&signed.raw)
            .map_err(|e| ChainError::Rpc(format!("deserialize tx: {e}")))?;
        Ok(tx.compute_txid().to_string())
    }

    async fn broadcast(&self, signed: &SignedTransaction) -> Result<String, ChainError> {
        // Esplora POST /tx принимает raw-транзакцию в hex; возвращает txid.
        let url = format!("{}/tx", self.esplora_url);
        let hex = signed
            .raw
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let txid = self
            .http
            .post(&url)
            .body(hex)
            .send()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .text()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        Ok(txid)
    }

    async fn tx_status(
        &self,
        tx_hash: &str,
        _from_address: &str,
        _tracking: Option<&str>,
    ) -> Result<TxObservation, ChainError> {
        // Статус транзакции: confirmed + высота блока (Esplora /tx/{txid}/status).
        // У Bitcoin нет понятий «провалена»/«истекла»: невключённая транзакция просто
        // отсутствует (NotFound) либо ждёт в мемпуле (Pending{0}).
        let url = format!("{}/tx/{tx_hash}/status", self.esplora_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(TxObservation::NotFound);
        }
        let status: EsploraTxStatus = resp
            .json()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        if !status.confirmed {
            return Ok(TxObservation::Pending { confirmations: 0 });
        }
        let Some(height) = status.block_height else {
            return Ok(TxObservation::Pending { confirmations: 1 });
        };
        // Высота вершины цепи (/blocks/tip/height) → подтверждения = tip − height + 1.
        let tip_url = format!("{}/blocks/tip/height", self.esplora_url);
        let tip: u64 = self
            .http
            .get(&tip_url)
            .send()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .text()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .trim()
            .parse()
            .map_err(|_| ChainError::Rpc("bad tip height".into()))?;
        Ok(TxObservation::Pending {
            confirmations: tip.saturating_sub(height) + 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> blockchain::ChainConfig {
        blockchain::ChainConfig {
            chain: Chain::Bitcoin,
            decimals: 8,
            confirmations: Some(2),
            reorg_window: 6,
            dust_limit: U256::from(546u64),
            fee_cap_factor: 2.0,
        }
    }

    #[test]
    fn validates_testnet_address() {
        let c = BtcClient::new("https://blockstream.info/testnet/api", cfg());
        // testnet P2PKH (начинается с m/n)
        assert!(c
            .parse_testnet_address("mipcBbFg9gMiCh81Kj8tqqdgoZub1ZJRfn")
            .is_ok());
        assert!(c.parse_testnet_address("not-an-address").is_err());
    }

    #[test]
    fn rejects_mainnet_address_on_testnet() {
        let c = BtcClient::new("https://blockstream.info/testnet/api", cfg());
        // mainnet P2PKH (1...) — на testnet отклоняется.
        assert!(c
            .parse_testnet_address("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa")
            .is_err());
    }

    /// Сборка scriptSig из recoverable-подписей: проверяем без сети (build_unsigned
    /// требует Esplora). Контекст — настоящая 2-входовая транзакция, подписи реальные.
    #[test]
    fn assemble_fills_scriptsig_and_validates_counts() {
        use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};

        let c = BtcClient::new("https://blockstream.info/testnet/api", cfg());
        let txid =
            Txid::from_str("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let mk_in = |vout| TxIn {
            previous_output: OutPoint { txid, vout },
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::new(),
        };
        let tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![mk_in(0), mk_in(1)],
            output: vec![TxOut {
                value: BtcAmount::from_sat(1000),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        };
        let context = bitcoin::consensus::encode::serialize(&tx);

        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[0x11u8; 32]).unwrap();
        let sign = |digest: [u8; 32]| {
            let msg = Message::from_digest_slice(&digest).unwrap();
            let (recid, compact) = secp.sign_ecdsa_recoverable(&msg, &sk).serialize_compact();
            let mut out = compact.to_vec();
            out.push(recid.to_i32() as u8);
            out
        };
        let p0 = [0xaau8; 32];
        let p1 = [0xbbu8; 32];
        let req = |p: [u8; 32]| SigningRequest {
            scheme: SignScheme::EcdsaPrehash,
            derivation_path: "m/44'/1'/0'/0/0".into(),
            payload: p.to_vec(),
        };
        let unsigned = UnsignedTransaction {
            chain: Chain::Bitcoin,
            requests: vec![req(p0), req(p1)],
            context,
            tracking: None,
        };
        let sigs = vec![sign(p0), sign(p1)];

        // Несовпадение числа подписей и битая длина — отказ.
        assert!(c.assemble_signed(&unsigned, &sigs[..1]).is_err());
        assert!(c
            .assemble_signed(&unsigned, &[vec![0u8; 10], vec![0u8; 10]])
            .is_err());

        // Оба входа получают непустой scriptSig, число входов сохраняется.
        let signed = c.assemble_signed(&unsigned, &sigs).unwrap();
        let rebuilt: Transaction = bitcoin::consensus::encode::deserialize(&signed.raw).unwrap();
        assert_eq!(rebuilt.input.len(), 2);
        assert!(!rebuilt.input[0].script_sig.is_empty());
        assert!(!rebuilt.input[1].script_sig.is_empty());
    }
}
