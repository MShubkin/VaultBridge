//! Детерминированный тест-двойник `BlockchainClient`. Нужен, чтобы гонять сагу вывода
//! без выхода в сеть: вместо реального RPC — предсказуемые балансы, комиссии и tx-хэши.
//! В проде его место занимают настоящие адаптеры (EVM/BTC/Solana), реализующие тот же трейт.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;

use core_domain::{Amount, Chain, U256};

use crate::{
    BalanceView, BlockchainClient, ChainConfig, ChainError, SignScheme, SignedTransaction,
    SigningRequest, TxObservation, UnsignedTransaction, WithdrawRequest,
};

/// Мок сети с состоянием в памяти под мьютексами — можно дёргать из нескольких задач.
pub struct MockChain {
    /// Конфиг «сети», который мок отдаёт наружу.
    config: ChainConfig,
    /// Балансы адресов, выставляемые тестом через `set_balance`.
    balances: Mutex<HashMap<String, U256>>,
    /// Псевдо-nonce на адрес: растёт при каждой сборке, чтобы payload'ы не совпадали.
    nonces: Mutex<HashMap<String, u64>>,
    /// Уже отправленные транзакции: хэш raw → tx-hash. Ключ к проверке идемпотентности.
    broadcasts: Mutex<HashMap<String, String>>,
    /// Наблюдаемое состояние по tx-hash — тест выставляет его вручную, имитируя сеть.
    observations: Mutex<HashMap<String, TxObservation>>,
    /// Фиксированная комиссия, которую возвращает `estimate_fee`.
    fixed_fee: U256,
    /// Неснижаемый резерв (для проверки логики spendable).
    reserved: U256,
    /// Сколько запросов на подпись эмитировать: 1 — account-модель, N — имитация UTXO.
    num_inputs: usize,
}

impl MockChain {
    /// EVM-подобный мок: 18 decimals, dust 1, фикс. комиссия, нулевой резерв, 1 вход.
    pub fn ethereum() -> Self {
        Self {
            config: ChainConfig {
                chain: Chain::Ethereum,
                decimals: 18,
                confirmations: Some(3),
                reorg_window: 6,
                dust_limit: U256::from(1u64),
                fee_cap_factor: 2.0,
            },
            balances: Mutex::new(HashMap::new()),
            nonces: Mutex::new(HashMap::new()),
            broadcasts: Mutex::new(HashMap::new()),
            observations: Mutex::new(HashMap::new()),
            fixed_fee: U256::from(21_000u64),
            reserved: U256::ZERO,
            num_inputs: 1,
        }
    }

    /// Задать число подтверждений для tx-hash (имитация прогресса сети для реконсилятора).
    pub fn set_confirmations(&self, tx_hash: &str, n: u64) {
        self.set_observation(tx_hash, TxObservation::Pending { confirmations: n });
    }

    /// Задать произвольное наблюдаемое состояние tx-hash (для проверки Failed/Expired/Replaced/реорга).
    pub fn set_observation(&self, tx_hash: &str, obs: TxObservation) {
        self.observations
            .lock()
            .unwrap()
            .insert(tx_hash.to_string(), obs);
    }

    /// Имитация UTXO: заставить мок отдавать `n` запросов на подпись (по одному на вход).
    pub fn with_inputs(mut self, n: usize) -> Self {
        self.num_inputs = n.max(1);
        self
    }

    /// Выставить баланс адреса (для тестов).
    pub fn set_balance(&self, address: &str, total: U256) {
        self.balances
            .lock()
            .unwrap()
            .insert(address.to_string(), total);
    }

    /// Задать фиксированную комиссию (для тестов).
    pub fn set_fee(&mut self, fee: U256) {
        self.fixed_fee = fee;
    }

    /// Сколько раз транзакция реально ушла в сеть (для проверки отсутствия задвоения).
    pub fn broadcast_count(&self) -> usize {
        self.broadcasts.lock().unwrap().len()
    }
}

fn hash_hex(parts: &[&[u8]]) -> String {
    let mut h = DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

#[async_trait::async_trait]
impl BlockchainClient for MockChain {
    fn chain(&self) -> Chain {
        self.config.chain
    }

    fn config(&self) -> &ChainConfig {
        &self.config
    }

    fn validate_address(&self, address: &str) -> Result<(), ChainError> {
        let ok = address.len() == 42
            && address.starts_with("0x")
            && address[2..].chars().all(|c| c.is_ascii_hexdigit());
        if ok {
            Ok(())
        } else {
            Err(ChainError::InvalidAddress(self.config.chain))
        }
    }

    async fn get_balance(&self, address: &str) -> Result<BalanceView, ChainError> {
        let total = self
            .balances
            .lock()
            .unwrap()
            .get(address)
            .copied()
            .unwrap_or(U256::ZERO);
        let spendable = total.saturating_sub(self.reserved);
        Ok(BalanceView {
            total: Amount::new(self.config.chain, total),
            reserved: Amount::new(self.config.chain, self.reserved),
            spendable: Amount::new(self.config.chain, spendable),
        })
    }

    async fn estimate_fee(&self, _req: &WithdrawRequest) -> Result<Amount, ChainError> {
        Ok(Amount::new(self.config.chain, self.fixed_fee))
    }

    async fn build_unsigned(
        &self,
        req: &WithdrawRequest,
        fee: &Amount,
    ) -> Result<UnsignedTransaction, ChainError> {
        if req.amount.raw < self.config.dust_limit {
            return Err(ChainError::BelowDust);
        }
        // В реальном адаптере nonce резервируется под локом саги; здесь просто монотонно растёт.
        let nonce = {
            let mut nonces = self.nonces.lock().unwrap();
            let n = nonces.entry(req.from_address.clone()).or_insert(0);
            let cur = *n;
            *n += 1;
            cur
        };
        let context = format!(
            "{}|{}|{}|{}|{}",
            req.from_address, req.to_address, req.amount.raw, fee.raw, nonce
        )
        .into_bytes();
        // По запросу на каждый «вход» (для num_inputs>1 — разные payload, как у UTXO).
        let requests = (0..self.num_inputs)
            .map(|i| {
                let sighash_hex = hash_hex(&[&context, &[i as u8]]);
                let mut payload = vec![0u8; 32];
                payload[..16].copy_from_slice(&hex_to_16(&sighash_hex));
                SigningRequest {
                    scheme: SignScheme::EcdsaPrehash,
                    derivation_path: req.derivation_path.clone(),
                    payload,
                }
            })
            .collect();
        Ok(UnsignedTransaction {
            chain: self.config.chain,
            requests,
            context,
            tracking: None,
        })
    }

    fn assemble_signed(
        &self,
        unsigned: &UnsignedTransaction,
        signatures: &[Vec<u8>],
    ) -> Result<SignedTransaction, ChainError> {
        if signatures.len() != unsigned.requests.len() {
            return Err(ChainError::Rpc(format!(
                "expected {} signatures, got {}",
                unsigned.requests.len(),
                signatures.len()
            )));
        }
        let mut raw = unsigned.context.clone();
        for sig in signatures {
            raw.extend_from_slice(sig);
        }
        Ok(SignedTransaction {
            chain: self.config.chain,
            raw,
        })
    }

    fn txid(&self, signed: &SignedTransaction) -> Result<String, ChainError> {
        // Тот же id, что вернёт broadcast — детерминирован от raw.
        Ok(format!("0xmocktx{}", hash_hex(&[&signed.raw])))
    }

    async fn broadcast(&self, signed: &SignedTransaction) -> Result<String, ChainError> {
        let raw_hash = hash_hex(&[&signed.raw]);
        let mut broadcasts = self.broadcasts.lock().unwrap();
        // Идемпотентность: повторная отправка того же raw → тот же tx-hash, без дубля.
        let tx_hash = broadcasts
            .entry(raw_hash.clone())
            .or_insert_with(|| format!("0xmocktx{raw_hash}"))
            .clone();
        Ok(tx_hash)
    }

    async fn tx_status(
        &self,
        tx_hash: &str,
        _from_address: &str,
        _tracking: Option<&str>,
    ) -> Result<TxObservation, ChainError> {
        Ok(self
            .observations
            .lock()
            .unwrap()
            .get(tx_hash)
            .copied()
            .unwrap_or(TxObservation::NotFound))
    }
}

fn hex_to_16(s: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let bytes = s.as_bytes();
    for (i, chunk) in bytes.chunks(2).take(8).enumerate() {
        let hi = (chunk[0] as char).to_digit(16).unwrap_or(0) as u8;
        let lo = chunk
            .get(1)
            .map(|c| (*c as char).to_digit(16).unwrap_or(0) as u8)
            .unwrap_or(0);
        out[i] = (hi << 4) | lo;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(to: &str, amount: u64) -> WithdrawRequest {
        WithdrawRequest {
            chain: Chain::Ethereum,
            from_address: "0x1111111111111111111111111111111111111111".into(),
            to_address: to.into(),
            amount: Amount::new(Chain::Ethereum, U256::from(amount)),
            derivation_path: "m/44'/60'/0'/0/0".into(),
        }
    }

    #[tokio::test]
    async fn balance_reflects_set_value() {
        let mc = MockChain::ethereum();
        let addr = "0x2222222222222222222222222222222222222222";
        mc.set_balance(addr, U256::from(1000u64));
        let b = mc.get_balance(addr).await.unwrap();
        assert_eq!(b.total.raw, U256::from(1000u64));
        assert_eq!(b.spendable.raw, U256::from(1000u64));
    }

    #[test]
    fn validate_address_rejects_garbage() {
        let mc = MockChain::ethereum();
        assert!(mc
            .validate_address("0x2222222222222222222222222222222222222222")
            .is_ok());
        assert!(mc.validate_address("not-an-address").is_err());
        assert!(mc.validate_address("0xzz").is_err());
    }

    #[tokio::test]
    async fn build_rejects_below_dust() {
        let mc = MockChain::ethereum();
        let fee = Amount::new(Chain::Ethereum, U256::from(1u64));
        let r = WithdrawRequest {
            amount: Amount::new(Chain::Ethereum, U256::ZERO),
            ..req("0x3333333333333333333333333333333333333333", 0)
        };
        assert!(matches!(
            mc.build_unsigned(&r, &fee).await,
            Err(ChainError::BelowDust)
        ));
    }

    #[tokio::test]
    async fn broadcast_is_idempotent_for_same_raw() {
        let mc = MockChain::ethereum();
        let signed = SignedTransaction {
            chain: Chain::Ethereum,
            raw: b"same-raw-bytes".to_vec(),
        };
        let h1 = mc.broadcast(&signed).await.unwrap();
        let h2 = mc.broadcast(&signed).await.unwrap();
        assert_eq!(h1, h2);
        assert_eq!(mc.broadcast_count(), 1);
    }

    #[tokio::test]
    async fn txid_matches_broadcast_hash() {
        // Инвариант, на котором держится фикс «Broadcast без tx_hash»: предвычисленный id
        // совпадает с тем, что вернёт сеть. Иначе сага запишет один hash, а сверять будем другой.
        let mc = MockChain::ethereum();
        let signed = SignedTransaction {
            chain: Chain::Ethereum,
            raw: b"deterministic-raw".to_vec(),
        };
        assert_eq!(
            mc.txid(&signed).unwrap(),
            mc.broadcast(&signed).await.unwrap()
        );
    }

    #[tokio::test]
    async fn nonce_increments_per_build() {
        let mc = MockChain::ethereum();
        let fee = Amount::new(Chain::Ethereum, U256::from(1u64));
        let u1 = mc
            .build_unsigned(&req("0x3333333333333333333333333333333333333333", 10), &fee)
            .await
            .unwrap();
        let u2 = mc
            .build_unsigned(&req("0x3333333333333333333333333333333333333333", 10), &fee)
            .await
            .unwrap();
        // Разный nonce → разный payload запроса, иначе две выплаты совпали бы.
        assert_ne!(u1.requests[0].payload, u2.requests[0].payload);
    }

    #[tokio::test]
    async fn multi_input_emits_one_request_per_input() {
        let mc = MockChain::ethereum().with_inputs(3);
        let fee = Amount::new(Chain::Ethereum, U256::from(1u64));
        let u = mc
            .build_unsigned(&req("0x3333333333333333333333333333333333333333", 10), &fee)
            .await
            .unwrap();
        assert_eq!(u.requests.len(), 3);
        // assemble требует ровно столько подписей, сколько запросов.
        let sigs = vec![vec![1u8; 65], vec![2u8; 65]];
        assert!(mc.assemble_signed(&u, &sigs).is_err()); // 2 != 3
        let sigs3 = vec![vec![1u8; 65], vec![2u8; 65], vec![3u8; 65]];
        assert!(mc.assemble_signed(&u, &sigs3).is_ok());
    }
}
