//! VaultBridge Signing Service — gRPC-сервер.
//!
//! Поднимает tonic-сервис `Signer { DeriveAddress, Sign }` поверх крипто-ядра
//! (`LocalSigner`). Приватный ключ не покидает процесс; gateway обращается за подписью
//! только по сети. При заданных TLS-сертификатах включается **взаимный TLS**: сервер
//! требует валидный клиентский сертификат, подписанный нашим CA.
//!
//! Переменные окружения:
//! - `SIGNER_MNEMONIC` — seed (в проде пришёл бы из KMS/secret, а не из env).
//! - `SIGNER_BIND` — адрес прослушивания (деф. `0.0.0.0:50051`).
//! - `SIGNER_TLS_CERT`/`SIGNER_TLS_KEY`/`SIGNER_TLS_CLIENT_CA` — PEM для mTLS; если заданы
//!   все три, транспорт шифруется и клиент аутентифицируется. Иначе — plaintext (dev).

use std::sync::Arc;

use signing_service::{LocalSigner, Signer, SignerService};
use tonic::transport::Server;

const DEV_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();

    let mnemonic = std::env::var("SIGNER_MNEMONIC").unwrap_or_else(|_| DEV_MNEMONIC.into());
    let signer: Arc<dyn Signer> = Arc::new(
        LocalSigner::from_mnemonic(&mnemonic, "")
            .map_err(|e| anyhow::anyhow!("signer init: {e}"))?,
    );
    // Smoke-деривация: убеждаемся, что seed валиден, до открытия сокета.
    let addr0 = signer
        .derive_address(core_domain::Chain::Ethereum, "m/44'/60'/0'/0/0")
        .await
        .map_err(|e| anyhow::anyhow!("derive: {e}"))?;
    tracing::info!(first_evm_address = %addr0, "signing-service crypto ready");

    let bind: std::net::SocketAddr = std::env::var("SIGNER_BIND")
        .unwrap_or_else(|_| "0.0.0.0:50051".into())
        .parse()
        .map_err(|e| anyhow::anyhow!("bad SIGNER_BIND: {e}"))?;

    let mut builder = Server::builder();

    // mTLS включается, только если заданы все три PEM-файла.
    match (
        std::env::var("SIGNER_TLS_CERT"),
        std::env::var("SIGNER_TLS_KEY"),
        std::env::var("SIGNER_TLS_CLIENT_CA"),
    ) {
        (Ok(cert), Ok(key), Ok(client_ca)) => {
            let cert = std::fs::read(&cert)?;
            let key = std::fs::read(&key)?;
            let client_ca = std::fs::read(&client_ca)?;
            let tls = proto::tls::server_config(&cert, &key, &client_ca);
            builder = builder
                .tls_config(tls)
                .map_err(|e| anyhow::anyhow!("tls config: {e}"))?;
            tracing::info!(%bind, "signing-service listening (mTLS, client cert required)");
        }
        _ => {
            tracing::warn!(%bind, "signing-service listening (PLAINTEXT — set SIGNER_TLS_* for mTLS)");
        }
    }

    let service = SignerService::new(signer).into_server();
    builder
        .add_service(service)
        .serve_with_shutdown(bind, shutdown_signal())
        .await?;
    Ok(())
}

/// Ждать Ctrl-C (SIGINT): по нему tonic перестаёт принимать вызовы и корректно останавливается.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("signing-service shutdown signal received");
}
