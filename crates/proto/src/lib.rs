//! gRPC-контракт между gateway и signing-service плюс общие строители mTLS.
//!
//! Сгенерированный `tonic`/`prost`-код (сервис `Signer { DeriveAddress, Sign }`) и
//! функции сборки TLS-конфигов лежат в одном крейте сознательно: сервер, клиент и
//! интеграционный тест должны пользоваться одним и тем же security-критичным кодом,
//! а не тремя слегка разошедшимися копиями.

/// Сгенерированные типы и сервис (`vaultbridge.signer.v1`).
pub mod signer {
    tonic::include_proto!("vaultbridge.signer.v1");
}

pub use signer::signer_client::SignerClient;
pub use signer::signer_server::{Signer as SignerRpc, SignerServer};
pub use signer::{DeriveAddressRequest, DeriveAddressResponse, SignRequest, SignResponse};

/// Строители взаимного TLS (mTLS) для канала gateway↔signing.
///
/// Сервер предъявляет свой сертификат и **требует** валидный клиентский (подписанный
/// нашим CA) — никто без клиентского ключа не достучится до подписи. Клиент предъявляет
/// свой сертификат и проверяет сервер по тому же CA. Обе стороны — взаимная аутентификация.
pub mod tls {
    use tonic::transport::{Certificate, ClientTlsConfig, Identity, ServerTlsConfig};

    /// Конфиг TLS для сервера signing-service: identity = серверный серт/ключ,
    /// `client_ca_root` = CA, по которому проверяются клиентские сертификаты.
    pub fn server_config(
        server_cert_pem: &[u8],
        server_key_pem: &[u8],
        client_ca_pem: &[u8],
    ) -> ServerTlsConfig {
        ServerTlsConfig::new()
            .identity(Identity::from_pem(server_cert_pem, server_key_pem))
            .client_ca_root(Certificate::from_pem(client_ca_pem))
    }

    /// Конфиг TLS для клиента gateway: identity = клиентский серт/ключ, `ca_certificate`
    /// = CA сервера, `domain_name` = ожидаемый SAN сервера.
    pub fn client_config(
        client_cert_pem: &[u8],
        client_key_pem: &[u8],
        server_ca_pem: &[u8],
        domain: impl Into<String>,
    ) -> ClientTlsConfig {
        ClientTlsConfig::new()
            .identity(Identity::from_pem(client_cert_pem, client_key_pem))
            .ca_certificate(Certificate::from_pem(server_ca_pem))
            .domain_name(domain)
    }
}
