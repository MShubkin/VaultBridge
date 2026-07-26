//! Postgres-реализация репозиториев на `diesel-async` (без libpq).
//! Транспорт — tokio-postgres внутри `AsyncPgConnection`; пул — bb8.
//!
//! Денежные величины (`amount_raw`/`fee_raw`) хранятся как TEXT (десятичная строка U256) —
//! лосслессно и без bigdecimal. Enum-поля — TEXT с конверсиями из `core-domain`.
//!
//! Прогон против реальной БД — интеграционные тесты на `DATABASE_URL` (CI/локально);
//! в этой кодовой базе путь проверен компиляцией (типобезопасность Diesel).

use diesel::prelude::*;
use diesel::result::{DatabaseErrorKind, Error as DieselError};
use diesel_async::pooled_connection::bb8::Pool;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use time::OffsetDateTime;
use uuid::Uuid;

use core_domain::{
    Chain, Direction, KycStatus, Role, TransactionId, TransactionStatus, UserId, WalletId, U256,
};

use crate::schema::{audit_log, transactions, users, wallets};
use crate::{
    AuditEntry, AuditRepository, NewAudit, NewOutgoing, NewUser, NewWallet, Result, StorageError,
    Transaction, TransactionRepository, User, UserRepository, Wallet, WalletRepository,
};

pub type PgPool = Pool<AsyncPgConnection>;

/// Построить пул async-соединений из `DATABASE_URL`.
pub async fn build_pool(database_url: &str) -> Result<PgPool> {
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    Pool::builder()
        .build(manager)
        .await
        .map_err(|e| StorageError::Backend(format!("pool: {e}")))
}

/// Применить схему при старте (идемпотентно, `CREATE TABLE IF NOT EXISTS`).
/// Без diesel-CLI/libpq — выполняем DDL через ту же async-связку. Для демо/dev;
/// в проде — версионируемые миграции (`diesel migration run`).
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| StorageError::Backend(e.to_string()))?;
    let ddl = include_str!("../../../migrations/0001_init/up.sql")
        .replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ")
        .replace("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS ");
    diesel_async::SimpleAsyncConnection::batch_execute(&mut *conn, &ddl)
        .await
        .map_err(|e| StorageError::Backend(format!("migrate: {e}")))?;
    Ok(())
}

/// Все репозитории поверх одного пула Postgres. Каждый метод берёт из пула соединение на
/// время запроса и возвращает обратно — своего состояния в `PgStore` нет.
pub struct PgStore {
    /// Пул async-соединений (bb8). Клонируется дёшево, шарится между хендлерами.
    pool: PgPool,
}

impl PgStore {
    /// Обернуть готовый пул. Пул создаётся один раз при старте через [`build_pool`].
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Перевести ошибку Diesel в доменную `StorageError`. Две ошибки разбираем по смыслу:
/// «строка не найдена» и нарушение UNIQUE (это конфликт, а не сбой БД). Всё остальное —
/// `Backend`, наружу оно уйдёт как 500 без деталей.
fn map_db(e: DieselError) -> StorageError {
    match e {
        DieselError::NotFound => StorageError::NotFound,
        DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, info) => {
            StorageError::Conflict(info.message().to_string())
        }
        other => StorageError::Backend(other.to_string()),
    }
}

/// Развернуть `Option`, полученный при разборе строкового значения из БД. `None` тут значит
/// битые данные (например, неизвестный код сети), поэтому это ошибка бэкенда, а не `NotFound`.
fn parse<T>(opt: Option<T>, what: &str) -> Result<T> {
    opt.ok_or_else(|| StorageError::Backend(format!("invalid {what} in db")))
}

// ---- users ----

/// Строка таблицы `users` как её видит Diesel: enum-поля тут ещё строки, id — сырой `Uuid`.
#[derive(Queryable, Selectable)]
#[diesel(table_name = users)]
struct UserRow {
    id: Uuid,
    email: String,
    password_hash: String,
    kyc_status: String,
    role: String,
    hd_account_index: i32,
    created_at: OffsetDateTime,
}

/// То, что вставляем при создании пользователя (набор колонок для INSERT).
#[derive(Insertable)]
#[diesel(table_name = users)]
struct UserInsert {
    id: Uuid,
    email: String,
    password_hash: String,
    kyc_status: String,
    role: String,
    hd_account_index: i32,
    created_at: OffsetDateTime,
}

/// Собрать доменного `User` из строки БД. Строковые коды статуса и роли тут превращаются
/// обратно в enum'ы; битое значение даёт ошибку бэкенда (см. [`parse`]).
fn to_user(r: UserRow) -> Result<User> {
    Ok(User {
        id: UserId(r.id),
        email: r.email,
        password_hash: r.password_hash,
        kyc_status: parse(KycStatus::parse(&r.kyc_status), "kyc_status")?,
        role: parse(Role::parse(&r.role), "role")?,
        hd_account_index: r.hd_account_index as u32,
        created_at: r.created_at,
    })
}

#[async_trait::async_trait]
impl UserRepository for PgStore {
    async fn create(&self, new: NewUser) -> Result<User> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        // Число уже существующих пользователей = следующий индекс HD-аккаунта. Пользователей
        // не удаляют, поэтому счётчик монотонный и индексы не переиспользуются.
        let count: i64 = users::table
            .count()
            .get_result(&mut conn)
            .await
            .map_err(map_db)?;
        let row = UserInsert {
            id: Uuid::new_v4(),
            email: new.email,
            password_hash: new.password_hash,
            kyc_status: KycStatus::Pending.as_str().to_string(),
            role: new.role.as_str().to_string(),
            hd_account_index: count as i32,
            created_at: OffsetDateTime::now_utc(),
        };
        let inserted: UserRow = diesel::insert_into(users::table)
            .values(&row)
            .returning(UserRow::as_returning())
            .get_result(&mut conn)
            .await
            .map_err(map_db)?;
        to_user(inserted)
    }

    async fn by_id(&self, id: UserId) -> Result<User> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let row: UserRow = users::table
            .find(id.0)
            .select(UserRow::as_select())
            .first(&mut conn)
            .await
            .map_err(map_db)?;
        to_user(row)
    }

    async fn by_email(&self, email: &str) -> Result<User> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let row: UserRow = users::table
            .filter(users::email.eq(email))
            .select(UserRow::as_select())
            .first(&mut conn)
            .await
            .map_err(map_db)?;
        to_user(row)
    }

    async fn set_kyc(&self, id: UserId, status: KycStatus) -> Result<()> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let n = diesel::update(users::table.find(id.0))
            .set(users::kyc_status.eq(status.as_str()))
            .execute(&mut conn)
            .await
            .map_err(map_db)?;
        if n == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }
}

// ---- wallets ----

/// Строка таблицы `wallets`. `chain` тут ещё строка, id/user_id — сырые `Uuid`.
#[derive(Queryable, Selectable)]
#[diesel(table_name = wallets)]
struct WalletRow {
    id: Uuid,
    user_id: Uuid,
    chain: String,
    address: String,
    derivation_path: String,
    created_at: OffsetDateTime,
}

/// Набор колонок для INSERT нового кошелька.
#[derive(Insertable)]
#[diesel(table_name = wallets)]
struct WalletInsert {
    id: Uuid,
    user_id: Uuid,
    chain: String,
    address: String,
    derivation_path: String,
    created_at: OffsetDateTime,
}

/// Собрать доменный `Wallet` из строки БД (код сети → enum `Chain`).
fn to_wallet(r: WalletRow) -> Result<Wallet> {
    Ok(Wallet {
        id: WalletId(r.id),
        user_id: UserId(r.user_id),
        chain: parse(Chain::parse(&r.chain), "chain")?,
        address: r.address,
        derivation_path: r.derivation_path,
        created_at: r.created_at,
    })
}

#[async_trait::async_trait]
impl WalletRepository for PgStore {
    async fn create(&self, new: NewWallet, max_per_user: usize) -> Result<Wallet> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let count: i64 = wallets::table
            .filter(wallets::user_id.eq(new.user_id.0))
            .count()
            .get_result(&mut conn)
            .await
            .map_err(map_db)?;
        if count as usize >= max_per_user {
            return Err(StorageError::LimitExceeded(format!(
                "max {max_per_user} wallets per user"
            )));
        }
        let row = WalletInsert {
            id: Uuid::new_v4(),
            user_id: new.user_id.0,
            chain: new.chain.as_str().to_string(),
            address: new.address,
            derivation_path: new.derivation_path,
            created_at: OffsetDateTime::now_utc(),
        };
        let inserted: WalletRow = diesel::insert_into(wallets::table)
            .values(&row)
            .returning(WalletRow::as_returning())
            .get_result(&mut conn)
            .await
            .map_err(map_db)?;
        to_wallet(inserted)
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Wallet>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let rows: Vec<WalletRow> = wallets::table
            .filter(wallets::user_id.eq(user_id.0))
            .order(wallets::created_at.asc())
            .select(WalletRow::as_select())
            .load(&mut conn)
            .await
            .map_err(map_db)?;
        rows.into_iter().map(to_wallet).collect()
    }

    async fn owned(&self, id: WalletId, user_id: UserId) -> Result<Wallet> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let row: WalletRow = wallets::table
            .filter(wallets::id.eq(id.0))
            .filter(wallets::user_id.eq(user_id.0))
            .select(WalletRow::as_select())
            .first(&mut conn)
            .await
            .map_err(map_db)?;
        to_wallet(row)
    }

    async fn by_id(&self, id: WalletId) -> Result<Wallet> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let row: WalletRow = wallets::table
            .filter(wallets::id.eq(id.0))
            .select(WalletRow::as_select())
            .first(&mut conn)
            .await
            .map_err(map_db)?;
        to_wallet(row)
    }
}

// ---- transactions ----

/// Строка таблицы `transactions`. Суммы (`amount_raw`/`fee_raw`) тут строки — U256 в
/// десятичном виде; enum-поля тоже строки. Разбор в доменные типы — в [`to_tx`].
#[derive(Queryable, Selectable)]
#[diesel(table_name = transactions)]
struct TxRow {
    id: Uuid,
    wallet_id: Uuid,
    chain: String,
    tx_hash: Option<String>,
    direction: String,
    to_address: Option<String>,
    amount_raw: String,
    fee_raw: Option<String>,
    status: String,
    idempotency_key: Option<String>,
    tracking: Option<String>,
    created_at: OffsetDateTime,
}

/// Колонки для INSERT новой транзакции. Здесь только то, что известно на старте саги:
/// хэша, комиссии и токена реконсиляции ещё нет — они проставляются позже через `set_status`
/// и `set_tracking`.
#[derive(Insertable)]
#[diesel(table_name = transactions)]
struct TxInsert {
    id: Uuid,
    wallet_id: Uuid,
    chain: String,
    direction: String,
    to_address: Option<String>,
    amount_raw: String,
    status: String,
    idempotency_key: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

/// Собрать доменную `Transaction` из строки БД. Сумма разбирается из десятичной строки в
/// U256; если строка битая — ошибка бэкенда, ронять число молча нельзя.
fn to_tx(r: TxRow) -> Result<Transaction> {
    Ok(Transaction {
        id: TransactionId(r.id),
        wallet_id: WalletId(r.wallet_id),
        chain: parse(Chain::parse(&r.chain), "chain")?,
        direction: parse(Direction::parse(&r.direction), "direction")?,
        to_address: r.to_address,
        amount_raw: parse(r.amount_raw.parse::<U256>().ok(), "amount_raw")?,
        fee_raw: r.fee_raw,
        status: parse(TransactionStatus::parse(&r.status), "status")?,
        tx_hash: r.tx_hash,
        idempotency_key: r.idempotency_key,
        tracking: r.tracking,
        created_at: r.created_at,
    })
}

#[async_trait::async_trait]
impl TransactionRepository for PgStore {
    async fn create_outgoing(&self, new: NewOutgoing) -> Result<Transaction> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let now = OffsetDateTime::now_utc();
        let row = TxInsert {
            id: Uuid::new_v4(),
            wallet_id: new.wallet_id.0,
            chain: new.chain.as_str().to_string(),
            direction: Direction::Outgoing.as_str().to_string(),
            to_address: Some(new.to_address),
            amount_raw: new.amount_raw.to_string(),
            status: TransactionStatus::Created.as_str().to_string(),
            idempotency_key: Some(new.idempotency_key),
            created_at: now,
            updated_at: now,
        };
        let inserted: TxRow = diesel::insert_into(transactions::table)
            .values(&row)
            .returning(TxRow::as_returning())
            .get_result(&mut conn)
            .await
            .map_err(map_db)?;
        to_tx(inserted)
    }

    async fn set_status(
        &self,
        id: TransactionId,
        status: TransactionStatus,
        tx_hash: Option<String>,
        fee_raw: Option<U256>,
    ) -> Result<Transaction> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        // Статус/updated_at — всегда; tx_hash и fee — только если переданы.
        let n = diesel::update(transactions::table.find(id.0))
            .set((
                transactions::status.eq(status.as_str()),
                transactions::updated_at.eq(OffsetDateTime::now_utc()),
            ))
            .execute(&mut conn)
            .await
            .map_err(map_db)?;
        if n == 0 {
            return Err(StorageError::NotFound);
        }
        if let Some(h) = tx_hash {
            diesel::update(transactions::table.find(id.0))
                .set(transactions::tx_hash.eq(h))
                .execute(&mut conn)
                .await
                .map_err(map_db)?;
        }
        if let Some(f) = fee_raw {
            diesel::update(transactions::table.find(id.0))
                .set(transactions::fee_raw.eq(f.to_string()))
                .execute(&mut conn)
                .await
                .map_err(map_db)?;
        }
        let fresh: TxRow = transactions::table
            .find(id.0)
            .select(TxRow::as_select())
            .first(&mut conn)
            .await
            .map_err(map_db)?;
        to_tx(fresh)
    }

    async fn set_tracking(&self, id: TransactionId, tracking: &str) -> Result<()> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let n = diesel::update(transactions::table.find(id.0))
            .set(transactions::tracking.eq(tracking))
            .execute(&mut conn)
            .await
            .map_err(map_db)?;
        if n == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    async fn get(&self, id: TransactionId) -> Result<Transaction> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let row: TxRow = transactions::table
            .find(id.0)
            .select(TxRow::as_select())
            .first(&mut conn)
            .await
            .map_err(map_db)?;
        to_tx(row)
    }

    async fn list_for_wallet(&self, wallet_id: WalletId) -> Result<Vec<Transaction>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let rows: Vec<TxRow> = transactions::table
            .filter(transactions::wallet_id.eq(wallet_id.0))
            .order(transactions::created_at.asc())
            .select(TxRow::as_select())
            .load(&mut conn)
            .await
            .map_err(map_db)?;
        rows.into_iter().map(to_tx).collect()
    }

    async fn list_all_outgoing(&self) -> Result<Vec<Transaction>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let rows: Vec<TxRow> = transactions::table
            .filter(transactions::direction.eq(Direction::Outgoing.as_str()))
            .order(transactions::created_at.asc())
            .select(TxRow::as_select())
            .load(&mut conn)
            .await
            .map_err(map_db)?;
        rows.into_iter().map(to_tx).collect()
    }
}

// ---- audit ----

/// Строка таблицы `audit_log`.
#[derive(Queryable, Selectable)]
#[diesel(table_name = audit_log)]
struct AuditRow {
    id: i64,
    actor: Option<Uuid>,
    action: String,
    wallet_id: Option<Uuid>,
    result: String,
    created_at: OffsetDateTime,
}

/// Колонки для INSERT записи аудита. `id` не задаём — его выдаёт автоинкремент БД.
#[derive(Insertable)]
#[diesel(table_name = audit_log)]
struct AuditInsert {
    actor: Option<Uuid>,
    action: String,
    wallet_id: Option<Uuid>,
    result: String,
    created_at: OffsetDateTime,
}

#[async_trait::async_trait]
impl AuditRepository for PgStore {
    async fn record(&self, entry: NewAudit) -> Result<()> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let row = AuditInsert {
            actor: entry.actor.map(|a| a.0),
            action: entry.action,
            wallet_id: entry.wallet_id.map(|w| w.0),
            result: entry.result,
            created_at: OffsetDateTime::now_utc(),
        };
        diesel::insert_into(audit_log::table)
            .values(&row)
            .execute(&mut conn)
            .await
            .map_err(map_db)?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<AuditEntry>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let rows: Vec<AuditRow> = audit_log::table
            .order(audit_log::id.asc())
            .select(AuditRow::as_select())
            .load(&mut conn)
            .await
            .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|r| AuditEntry {
                id: r.id,
                actor: r.actor.map(UserId),
                action: r.action,
                wallet_id: r.wallet_id.map(WalletId),
                result: r.result,
                created_at: r.created_at,
            })
            .collect())
    }
}
