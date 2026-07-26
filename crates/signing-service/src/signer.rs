//! Контракт подписи. Через границу ходят только путь деривации и payload — приватный
//! ключ остаётся внутри. `LocalSigner` держит seed и выводит ключи по запросу; та же
//! абстракция за gRPC-обёрткой даёт удалённый signing-service.

use async_trait::async_trait;
use core_domain::Chain;
use zeroize::Zeroizing;

use crate::envelope::{self, Sealed};
use crate::{hd, multisig, slip10};

#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    /// Сеть пока не поддерживается.
    #[error("chain not supported yet: {0}")]
    Unsupported(Chain),
    /// Ошибка HD-деривации (плохой путь, мнемоника и т.п.).
    #[error(transparent)]
    Hd(#[from] hd::HdError),
    /// Ошибка envelope-шифрования seed (запечатывание/распечатывание).
    #[error(transparent)]
    Envelope(#[from] envelope::EnvelopeError),
    /// Ошибка удалённого signing-service (gRPC/mTLS-транспорт), для `RemoteSigner`.
    #[error("remote signer: {0}")]
    Remote(String),
}

/// Абстракция подписывающего сервиса. Возвращает публичные данные/подписи, не ключи.
///
/// Метод async, потому что реализация может быть удалённой (`RemoteSigner` поверх
/// gRPC/mTLS); для `LocalSigner` это просто обёртка над синхронной криптографией.
#[async_trait]
pub trait Signer: Send + Sync {
    /// Адрес для пути деривации — нужен при создании кошелька.
    async fn derive_address(&self, chain: Chain, path: &str) -> Result<String, SignerError>;
    /// Подпись payload (для EVM — 32-байтовый prehash) → байты подписи.
    async fn sign(&self, chain: Chain, path: &str, payload: &[u8]) -> Result<Vec<u8>, SignerError>;
}

/// In-process реализация: единственный владелец seed. Seed в памяти под `Zeroizing`.
pub struct LocalSigner {
    seed: Zeroizing<[u8; 64]>,
}

impl LocalSigner {
    /// Собрать signer из BIP39-мнемоники и (опциональной) passphrase. Из них выводится
    /// 64-байтовый seed, дальше все ключи деривируются уже из него.
    pub fn from_mnemonic(phrase: &str, passphrase: &str) -> Result<Self, SignerError> {
        Ok(Self {
            seed: hd::seed_from_mnemonic(phrase, passphrase)?,
        })
    }

    /// Загрузить seed из запечатанного хранилища (envelope), развернув его под KEK.
    pub fn from_sealed(kek: &[u8; 32], sealed: &Sealed) -> Result<Self, SignerError> {
        let opened = envelope::open(kek, sealed)?;
        let mut seed = [0u8; 64];
        if opened.len() != 64 {
            return Err(SignerError::Envelope(envelope::EnvelopeError::KeyLength));
        }
        seed.copy_from_slice(&opened);
        Ok(Self {
            seed: Zeroizing::new(seed),
        })
    }

    /// Запечатать текущий seed под KEK (для записи в хранилище).
    pub fn seal_seed(&self, kek: &[u8; 32]) -> Result<Sealed, SignerError> {
        Ok(envelope::seal(kek, &self.seed[..])?)
    }

    /// Bitcoin 2-of-3 multisig P2SH testnet-адрес из трёх путей деривации (бонус).
    pub fn btc_multisig_2of3_address(&self, paths: [&str; 3]) -> Result<String, SignerError> {
        let mut keys = Vec::with_capacity(3);
        for p in paths {
            keys.push(*hd::derive_secp256k1(&self.seed[..], p)?.verifying_key());
        }
        let arr: [_; 3] = keys.try_into().expect("3 keys");
        let script = multisig::redeem_script_2of3(&arr);
        Ok(multisig::p2sh_testnet_address(&script))
    }
}

#[async_trait]
impl Signer for LocalSigner {
    async fn derive_address(&self, chain: Chain, path: &str) -> Result<String, SignerError> {
        match chain {
            Chain::Ethereum => {
                let key = hd::derive_secp256k1(&self.seed[..], path)?;
                Ok(hd::evm_address(key.verifying_key()))
            }
            Chain::Bitcoin => {
                let key = hd::derive_secp256k1(&self.seed[..], path)?;
                Ok(hd::btc_p2pkh_testnet(key.verifying_key()))
            }
            Chain::Solana => {
                let secret = slip10::derive_ed25519(&self.seed[..], path)?;
                Ok(hd::solana_address(&hd::ed25519_pubkey(&secret)))
            }
        }
    }

    async fn sign(&self, chain: Chain, path: &str, payload: &[u8]) -> Result<Vec<u8>, SignerError> {
        match chain {
            // EVM/BTC: ECDSA над 32-байтовым prehash (sighash).
            Chain::Ethereum | Chain::Bitcoin => {
                let key = hd::derive_secp256k1(&self.seed[..], path)?;
                Ok(hd::sign_prehash(&key, payload)?.to_vec())
            }
            // Solana: ed25519 над всем сообщением (не prehash).
            Chain::Solana => {
                let secret = slip10::derive_ed25519(&self.seed[..], path)?;
                Ok(hd::sign_ed25519(&secret, payload).to_vec())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[tokio::test]
    async fn derive_address_matches_hd_vector() {
        let signer = LocalSigner::from_mnemonic(M, "").unwrap();
        let addr = signer
            .derive_address(Chain::Ethereum, "m/44'/60'/0'/0/0")
            .await
            .unwrap();
        assert_eq!(addr, "0x9858effd232b4033e47d90003d41ec34ecaeda94");
    }

    #[tokio::test]
    async fn derives_addresses_for_all_chains() {
        let signer = LocalSigner::from_mnemonic(M, "").unwrap();
        let btc = signer
            .derive_address(Chain::Bitcoin, "m/44'/1'/0'/0/0")
            .await
            .unwrap();
        assert!(btc.starts_with('m') || btc.starts_with('n')); // testnet P2PKH
        let sol = signer
            .derive_address(Chain::Solana, "m/44'/501'/0'/0'")
            .await
            .unwrap();
        assert!(!sol.starts_with("0x") && sol.len() >= 32); // base58 pubkey
    }

    #[test]
    fn btc_multisig_2of3_address_is_testnet_p2sh() {
        let signer = LocalSigner::from_mnemonic(M, "").unwrap();
        let addr = signer
            .btc_multisig_2of3_address(["m/48'/1'/0'/0/0", "m/48'/1'/0'/0/1", "m/48'/1'/0'/0/2"])
            .unwrap();
        assert!(addr.starts_with('2')); // testnet P2SH
    }

    #[tokio::test]
    async fn solana_signs_arbitrary_message() {
        let signer = LocalSigner::from_mnemonic(M, "").unwrap();
        let sig = signer
            .sign(Chain::Solana, "m/44'/501'/0'/0'", b"not-a-32-byte-prehash")
            .await
            .unwrap();
        assert_eq!(sig.len(), 64); // ed25519
    }

    #[tokio::test]
    async fn seal_then_load_roundtrip_preserves_addresses() {
        let kek = [9u8; 32];
        let signer = LocalSigner::from_mnemonic(M, "").unwrap();
        let sealed = signer.seal_seed(&kek).unwrap();
        let restored = LocalSigner::from_sealed(&kek, &sealed).unwrap();
        let p = "m/44'/60'/0'/0/3";
        assert_eq!(
            signer.derive_address(Chain::Ethereum, p).await.unwrap(),
            restored.derive_address(Chain::Ethereum, p).await.unwrap()
        );
    }

    #[tokio::test]
    async fn signs_32_byte_prehash() {
        let signer = LocalSigner::from_mnemonic(M, "").unwrap();
        let sig = signer
            .sign(Chain::Ethereum, "m/44'/60'/0'/0/0", &[0x22u8; 32])
            .await
            .unwrap();
        assert_eq!(sig.len(), 65);
    }
}
