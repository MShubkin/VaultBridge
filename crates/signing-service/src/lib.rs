//! VaultBridge Signing Service — крипто-ядро сервиса подписи.
//!
//! Здесь собрано всё, что работает с ключами: HD-деривация (BIP39/BIP32 для secp256k1,
//! SLIP-0010 для ed25519), envelope encryption (DEK под KEK, AES-256-GCM, zeroize) и сами
//! подписи. Крейт даёт и библиотеку, и бинарь: тот же [`Signer`] вызывается либо in-process,
//! либо за gRPC-границей через [`SignerService`].

pub mod envelope;
pub mod grpc;
pub mod hd;
pub mod multisig;
pub mod signer;
pub mod slip10;

pub use grpc::SignerService;
pub use signer::{LocalSigner, Signer, SignerError};
