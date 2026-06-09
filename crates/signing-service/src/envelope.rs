//! Envelope encryption: секрет шифруется случайным одноразовым ключом DEK (AES-256-GCM),
//! а сам DEK заворачивается под мастер-ключом KEK. Смысл схемы — KEK живёт вне БД, поэтому
//! утёкшая база без него бесполезна. Всё расшифрованное держим в `Zeroizing` и затираем.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::{rngs::OsRng, RngCore};
use zeroize::Zeroizing;

/// Ошибки шифрования/расшифровки.
#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    /// Сбой AEAD: не та подпись/тег при расшифровке либо ошибка шифрования.
    #[error("aead failure")]
    Aead,
    /// Ключ не той длины (ожидается 32 байта).
    #[error("bad key length")]
    KeyLength,
}

/// DEK, завёрнутый под KEK: то, что безопасно хранить рядом с шифртекстом.
#[derive(Clone, Debug)]
pub struct WrappedDek {
    /// Nonce, под которым DEK шифровался KEK'ом.
    pub nonce: [u8; 12],
    /// Зашифрованный DEK.
    pub ciphertext: Vec<u8>,
}

/// Запечатанный секрет целиком: шифртекст данных под DEK плюс сам DEK в завёрнутом виде.
/// Ровно это и кладётся в хранилище ключевого материала.
#[derive(Clone, Debug)]
pub struct Sealed {
    /// Nonce шифрования данных под DEK.
    pub nonce: [u8; 12],
    /// Зашифрованные данные (секрет).
    pub ciphertext: Vec<u8>,
    /// Завёрнутый под KEK ключ DEK.
    pub wrapped_dek: WrappedDek,
}

/// Сгенерировать N случайных байт из системного CSPRNG.
fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    OsRng.fill_bytes(&mut b);
    b
}

/// Запечатать `plaintext`: новый DEK шифрует данные, KEK заворачивает DEK.
pub fn seal(kek: &[u8; 32], plaintext: &[u8]) -> Result<Sealed, EnvelopeError> {
    let dek = Zeroizing::new(random_bytes::<32>());

    let data_cipher = Aes256Gcm::new_from_slice(&dek[..]).map_err(|_| EnvelopeError::KeyLength)?;
    let nonce = random_bytes::<12>();
    let ciphertext = data_cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| EnvelopeError::Aead)?;

    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| EnvelopeError::KeyLength)?;
    let dek_nonce = random_bytes::<12>();
    let wrapped = kek_cipher
        .encrypt(Nonce::from_slice(&dek_nonce), &dek[..])
        .map_err(|_| EnvelopeError::Aead)?;

    Ok(Sealed {
        nonce,
        ciphertext,
        wrapped_dek: WrappedDek {
            nonce: dek_nonce,
            ciphertext: wrapped,
        },
    })
}

/// Распечатать: развернуть DEK под KEK, расшифровать данные. Результат затирается в `Drop`.
pub fn open(kek: &[u8; 32], sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| EnvelopeError::KeyLength)?;
    let dek = Zeroizing::new(
        kek_cipher
            .decrypt(
                Nonce::from_slice(&sealed.wrapped_dek.nonce),
                sealed.wrapped_dek.ciphertext.as_ref(),
            )
            .map_err(|_| EnvelopeError::Aead)?,
    );

    let data_cipher = Aes256Gcm::new_from_slice(&dek[..]).map_err(|_| EnvelopeError::KeyLength)?;
    let plaintext = data_cipher
        .decrypt(Nonce::from_slice(&sealed.nonce), sealed.ciphertext.as_ref())
        .map_err(|_| EnvelopeError::Aead)?;
    Ok(Zeroizing::new(plaintext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let kek = [7u8; 32];
        let secret = b"super-secret-seed-material";
        let sealed = seal(&kek, secret).unwrap();
        let opened = open(&kek, &sealed).unwrap();
        assert_eq!(&opened[..], secret);
    }

    #[test]
    fn wrong_kek_fails() {
        let sealed = seal(&[1u8; 32], b"data").unwrap();
        assert!(open(&[2u8; 32], &sealed).is_err());
    }

    #[test]
    fn ciphertext_differs_from_plaintext() {
        let sealed = seal(&[3u8; 32], b"plaintext").unwrap();
        assert_ne!(sealed.ciphertext, b"plaintext");
    }
}
