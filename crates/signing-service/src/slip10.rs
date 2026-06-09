//! SLIP-0010 деривация для ed25519. Обычный BIP32 к ed25519 неприменим (нет сложения
//! точек кривой, как у secp256k1), поэтому для Solana берём SLIP-0010 — а он умеет только
//! hardened-деривацию, что здесь и реализовано.

use hmac::{Hmac, Mac};
use sha2::Sha512;
use zeroize::Zeroizing;

use crate::hd::HdError;

type HmacSha512 = Hmac<Sha512>;

/// Разрезать 64-байтовый выход HMAC-SHA512 на (ключ, chain code).
fn split(i: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut key = [0u8; 32];
    let mut chain = [0u8; 32];
    key.copy_from_slice(&i[..32]);
    chain.copy_from_slice(&i[32..]);
    (key, chain)
}

/// Мастер-ключ: HMAC-SHA512 с ключом "ed25519 seed" по seed → (ключ, chain code).
fn master(seed: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut mac = HmacSha512::new_from_slice(b"ed25519 seed").expect("hmac key");
    mac.update(seed);
    split(&mac.finalize().into_bytes())
}

/// Один шаг hardened-деривации: из родительского (ключ, chain) и индекса — дочерние.
fn ckd_priv(key: &[u8; 32], chain: &[u8; 32], hardened_index: u32) -> ([u8; 32], [u8; 32]) {
    let index = hardened_index | 0x8000_0000;
    let mut mac = HmacSha512::new_from_slice(chain).expect("hmac key");
    mac.update(&[0u8]); // ed25519: data = 0x00 || key || ser32(i')
    mac.update(key);
    mac.update(&index.to_be_bytes());
    split(&mac.finalize().into_bytes())
}

/// Вывести 32-байтовый ed25519-секрет по SLIP-0010 пути (все сегменты hardened).
pub fn derive_ed25519(seed: &[u8], path: &str) -> Result<Zeroizing<[u8; 32]>, HdError> {
    let (mut key, mut chain) = master(seed);
    for seg in path.split('/') {
        if seg == "m" || seg.is_empty() {
            continue;
        }
        let num = seg
            .trim_end_matches('\'')
            .parse::<u32>()
            .map_err(|_| HdError::Path)?;
        let (k, c) = ckd_priv(&key, &chain, num);
        key = k;
        chain = c;
    }
    Ok(Zeroizing::new(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Официальный тест-вектор SLIP-0010 (ed25519), seed 000102...0f.
    fn seed() -> Vec<u8> {
        hex::decode("000102030405060708090a0b0c0d0e0f").unwrap()
    }

    #[test]
    fn master_matches_spec_vector() {
        let (key, _chain) = master(&seed());
        assert_eq!(
            hex::encode(key),
            "2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7"
        );
    }

    #[test]
    fn child_m0h_matches_spec_vector() {
        let key = derive_ed25519(&seed(), "m/0'").unwrap();
        assert_eq!(
            hex::encode(&key[..]),
            "68e0fe46dfb67e368c75379acec591dad19df3cde26e63b93a8e704f1dade7a3"
        );
    }

    #[test]
    fn pubkey_m0h_matches_spec_vector() {
        use ed25519_dalek::SigningKey;
        let key = derive_ed25519(&seed(), "m/0'").unwrap();
        let sk = SigningKey::from_bytes(&key);
        // Спека приводит публичный ключ с префиксом 0x00.
        assert_eq!(
            hex::encode(sk.verifying_key().to_bytes()),
            "8c8a13df77a28f3445213a0f432fde644acaa215fc72dcdf300d5efaa85d350c"
        );
    }
}
