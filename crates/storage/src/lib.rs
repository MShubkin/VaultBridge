//! Слой хранения: доменные записи, трейты репозиториев и политики доступа.
//!
//! Репозитории описаны трейтами, а реализаций две: in-memory (для тестов и dev без
//! инфраструктуры) и Diesel поверх Postgres (`diesel-async`). Бизнес-логика зависит только
//! от трейтов, поэтому смена бэкенда не трогает хендлеры. Тут же живут кеш, локи и
//! аналитический сток — всё за такими же трейтами.

use core_domain::{
    Chain, Direction, KycStatus, Role, TransactionId, TransactionStatus, UserId, WalletId, U256,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub mod analytics;
pub mod cache;
pub mod locks;
/// In-memory хранилище — только для тестов (см. фичу `testing`).
#[cfg(any(test, feature = "testing"))]
pub mod memory;
pub mod pg;
pub mod redis_cache;
pub mod schema;

pub use analytics::{AnalyticsRecord, AnalyticsSink, ClickHouseAnalytics};
pub use cache::BalanceCache;
pub use locks::{PgWalletLock, WalletLock};
pub use redis_cache::RedisBalanceCache;

// Тест-двойники: доступны только в тестовых сборках.
#[cfg(any(test, feature = "testing"))]
pub use analytics::InMemoryAnalytics;
#[cfg(any(test, feature = "testing"))]
pub use cache::InMemoryBalanceCache;
#[cfg(any(test, feature = "testing"))]
pub use locks::InMemoryWalletLock;
#[cfg(any(test, feature = "testing"))]
pub use memory::InMemoryStore;

/// Ошибки слоя хранения. Наружу они потом превращаются в HTTP-коды без утечки деталей.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Запись не найдена (или скрыта проверкой владельца).
    #[error("not found")]
    NotFound,
    /// Конфликт уникальности (например, повтор email).
    #[error("conflict: {0}")]
    Conflict(String),
    /// Превышен лимит (например, число кошельков на пользователя).
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),
    /// Ошибка самого бэкенда (БД, пул соединений и т.п.).
    #[error("backend error: {0}")]
    Backend(String),
}

/// Короткий алиас для результатов слоя хранения.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Учётная запись пользователя.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    /// Идентификатор.
    pub id: UserId,
    /// Email — он же логин.
    pub email: String,
    /// Argon2-хеш пароля (в открытом виде пароль нигде не хранится).
    pub password_hash: String,
    /// Текущий статус KYC.
    pub kyc_status: KycStatus,
    /// Роль доступа.
    pub role: Role,
    /// Индекс аккаунта пользователя в HD-дереве — чтобы кошельки разных людей не пересекались.
    pub hd_account_index: u32,
    /// Момент создания.
    pub created_at: OffsetDateTime,
}

/// Кошелёк — только публичные данные. Приватный ключ сюда не попадает: он выводится из
/// seed в signing-service по `derivation_path`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wallet {
    /// Идентификатор.
    pub id: WalletId,
    /// Владелец.
    pub user_id: UserId,
    /// Сеть кошелька.
    pub chain: Chain,
    /// Публичный адрес.
    pub address: String,
    /// Путь HD-деривации, например `m/44'/60'/{acct}'/0/{idx}`.
    pub derivation_path: String,
    /// Момент создания.
    pub created_at: OffsetDateTime,
}

/// Поля для создания пользователя.
pub struct NewUser {
    /// Email (логин).
    pub email: String,
    /// Готовый argon2-хеш пароля.
    pub password_hash: String,
    /// Роль доступа.
    pub role: Role,
}

/// Поля для создания кошелька.
pub struct NewWallet {
    /// Владелец.
    pub user_id: UserId,
    /// Сеть.
    pub chain: Chain,
    /// Публичный адрес (уже выведенный signing-service).
    pub address: String,
    /// Путь HD-деривации.
    pub derivation_path: String,
}

/// Транзакция в истории. Поля `to_address`/`fee_raw`/`idempotency_key` заполнены только
/// для исходящих — у входящих их нет.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    /// Идентификатор.
    pub id: TransactionId,
    /// Кошелёк, к которому относится.
    pub wallet_id: WalletId,
    /// Сеть.
    pub chain: Chain,
    /// Направление движения средств.
    pub direction: Direction,
    /// Адрес получателя (только для исходящих).
    pub to_address: Option<String>,
    /// Сумма в минимальных единицах.
    #[serde(with = "u256_str")]
    pub amount_raw: U256,
    /// Комиссия в минимальных единицах строкой (только для исходящих).
    pub fee_raw: Option<String>,
    /// Текущий статус по конечному автомату.
    pub status: TransactionStatus,
    /// Хэш транзакции в сети (появляется после broadcast).
    pub tx_hash: Option<String>,
    /// Ключ идемпотентности вывода (только для исходящих).
    pub idempotency_key: Option<String>,
    /// Chain-specific токен для реконсиляции: EVM — nonce, Solana — recent blockhash.
    /// По нему сканер отличает «заменена»/«истекла» от «ещё не дошла».
    pub tracking: Option<String>,
    /// Момент создания записи.
    pub created_at: OffsetDateTime,
}

/// Поля для создания исходящей транзакции — она заводится в статусе `Created` в начале саги.
pub struct NewOutgoing {
    /// Кошелёк-источник.
    pub wallet_id: WalletId,
    /// Сеть.
    pub chain: Chain,
    /// Адрес получателя.
    pub to_address: String,
    /// Сумма в минимальных единицах.
    pub amount_raw: U256,
    /// Ключ идемпотентности — защищает от задвоения вывода.
    pub idempotency_key: String,
}

/// Доступ к транзакциям.
#[async_trait::async_trait]
pub trait TransactionRepository: Send + Sync {
    /// Завести исходящую транзакцию в статусе `Created`.
    async fn create_outgoing(&self, new: NewOutgoing) -> Result<Transaction>;
    /// Перевести транзакцию в новый статус, попутно проставив хэш и/или комиссию.
    async fn set_status(
        &self,
        id: TransactionId,
        status: TransactionStatus,
        tx_hash: Option<String>,
        fee_raw: Option<U256>,
    ) -> Result<Transaction>;
    /// Сохранить chain-specific токен реконсиляции (nonce/blockhash) для транзакции.
    async fn set_tracking(&self, id: TransactionId, tracking: &str) -> Result<()>;
    /// Достать транзакцию по id.
    async fn get(&self, id: TransactionId) -> Result<Transaction>;
    /// История транзакций конкретного кошелька.
    async fn list_for_wallet(&self, wallet_id: WalletId) -> Result<Vec<Transaction>>;
    /// Все исходящие транзакции по всем пользователям — для операторского доступа и
    /// фонового реконсилятора.
    async fn list_all_outgoing(&self) -> Result<Vec<Transaction>>;
}

/// Сериализуем U256 десятичной строкой: 78-значное число не влезает в JSON-число без
/// потери точности, а деньги ронять нельзя.
mod u256_str {
    use super::U256;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &U256, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<U256, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Запись аудита — строка в append-only журнале чувствительных операций.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Автоинкрементный идентификатор записи.
    pub id: i64,
    /// Кто инициировал действие (если применимо).
    pub actor: Option<UserId>,
    /// Что за действие (строковый код).
    pub action: String,
    /// К какому кошельку относится (если применимо).
    pub wallet_id: Option<WalletId>,
    /// Исход: `ok` | `denied` | `error`.
    pub result: String,
    /// Когда произошло.
    pub created_at: OffsetDateTime,
}

/// Поля для новой записи аудита.
pub struct NewAudit {
    /// Кто инициировал.
    pub actor: Option<UserId>,
    /// Код действия.
    pub action: String,
    /// Связанный кошелёк.
    pub wallet_id: Option<WalletId>,
    /// Исход.
    pub result: String,
}

/// Доступ к аудит-журналу.
#[async_trait::async_trait]
pub trait AuditRepository: Send + Sync {
    /// Дописать запись (журнал append-only — апдейтов нет).
    async fn record(&self, entry: NewAudit) -> Result<()>;
    /// Прочитать журнал целиком (операторский доступ).
    async fn list(&self) -> Result<Vec<AuditEntry>>;
}

/// Доступ к пользователям.
#[async_trait::async_trait]
pub trait UserRepository: Send + Sync {
    /// Создать пользователя.
    async fn create(&self, new: NewUser) -> Result<User>;
    /// Найти по id.
    async fn by_id(&self, id: UserId) -> Result<User>;
    /// Найти по email (для логина).
    async fn by_email(&self, email: &str) -> Result<User>;
    /// Обновить статус KYC.
    async fn set_kyc(&self, id: UserId, status: KycStatus) -> Result<()>;
}

/// Доступ к кошелькам. Здесь же зашита политика владения, чтобы её нельзя было обойти.
#[async_trait::async_trait]
pub trait WalletRepository: Send + Sync {
    /// Создать кошелёк. `max_per_user` — лимит на пользователя: при превышении вернётся
    /// `LimitExceeded`.
    async fn create(&self, new: NewWallet, max_per_user: usize) -> Result<Wallet>;

    /// Кошельки пользователя (только свои).
    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Wallet>>;

    /// Достать кошелёк с проверкой владельца — на этом стоит экстрактор `OwnedWallet`.
    /// Чужой или несуществующий одинаково дают `NotFound`, чтобы наружу уходил `404` и не
    /// раскрывал факт существования кошелька.
    async fn owned(&self, id: WalletId, user_id: UserId) -> Result<Wallet>;

    /// Достать кошелёк по id без проверки владельца — для внутренних задач: например,
    /// block-scanner так находит владельца исходящей транзакции, чтобы адресовать WS-событие.
    /// Наружу этот метод не выставляется.
    async fn by_id(&self, id: WalletId) -> Result<Wallet>;
}
