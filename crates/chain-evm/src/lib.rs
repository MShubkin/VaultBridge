//! EVM-адаптер на `alloy`: реальные RPC-вызовы (баланс, комиссия, broadcast) плюс сборка
//! и подпись EIP-1559 транзакции. Реализует `BlockchainClient`.
//!
//! Подпись внешняя: адаптер строит sighash, отдаёт его наружу и собирает raw из готовой
//! подписи. Сами ключи в адаптер не попадают.
//!
//! Для рантайма нужен EVM JSON-RPC (например, Sepolia). В этой кодовой базе путь проверен
//! компиляцией и юнит-тестами детерминированных кусков; интеграционные тесты включаются
//! только при заданном `EVM_RPC_URL`.

use std::str::FromStr;

use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::primitives::{
    keccak256, Address, Bytes, PrimitiveSignature, TxKind, B256, U256 as AlloyU256,
};
use alloy::providers::{Provider, ProviderBuilder};
use async_trait::async_trait;
use core_domain::{Amount, Chain, U256};

use blockchain::{
    BalanceView, BlockchainClient, ChainConfig, ChainError, SignScheme, SignedTransaction,
    SigningRequest, TxObservation, UnsignedTransaction, WithdrawRequest,
};

/// Газ на простой перевод ETH — фиксированные 21000. Контрактных вызовов адаптер не делает,
/// поэтому лимит константный, а не оценивается через `eth_estimateGas`.
const ETH_TRANSFER_GAS: u128 = 21_000;

/// Наш `U256` → `U256` из alloy. Оба 256-битные, поэтому перегоняем через big-endian байты
/// без потерь.
fn to_alloy(v: U256) -> AlloyU256 {
    AlloyU256::from_be_bytes(v.to_be_bytes::<32>())
}
/// Обратное преобразование: `U256` alloy → наш доменный `U256`.
fn from_alloy(v: AlloyU256) -> U256 {
    U256::from_be_bytes(v.to_be_bytes::<32>())
}

/// HTTP-провайдер alloy. Вынесен в алиас, чтобы длинный вложенный тип не тянулся по коду.
type DynProvider =
    alloy::providers::RootProvider<alloy::transports::http::Http<alloy::transports::http::Client>>;

/// EVM-адаптер: держит HTTP-подключение к узлу и конфиг сети.
pub struct EvmClient {
    /// Провайдер для JSON-RPC вызовов к узлу.
    provider: DynProvider,
    /// Параметры сети (decimals, порог подтверждений и т.п.).
    config: ChainConfig,
}

impl EvmClient {
    /// Создать адаптер по URL узла. Плохой URL — сразу ошибка, соединение ленивое (RPC
    /// поднимается при первом запросе).
    pub fn new(rpc_url: &str, config: ChainConfig) -> Result<Self, ChainError> {
        let url = rpc_url
            .parse()
            .map_err(|_| ChainError::Rpc("bad rpc url".into()))?;
        let provider = ProviderBuilder::new().on_http(url);
        Ok(Self { provider, config })
    }

    /// Разобрать строку в EVM-адрес; неверный формат → `InvalidAddress`.
    fn parse_addr(&self, address: &str) -> Result<Address, ChainError> {
        Address::from_str(address).map_err(|_| ChainError::InvalidAddress(Chain::Ethereum))
    }
}

#[async_trait]
impl BlockchainClient for EvmClient {
    fn chain(&self) -> Chain {
        Chain::Ethereum
    }

    fn config(&self) -> &ChainConfig {
        &self.config
    }

    fn validate_address(&self, address: &str) -> Result<(), ChainError> {
        self.parse_addr(address).map(|_| ())
    }

    async fn get_balance(&self, address: &str) -> Result<BalanceView, ChainError> {
        let addr = self.parse_addr(address)?;
        let total = from_alloy(
            self.provider
                .get_balance(addr)
                .await
                .map_err(|e| ChainError::Rpc(e.to_string()))?,
        );
        // У EVM нет неснижаемого резерва.
        Ok(BalanceView {
            total: Amount::new(Chain::Ethereum, total),
            reserved: Amount::new(Chain::Ethereum, U256::ZERO),
            spendable: Amount::new(Chain::Ethereum, total),
        })
    }

    async fn estimate_fee(&self, _req: &WithdrawRequest) -> Result<Amount, ChainError> {
        let fees = self
            .provider
            .estimate_eip1559_fees(None)
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        // Комиссия простого перевода: max_fee_per_gas × 21000.
        let fee = fees.max_fee_per_gas.saturating_mul(ETH_TRANSFER_GAS);
        Ok(Amount::new(Chain::Ethereum, U256::from(fee)))
    }

    async fn build_unsigned(
        &self,
        req: &WithdrawRequest,
        _fee: &Amount,
    ) -> Result<UnsignedTransaction, ChainError> {
        let from = self.parse_addr(&req.from_address)?;
        let to = self.parse_addr(&req.to_address)?;

        let chain_id = self
            .provider
            .get_chain_id()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        let nonce = self
            .provider
            .get_transaction_count(from)
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        let fees = self
            .provider
            .estimate_eip1559_fees(None)
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;

        let tx = TxEip1559 {
            chain_id,
            nonce,
            gas_limit: ETH_TRANSFER_GAS as u64,
            max_fee_per_gas: fees.max_fee_per_gas,
            max_priority_fee_per_gas: fees.max_priority_fee_per_gas,
            to: TxKind::Call(to),
            value: to_alloy(req.amount.raw),
            access_list: Default::default(),
            input: Bytes::new(),
        };
        let sighash: B256 = tx.signature_hash();
        let context =
            serde_json::to_vec(&tx).map_err(|e| ChainError::Rpc(format!("serialize tx: {e}")))?;
        // Account-модель: ровно один запрос на подпись (32-байтовый prehash).
        Ok(UnsignedTransaction {
            chain: Chain::Ethereum,
            requests: vec![SigningRequest {
                scheme: SignScheme::EcdsaPrehash,
                derivation_path: req.derivation_path.clone(),
                payload: sighash.0.to_vec(),
            }],
            context,
            // Сохраняем nonce: по нему реконсилятор отличит «заменена» от «ещё не дошла».
            tracking: Some(nonce.to_string()),
        })
    }

    fn assemble_signed(
        &self,
        unsigned: &UnsignedTransaction,
        signatures: &[Vec<u8>],
    ) -> Result<SignedTransaction, ChainError> {
        let signature = signatures
            .first()
            .ok_or_else(|| ChainError::Rpc("missing signature".into()))?;
        if signature.len() != 65 {
            return Err(ChainError::Rpc(
                "signature must be 65 bytes (r||s||v)".into(),
            ));
        }
        let tx: TxEip1559 = serde_json::from_slice(&unsigned.context)
            .map_err(|e| ChainError::Rpc(format!("deserialize tx: {e}")))?;

        let r = AlloyU256::from_be_slice(&signature[0..32]);
        let s = AlloyU256::from_be_slice(&signature[32..64]);
        let y_parity = signature[64] == 1;
        let sig = PrimitiveSignature::new(r, s, y_parity);

        let signed = tx.into_signed(sig);
        let envelope = TxEnvelope::from(signed);
        let raw = envelope.encoded_2718();
        Ok(SignedTransaction {
            chain: Chain::Ethereum,
            raw,
        })
    }

    fn txid(&self, signed: &SignedTransaction) -> Result<String, ChainError> {
        // Хэш EVM-транзакции = keccak256 от 2718-кодированных байт (тип || RLP).
        Ok(format!("{:#x}", keccak256(&signed.raw)))
    }

    async fn broadcast(&self, signed: &SignedTransaction) -> Result<String, ChainError> {
        let pending = self
            .provider
            .send_raw_transaction(&signed.raw)
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        Ok(format!("{:#x}", pending.tx_hash()))
    }

    async fn tx_status(
        &self,
        tx_hash: &str,
        from_address: &str,
        tracking: Option<&str>,
    ) -> Result<TxObservation, ChainError> {
        let hash = B256::from_str(tx_hash).map_err(|_| ChainError::Rpc("bad tx hash".into()))?;
        let receipt = self
            .provider
            .get_transaction_receipt(hash)
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        if let Some(receipt) = receipt {
            // Включена в блок. status == false → транзакция исполнилась с ошибкой (revert).
            if !receipt.status() {
                return Ok(TxObservation::Failed);
            }
            let Some(block) = receipt.block_number else {
                return Ok(TxObservation::Pending { confirmations: 0 });
            };
            let tip = self
                .provider
                .get_block_number()
                .await
                .map_err(|e| ChainError::Rpc(e.to_string()))?;
            return Ok(TxObservation::Pending {
                confirmations: tip.saturating_sub(block) + 1,
            });
        }
        // Квитанции нет: либо ещё в мемпуле, либо узел о транзакции не знает.
        if self
            .provider
            .get_transaction_by_hash(hash)
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .is_some()
        {
            return Ok(TxObservation::Pending { confirmations: 0 });
        }
        // Не в мемпуле и не в блоке. Если nonce известен и аккаунт уже промотал его дальше
        // (mined-nonce > наш) — этот nonce занят другой, подтверждённой транзакцией → замена.
        if let (Ok(from), Some(nonce)) = (
            self.parse_addr(from_address),
            tracking.and_then(|t| t.parse::<u64>().ok()),
        ) {
            let mined = self
                .provider
                .get_transaction_count(from)
                .await
                .map_err(|e| ChainError::Rpc(e.to_string()))?;
            if mined > nonce {
                return Ok(TxObservation::Replaced);
            }
        }
        // nonce ещё не пройден (или неизвестен) — транзакция просто пока не дошла.
        Ok(TxObservation::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ChainConfig {
        ChainConfig {
            chain: Chain::Ethereum,
            decimals: 18,
            confirmations: Some(3),
            reorg_window: 6,
            dust_limit: U256::from(1u64),
            fee_cap_factor: 2.0,
        }
    }

    #[test]
    fn u256_roundtrip() {
        let v = U256::from(123456789u64);
        assert_eq!(from_alloy(to_alloy(v)), v);
    }

    #[test]
    fn validate_address_format() {
        let c = EvmClient::new("http://localhost:8545", cfg()).unwrap();
        assert!(c
            .validate_address("0x000000000000000000000000000000000000dEaD")
            .is_ok());
        assert!(c.validate_address("not-an-address").is_err());
        assert!(c.validate_address("0xzz").is_err());
    }

    #[test]
    fn assemble_rejects_bad_signature_len() {
        let c = EvmClient::new("http://localhost:8545", cfg()).unwrap();
        let unsigned = UnsignedTransaction {
            chain: Chain::Ethereum,
            requests: vec![],
            context: vec![],
            tracking: None,
        };
        assert!(c.assemble_signed(&unsigned, &[vec![0u8; 10]]).is_err());
    }
}
