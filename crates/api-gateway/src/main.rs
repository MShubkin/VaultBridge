//! VaultBridge API Gateway — публичный `axum`-сервер и точка сборки приложения.
//!
//! `main` поднимает сервер, а `build_state` собирает `AppState` из боевых зависимостей:
//! Postgres (хранилище + advisory-локи), Redis (кеш + идемпотентность), ClickHouse
//! (аналитика), удалённый signing-service по gRPC+mTLS, HTTP-провайдеры KYC/AML и реальные
//! адаптеры сетей. Все обязательные адреса берутся из окружения — если чего-то нет, сервис
//! не стартует, а не тихо подменяет зависимость заглушкой.

mod auth;
mod balance;
mod error;
mod events;
mod graphql;
mod idempotency;
mod metrics;
mod remote_signer;
mod routes;
mod scanner;
mod state;

use std::collections::HashMap;
use std::sync::Arc;

use blockchain::BlockchainClient;
use core_domain::Chain;
use kyc_aml::{AmlScreener, HttpAmlScreener, HttpKyc, KycProvider};
use storage::{
    AuditRepository, TransactionRepository, UserRepository, WalletLock, WalletRepository,
};

use crate::auth::JwtKeys;
use crate::idempotency::Idempotency;
use crate::state::{AppState, Config};

/// Точка входа. Особый режим: `api-gateway openapi` печатает OpenAPI-спеку и выходит — это
/// используется в сборке фронта для кодогена типов, без поднятия сервера.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("openapi") {
        use utoipa::OpenApi;
        println!("{}", routes::ApiDoc::openapi().to_pretty_json()?);
        return Ok(());
    }

    init_tracing();

    let state = build_state().await?;

    // Фоновый реконсилятор подтверждений (block-scanner): опрашивает сеть и доводит
    // исходящие транзакции до Confirmed. Интервал — SCAN_INTERVAL_SECS (по умолчанию 15с).
    let scan_secs: u64 = std::env::var("SCAN_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    scanner::spawn(state.clone(), std::time::Duration::from_secs(scan_secs));
    tracing::info!(interval_secs = scan_secs, "scanner: started");

    let app = routes::router(state).layer(cors_layer());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));

    tracing::info!(%addr, "api-gateway listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // Graceful shutdown: на SIGINT/SIGTERM перестаём принимать новые соединения,
    // даём in-flight запросам (включая сагу вывода) завершиться.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Прочитать обязательную переменную окружения или упасть с понятным сообщением.
fn require_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| anyhow::anyhow!("{key} is required"))
}

/// Собрать `AppState` из боевых зависимостей. Каждый обязательный адрес читается из env, и
/// при его отсутствии сборка падает — сервис не поднимется с тихой заглушкой вместо БД или
/// signer. Bitcoin и Solana опциональны: их адаптеры включаются, только если задан их RPC.
async fn build_state() -> anyhow::Result<AppState> {
    let secret = require_env("JWT_SECRET")?;
    let jwt = Arc::new(JwtKeys::from_secret("k1", secret.as_bytes(), 3600));

    // Хранилище: Postgres (diesel-async). Схема применяется на старте идемпотентно.
    let db_url = require_env("DATABASE_URL")?;
    let pool = storage::pg::build_pool(&db_url)
        .await
        .map_err(|e| anyhow::anyhow!("db pool: {e}"))?;
    storage::pg::run_migrations(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("migrate: {e}"))?;
    let locks: Arc<dyn WalletLock> = Arc::new(storage::PgWalletLock::new(pool.clone()));
    let pg = Arc::new(storage::pg::PgStore::new(pool));
    let users: Arc<dyn UserRepository> = pg.clone();
    let wallets: Arc<dyn WalletRepository> = pg.clone();
    let txs: Arc<dyn TransactionRepository> = pg.clone();
    let audit: Arc<dyn AuditRepository> = pg;
    tracing::info!("storage: Postgres (diesel-async), locks: advisory");

    // Signer: только удалённый signing-service по gRPC (опционально mTLS). Ключи живут в
    // отдельном процессе — gateway получает лишь адреса и подписи.
    let endpoint = require_env("SIGNER_GRPC_ENDPOINT")?;
    let tls = client_tls_from_env()?;
    let remote = crate::remote_signer::RemoteSigner::connect(&endpoint, tls)
        .await
        .map_err(|e| anyhow::anyhow!("remote signer: {e}"))?;
    let signer: Arc<dyn signing_service::Signer> = Arc::new(remote);
    tracing::info!(%endpoint, "signer: remote gRPC signing-service");

    // Сети: EVM обязателен, Bitcoin и Solana подключаются при заданных адресах RPC.
    let mut chains: HashMap<Chain, Arc<dyn BlockchainClient>> = HashMap::new();

    let evm_url = require_env("EVM_RPC_URL")?;
    let evm = chain_evm::EvmClient::new(
        &evm_url,
        blockchain::ChainConfig {
            chain: Chain::Ethereum,
            decimals: 18,
            confirmations: Some(3),
            reorg_window: 6,
            dust_limit: core_domain::U256::from(1u64),
            fee_cap_factor: 2.0,
        },
    )
    .map_err(|e| anyhow::anyhow!("evm adapter: {e}"))?;
    chains.insert(Chain::Ethereum, Arc::new(evm));
    tracing::info!("EVM: alloy adapter ({evm_url})");

    if let Ok(url) = std::env::var("BTC_ESPLORA_URL") {
        let cfg = blockchain::ChainConfig {
            chain: Chain::Bitcoin,
            decimals: 8,
            confirmations: Some(2),
            reorg_window: 6,
            dust_limit: core_domain::U256::from(546u64),
            fee_cap_factor: 2.0,
        };
        chains.insert(
            Chain::Bitcoin,
            Arc::new(chain_btc::BtcClient::new(&url, cfg)),
        );
        tracing::info!("BTC: Esplora adapter ({url})");
    }

    if let Ok(url) = std::env::var("SOLANA_RPC_URL") {
        let cfg = blockchain::ChainConfig {
            chain: Chain::Solana,
            decimals: 9,
            confirmations: None,
            reorg_window: 0,
            dust_limit: core_domain::U256::from(1u64),
            fee_cap_factor: 2.0,
        };
        chains.insert(
            Chain::Solana,
            Arc::new(chain_sol::SolClient::new(&url, cfg)),
        );
        tracing::info!("SOL: RPC adapter ({url})");
    }

    // Кеш балансов и идемпотентность — Redis.
    let redis_url = require_env("REDIS_URL")?;
    let cache: Arc<dyn storage::BalanceCache> = Arc::new(
        storage::RedisBalanceCache::connect(&redis_url)
            .await
            .map_err(|e| anyhow::anyhow!("redis cache: {e}"))?,
    );
    let idempotency: Arc<dyn Idempotency> = Arc::new(
        crate::idempotency::RedisIdempotency::connect(&redis_url, 86_400)
            .await
            .map_err(|e| anyhow::anyhow!("redis idempotency: {e}"))?,
    );
    tracing::info!("cache/idempotency: Redis");

    // Аналитика — ClickHouse (HTTP). Запись best-effort, но подключение обязательно.
    let ch_url = require_env("CLICKHOUSE_URL")?;
    let analytics: Arc<dyn storage::AnalyticsSink> = Arc::new(
        storage::ClickHouseAnalytics::connect(&ch_url, "tx_history")
            .await
            .map_err(|e| anyhow::anyhow!("clickhouse: {e}"))?,
    );
    tracing::info!("analytics: ClickHouse ({ch_url})");

    // Комплаенс — HTTP-провайдеры KYC и AML.
    let kyc: Arc<dyn KycProvider> = Arc::new(HttpKyc::new(&require_env("KYC_PROVIDER_URL")?));
    let aml: Arc<dyn AmlScreener> =
        Arc::new(HttpAmlScreener::new(&require_env("AML_SCREENING_URL")?));
    tracing::info!("kyc/aml: HTTP providers");

    Ok(AppState {
        jwt,
        users,
        wallets,
        txs,
        signer,
        chains: Arc::new(chains),
        cache,
        audit,
        analytics,
        kyc,
        aml,
        metrics: Arc::new(crate::metrics::Metrics::new()),
        events: crate::events::channel(),
        locks,
        idempotency,
        config: Config::default(),
    })
}

/// Собрать клиентский mTLS-конфиг для RemoteSigner из env. Нужны все четыре переменные:
/// `SIGNER_TLS_CLIENT_CERT`, `SIGNER_TLS_CLIENT_KEY`, `SIGNER_TLS_CA`, `SIGNER_TLS_DOMAIN`.
/// Если их нет — соединение остаётся plaintext (допустимо только в доверенной приватной сети).
fn client_tls_from_env() -> anyhow::Result<Option<tonic::transport::ClientTlsConfig>> {
    let (Ok(cert), Ok(key), Ok(ca), Ok(domain)) = (
        std::env::var("SIGNER_TLS_CLIENT_CERT"),
        std::env::var("SIGNER_TLS_CLIENT_KEY"),
        std::env::var("SIGNER_TLS_CA"),
        std::env::var("SIGNER_TLS_DOMAIN"),
    ) else {
        tracing::warn!("remote signer: PLAINTEXT (set SIGNER_TLS_* for mTLS)");
        return Ok(None);
    };
    let cert = std::fs::read(&cert)?;
    let key = std::fs::read(&key)?;
    let ca = std::fs::read(&ca)?;
    Ok(Some(proto::tls::client_config(&cert, &key, &ca, domain)))
}

/// CORS: конкретный origin из `CORS_ORIGIN` (домен фронта на Vercel),
/// иначе permissive для локальной разработки.
fn cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{Any, CorsLayer};
    match std::env::var("CORS_ORIGIN") {
        Ok(origin) => match origin.parse::<axum::http::HeaderValue>() {
            Ok(value) => CorsLayer::new()
                .allow_origin(value)
                .allow_methods(Any)
                .allow_headers(Any),
            Err(_) => CorsLayer::permissive(),
        },
        Err(_) => CorsLayer::permissive(),
    }
}

/// Ждать сигнал остановки (Ctrl-C / SIGINT). Как только он пришёл, `axum` перестаёт
/// принимать новые соединения и даёт текущим запросам доиграть.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received, draining");
}

/// Инициализировать логирование. Уровень берётся из `RUST_LOG`, по умолчанию `info`.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
