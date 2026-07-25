//! `AppState`: дёшево клонируется (всё под `Arc`), `Send + Sync`.
//! Репозитории/клиенты — за трейт-объектами, чтобы реальные реализации (Diesel, EVM-RPC,
//! gRPC-signer) подменялись без правок хендлеров.

use std::collections::HashMap;
use std::sync::Arc;

use blockchain::BlockchainClient;
use core_domain::Chain;
use kyc_aml::{AmlScreener, KycProvider};
use signing_service::Signer;
use storage::{
    AnalyticsSink, AuditRepository, BalanceCache, TransactionRepository, UserRepository,
    WalletRepository,
};

use crate::auth::JwtKeys;
use crate::events::WalletEvent;
use crate::idempotency::Idempotency;
use crate::metrics::Metrics;
use storage::WalletLock;
use tokio::sync::broadcast;

/// Общее состояние приложения — по сути контейнер зависимостей, который axum раздаёт в
/// каждый хендлер. Всё под `Arc`, поэтому клон дешёвый: это просто копирование указателей.
/// Почти все зависимости спрятаны за трейт-объектами, так что в тестах на их место встают
/// in-memory-двойники, а в проде — реальные реализации, и хендлеры об этом не знают.
#[derive(Clone)]
pub struct AppState {
    /// Ключи для выпуска и проверки JWT.
    pub jwt: Arc<JwtKeys>,
    /// Репозиторий пользователей.
    pub users: Arc<dyn UserRepository>,
    /// Репозиторий кошельков (внутри — политика владения).
    pub wallets: Arc<dyn WalletRepository>,
    /// Репозиторий транзакций.
    pub txs: Arc<dyn TransactionRepository>,
    /// Подписант: локальный или удалённый (gRPC) — снаружи неотличимы.
    pub signer: Arc<dyn Signer>,
    /// Клиенты блокчейнов по сети. Нет клиента для сети → операция по ней недоступна.
    pub chains: Arc<HashMap<Chain, Arc<dyn BlockchainClient>>>,
    /// Кеш балансов (read-through с TTL).
    pub cache: Arc<dyn BalanceCache>,
    /// Аудит-журнал чувствительных операций.
    pub audit: Arc<dyn AuditRepository>,
    /// Сток аналитики (например, ClickHouse) — best-effort, не на критическом пути.
    pub analytics: Arc<dyn AnalyticsSink>,
    /// Провайдер KYC-проверок.
    pub kyc: Arc<dyn KycProvider>,
    /// Скринер AML (проверка адреса получателя перед выводом).
    pub aml: Arc<dyn AmlScreener>,
    /// Счётчики метрик.
    pub metrics: Arc<Metrics>,
    /// Шина WS-событий: сюда пишут хендлеры, отсюда читают подключённые клиенты.
    pub events: broadcast::Sender<WalletEvent>,
    /// Распределённые локи на кошелёк — чтобы два вывода с одного кошелька не шли параллельно.
    pub locks: Arc<dyn WalletLock>,
    /// Хранилище ключей идемпотентности вывода.
    pub idempotency: Arc<dyn Idempotency>,
    /// Прочие настройки рантайма.
    pub config: Config,
}

/// Настройки, которые не хочется тащить константами по коду.
#[derive(Clone)]
pub struct Config {
    /// Лимит кошельков на одного пользователя.
    pub max_wallets_per_user: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_wallets_per_user: 10,
        }
    }
}
