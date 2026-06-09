//! HD-деривация ключей: мнемоника → seed → BIP32 secp256k1-ключ по пути деривации.
//! Здесь же — получение адресов из публичных ключей (EVM keccak256[12..], Bitcoin P2PKH,
//! Solana base58) и сами подписи: recoverable ECDSA (r‖s‖v) для secp256k1 и ed25519.

use bip32::{DerivationPath, XPrv};
use bip39::Mnemonic;
use ed25519_dalek::{Signer as _, SigningKey as EdSigningKey};
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use ripemd::Ripemd160;
use sha2::Sha256;
use sha3::{Digest, Keccak256}; // единый трейт digest::Digest для всех хеш-функций
use zeroize::Zeroizing;

/// Что может пойти не так при деривации и подписи.
#[derive(Debug, thiserror::Error)]
pub enum HdError {
    /// Мнемоника не парсится (неверные слова или контрольная сумма).
    #[error("invalid mnemonic")]
    Mnemonic,
    /// Путь деривации синтаксически некорректен.
    #[error("invalid derivation path")]
    Path,
    /// Вывод ключа по пути не удался.
    #[error("derivation failed")]
    Derive,
    /// На подпись ECDSA пришёл payload не из 32 байт (это должен быть готовый хэш).
    #[error("signing payload must be a 32-byte prehash")]
    PayloadLength,
    /// Сама операция подписи завершилась ошибкой.
    #[error("signing failed")]
    Sign,
}

/// Мнемоника + passphrase → 64-байтовый seed (затирается в `Drop`).
pub fn seed_from_mnemonic(phrase: &str, passphrase: &str) -> Result<Zeroizing<[u8; 64]>, HdError> {
    let mnemonic = Mnemonic::parse(phrase).map_err(|_| HdError::Mnemonic)?;
    Ok(Zeroizing::new(mnemonic.to_seed(passphrase)))
}

/// Вывести secp256k1-ключ по BIP32-пути из seed.
pub fn derive_secp256k1(seed: &[u8], path: &str) -> Result<SigningKey, HdError> {
    let dp: DerivationPath = path.parse().map_err(|_| HdError::Path)?;
    let xprv = XPrv::derive_from_path(seed, &dp).map_err(|_| HdError::Derive)?;
    Ok(xprv.private_key().clone())
}

/// EVM-адрес из публичного ключа: `0x` + последние 20 байт keccak256(uncompressed[1..]).
pub fn evm_address(vk: &VerifyingKey) -> String {
    let point = vk.to_encoded_point(false);
    let bytes = point.as_bytes(); // 0x04 || X(32) || Y(32)
    let hash = Keccak256::digest(&bytes[1..]);
    format!("0x{}", hex::encode(&hash[12..]))
}

/// Recoverable ECDSA-подпись над 32-байтовым хешем → 65 байт (r||s||v).
pub fn sign_prehash(key: &SigningKey, prehash: &[u8]) -> Result<[u8; 65], HdError> {
    if prehash.len() != 32 {
        return Err(HdError::PayloadLength);
    }
    let (sig, recid): (Signature, RecoveryId) = key
        .sign_prehash_recoverable(prehash)
        .map_err(|_| HdError::Sign)?;
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = recid.to_byte();
    Ok(out)
}

/// RIPEMD160(SHA256(data)) — стандартный hash160 (Bitcoin).
pub(crate) fn hash160(data: &[u8]) -> [u8; 20] {
    let sha = Sha256::digest(data);
    let rip = Ripemd160::digest(sha);
    let mut out = [0u8; 20];
    out.copy_from_slice(&rip);
    out
}

/// Bitcoin testnet P2PKH-адрес: base58check(0x6F || hash160(compressed pubkey)).
pub fn btc_p2pkh_testnet(vk: &VerifyingKey) -> String {
    let compressed = vk.to_encoded_point(true);
    let h = hash160(compressed.as_bytes());
    let mut payload = Vec::with_capacity(21);
    payload.push(0x6f); // testnet pubkey-hash version
    payload.extend_from_slice(&h);
    bs58::encode(payload).with_check().into_string()
}

/// Solana-адрес: base58(ed25519 pubkey).
pub fn solana_address(pubkey: &[u8; 32]) -> String {
    bs58::encode(pubkey).into_string()
}

/// Публичный ed25519-ключ из 32-байтового секрета (секрет приходит из SLIP-0010-деривации).
pub fn ed25519_pubkey(secret: &[u8; 32]) -> [u8; 32] {
    EdSigningKey::from_bytes(secret).verifying_key().to_bytes()
}

/// Подпись ed25519 над произвольным сообщением (Solana подписывает всё сообщение) → 64 байта.
pub fn sign_ed25519(secret: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    EdSigningKey::from_bytes(secret).sign(msg).to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Канонический BIP39 тест-вектор (MetaMask): первый EVM-аккаунт.
    const TEST_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn derives_known_evm_address() {
        let seed = seed_from_mnemonic(TEST_MNEMONIC, "").unwrap();
        let key = derive_secp256k1(&seed[..], "m/44'/60'/0'/0/0").unwrap();
        let addr = evm_address(key.verifying_key());
        assert_eq!(addr, "0x9858effd232b4033e47d90003d41ec34ecaeda94");
    }

    #[test]
    fn different_paths_give_different_addresses() {
        let seed = seed_from_mnemonic(TEST_MNEMONIC, "").unwrap();
        let a = evm_address(
            derive_secp256k1(&seed[..], "m/44'/60'/0'/0/0")
                .unwrap()
                .verifying_key(),
        );
        let b = evm_address(
            derive_secp256k1(&seed[..], "m/44'/60'/0'/0/1")
                .unwrap()
                .verifying_key(),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn sign_and_verify_prehash() {
        use k256::ecdsa::signature::hazmat::PrehashVerifier;
        let seed = seed_from_mnemonic(TEST_MNEMONIC, "").unwrap();
        let key = derive_secp256k1(&seed[..], "m/44'/60'/0'/0/0").unwrap();
        let hash = [0x11u8; 32];
        let sig_bytes = sign_prehash(&key, &hash).unwrap();
        let sig = Signature::from_slice(&sig_bytes[..64]).unwrap();
        assert!(key.verifying_key().verify_prehash(&hash, &sig).is_ok());
    }

    #[test]
    fn rejects_non_32_byte_payload() {
        let seed = seed_from_mnemonic(TEST_MNEMONIC, "").unwrap();
        let key = derive_secp256k1(&seed[..], "m/44'/60'/0'/0/0").unwrap();
        assert!(matches!(
            sign_prehash(&key, &[0u8; 20]),
            Err(HdError::PayloadLength)
        ));
    }

    #[test]
    fn btc_testnet_address_is_deterministic_and_well_formed() {
        let seed = seed_from_mnemonic(TEST_MNEMONIC, "").unwrap();
        let key = derive_secp256k1(&seed[..], "m/44'/1'/0'/0/0").unwrap();
        let a1 = btc_p2pkh_testnet(key.verifying_key());
        let a2 = btc_p2pkh_testnet(key.verifying_key());
        assert_eq!(a1, a2);
        // testnet P2PKH начинается с m/n; base58check декодируется в 0x6f || 20 байт.
        assert!(a1.starts_with('m') || a1.starts_with('n'));
        let decoded = bs58::decode(&a1).with_check(None).into_vec().unwrap();
        assert_eq!(decoded.len(), 21);
        assert_eq!(decoded[0], 0x6f);
    }

    #[test]
    fn ed25519_sign_verify() {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let secret = [7u8; 32];
        let msg = b"solana message";
        let sig_bytes = sign_ed25519(&secret, msg);
        let vk = VerifyingKey::from_bytes(&ed25519_pubkey(&secret)).unwrap();
        let sig = Signature::from_bytes(&sig_bytes);
        assert!(vk.verify(msg, &sig).is_ok());
    }
}
