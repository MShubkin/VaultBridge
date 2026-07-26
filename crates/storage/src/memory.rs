//! In-memory реализация репозиториев — для тестов и локального прогона без БД.
//! Контракт идентичен будущей Diesel-реализации (тот же трейт).

use std::collections::HashMap;
use std::sync::Mutex;

use core_domain::{
    Chain, Direction, KycStatus, TransactionId, TransactionStatus, UserId, WalletId, U256,
};
use time::OffsetDateTime;

use crate::{
    AuditEntry, AuditRepository, NewAudit, NewOutgoing, NewUser, NewWallet, Result, StorageError,
    Transaction, TransactionRepository, User, UserRepository, Wallet, WalletRepository,
};

/// Хранилище в оперативной памяти: четыре таблицы, каждая под своим `Mutex`.
///
/// Данные живут, пока жив процесс: после рестарта всё пропадает. Годится для тестов и
/// локального прогона без БД, в production не используется. Блокировки берём по одной
/// таблице за раз и держим коротко, поэтому дедлоков тут не возникает.
#[derive(Default)]
pub struct InMemoryStore {
    /// Пользователи по их id.
    users: Mutex<HashMap<UserId, User>>,
    /// Кошельки по id.
    wallets: Mutex<HashMap<WalletId, Wallet>>,
    /// Транзакции по id — и входящие, и исходящие в одной карте.
    txs: Mutex<HashMap<TransactionId, Transaction>>,
    /// Аудит-журнал. Он append-only, поэтому обычный `Vec`, а не карта по ключу.
    audit: Mutex<Vec<AuditEntry>>,
}

impl InMemoryStore {
    /// Пустое хранилище.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl UserRepository for InMemoryStore {
    /// Завести пользователя. Новичок всегда стартует со статусом KYC `Pending`.
    async fn create(&self, new: NewUser) -> Result<User> {
        let mut users = self.users.lock().unwrap();
        // Email — это логин, второй такой же заводить нельзя.
        if users.values().any(|u| u.email == new.email) {
            return Err(StorageError::Conflict("email already exists".into()));
        }
        let user = User {
            id: UserId::new(),
            email: new.email,
            password_hash: new.password_hash,
            kyc_status: KycStatus::Pending,
            role: new.role,
            // Индекс аккаунта в HD-дереве раздаём по порядку появления. Пользователей
            // тут не удаляют, поэтому `len()` всегда даёт свежий, ещё не занятый номер —
            // ветки ключей у разных людей не пересекаются.
            hd_account_index: users.len() as u32,
            created_at: OffsetDateTime::now_utc(),
        };
        users.insert(user.id, user.clone());
        Ok(user)
    }

    /// Найти пользователя по id.
    async fn by_id(&self, id: UserId) -> Result<User> {
        self.users
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    /// Найти пользователя по email — этим пользуется логин. Линейный перебор: для тестового
    /// хранилища объёмы небольшие, индекс по email заводить незачем.
    async fn by_email(&self, email: &str) -> Result<User> {
        self.users
            .lock()
            .unwrap()
            .values()
            .find(|u| u.email == email)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    /// Обновить статус KYC. Нет пользователя — `NotFound`.
    async fn set_kyc(&self, id: UserId, status: KycStatus) -> Result<()> {
        let mut users = self.users.lock().unwrap();
        let user = users.get_mut(&id).ok_or(StorageError::NotFound)?;
        user.kyc_status = status;
        Ok(())
    }
}

#[async_trait::async_trait]
impl WalletRepository for InMemoryStore {
    /// Создать кошелёк, соблюдая лимит на пользователя и уникальность адреса.
    async fn create(&self, new: NewWallet, max_per_user: usize) -> Result<Wallet> {
        let mut wallets = self.wallets.lock().unwrap();
        // Считаем, сколько кошельков уже у этого пользователя, и упираемся в лимит.
        let count = wallets
            .values()
            .filter(|w| w.user_id == new.user_id)
            .count();
        if count >= max_per_user {
            return Err(StorageError::LimitExceeded(format!(
                "max {max_per_user} wallets per user"
            )));
        }
        // Один и тот же адрес в одной сети дважды не заводим (в БД это UNIQUE-констрейнт).
        if wallets
            .values()
            .any(|w| w.chain == new.chain && w.address == new.address)
        {
            return Err(StorageError::Conflict("address already exists".into()));
        }
        let wallet = Wallet {
            id: WalletId::new(),
            user_id: new.user_id,
            chain: new.chain,
            address: new.address,
            derivation_path: new.derivation_path,
            created_at: OffsetDateTime::now_utc(),
        };
        wallets.insert(wallet.id, wallet.clone());
        Ok(wallet)
    }

    /// Кошельки пользователя, по возрастанию времени создания — так список стабилен между
    /// вызовами (порядок обхода `HashMap` сам по себе случайный).
    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Wallet>> {
        let mut list: Vec<Wallet> = self
            .wallets
            .lock()
            .unwrap()
            .values()
            .filter(|w| w.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by_key(|w| w.created_at);
        Ok(list)
    }

    /// Достать кошелёк с проверкой владельца. Чужой кошелёк и несуществующий одинаково дают
    /// `NotFound` — наружу это `404`, который не подсказывает, что такой кошелёк вообще есть.
    async fn owned(&self, id: WalletId, user_id: UserId) -> Result<Wallet> {
        self.wallets
            .lock()
            .unwrap()
            .get(&id)
            .filter(|w| w.user_id == user_id)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    /// Достать кошелёк по id без проверки владельца — для внутренних задач (например, сканер
    /// так находит владельца транзакции). Наружу не выставляется.
    async fn by_id(&self, id: WalletId) -> Result<Wallet> {
        self.wallets
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(StorageError::NotFound)
    }
}

#[async_trait::async_trait]
impl TransactionRepository for InMemoryStore {
    /// Завести исходящую транзакцию в самом начале саги вывода. Комиссия, хэш и токен
    /// реконсиляции ещё неизвестны, поэтому все они `None`, а статус — `Created`.
    async fn create_outgoing(&self, new: NewOutgoing) -> Result<Transaction> {
        let tx = Transaction {
            id: TransactionId::new(),
            wallet_id: new.wallet_id,
            chain: new.chain,
            direction: Direction::Outgoing,
            to_address: Some(new.to_address),
            amount_raw: new.amount_raw,
            fee_raw: None,
            status: TransactionStatus::Created,
            tx_hash: None,
            idempotency_key: Some(new.idempotency_key),
            tracking: None,
            created_at: OffsetDateTime::now_utc(),
        };
        self.txs.lock().unwrap().insert(tx.id, tx.clone());
        Ok(tx)
    }

    /// Перевести транзакцию в новый статус. Хэш и комиссию проставляем, только если они
    /// пришли: последующие смены статуса передают `None`, и затирать уже известный хэш
    /// таким `None` нельзя.
    async fn set_status(
        &self,
        id: TransactionId,
        status: TransactionStatus,
        tx_hash: Option<String>,
        fee_raw: Option<U256>,
    ) -> Result<Transaction> {
        let mut txs = self.txs.lock().unwrap();
        let tx = txs.get_mut(&id).ok_or(StorageError::NotFound)?;
        tx.status = status;
        if tx_hash.is_some() {
            tx.tx_hash = tx_hash;
        }
        if let Some(fee) = fee_raw {
            tx.fee_raw = Some(fee.to_string());
        }
        Ok(tx.clone())
    }

    /// Сохранить токен реконсиляции (EVM — nonce, Solana — recent blockhash). По нему сканер
    /// потом отличает «заменена»/«истекла» от «просто ещё не дошла».
    async fn set_tracking(&self, id: TransactionId, tracking: &str) -> Result<()> {
        let mut txs = self.txs.lock().unwrap();
        let tx = txs.get_mut(&id).ok_or(StorageError::NotFound)?;
        tx.tracking = Some(tracking.to_string());
        Ok(())
    }

    /// Достать транзакцию по id.
    async fn get(&self, id: TransactionId) -> Result<Transaction> {
        self.txs
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    /// История транзакций одного кошелька, по времени создания.
    async fn list_for_wallet(&self, wallet_id: WalletId) -> Result<Vec<Transaction>> {
        let mut list: Vec<Transaction> = self
            .txs
            .lock()
            .unwrap()
            .values()
            .filter(|t| t.wallet_id == wallet_id)
            .cloned()
            .collect();
        list.sort_by_key(|t| t.created_at);
        Ok(list)
    }

    /// Все исходящие транзакции по всем пользователям — этим пользуются операторский доступ
    /// и фоновый реконсилятор, которому нужно обойти всё «в полёте».
    async fn list_all_outgoing(&self) -> Result<Vec<Transaction>> {
        let mut list: Vec<Transaction> = self
            .txs
            .lock()
            .unwrap()
            .values()
            .filter(|t| t.direction == Direction::Outgoing)
            .cloned()
            .collect();
        list.sort_by_key(|t| t.created_at);
        Ok(list)
    }
}

#[async_trait::async_trait]
impl AuditRepository for InMemoryStore {
    /// Дописать запись в журнал.
    async fn record(&self, entry: NewAudit) -> Result<()> {
        let mut log = self.audit.lock().unwrap();
        // id 1-based, как автоинкремент в БД: первая запись получает 1, а не 0.
        let id = log.len() as i64 + 1;
        log.push(AuditEntry {
            id,
            actor: entry.actor,
            action: entry.action,
            wallet_id: entry.wallet_id,
            result: entry.result,
            created_at: OffsetDateTime::now_utc(),
        });
        Ok(())
    }

    /// Прочитать журнал целиком (операторский доступ).
    async fn list(&self) -> Result<Vec<AuditEntry>> {
        Ok(self.audit.lock().unwrap().clone())
    }
}

/// Хелпер для построения адреса-заглушки на этапе 1 (реальная деривация — этап 2).
pub fn stub_address(chain: Chain, index: u32) -> String {
    format!("stub-{chain}-{index}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_domain::Role;

    fn new_user(email: &str) -> NewUser {
        NewUser {
            email: email.into(),
            password_hash: "hash".into(),
            role: Role::User,
        }
    }

    #[tokio::test]
    async fn create_and_fetch_user() {
        let store = InMemoryStore::new();
        let u = UserRepository::create(&store, new_user("a@b.c"))
            .await
            .unwrap();
        assert_eq!(
            UserRepository::by_id(&store, u.id).await.unwrap().email,
            "a@b.c"
        );
        assert_eq!(store.by_email("a@b.c").await.unwrap().id, u.id);
    }

    #[tokio::test]
    async fn set_tracking_persists() {
        let store = InMemoryStore::new();
        let tx = store
            .create_outgoing(NewOutgoing {
                wallet_id: WalletId::new(),
                chain: Chain::Ethereum,
                to_address: "0xabc".into(),
                amount_raw: U256::from(1u64),
                idempotency_key: "k".into(),
            })
            .await
            .unwrap();
        assert_eq!(tx.tracking, None);
        store.set_tracking(tx.id, "42").await.unwrap();
        assert_eq!(
            store.get(tx.id).await.unwrap().tracking.as_deref(),
            Some("42")
        );
    }

    #[tokio::test]
    async fn duplicate_email_conflicts() {
        let store = InMemoryStore::new();
        UserRepository::create(&store, new_user("a@b.c"))
            .await
            .unwrap();
        assert!(matches!(
            UserRepository::create(&store, new_user("a@b.c")).await,
            Err(StorageError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn wallet_limit_enforced() {
        let store = InMemoryStore::new();
        let u = UserRepository::create(&store, new_user("a@b.c"))
            .await
            .unwrap();
        let mk = |i: u32| NewWallet {
            user_id: u.id,
            chain: Chain::Ethereum,
            address: stub_address(Chain::Ethereum, i),
            derivation_path: format!("m/44'/60'/0'/0/{i}"),
        };
        WalletRepository::create(&store, mk(0), 2).await.unwrap();
        WalletRepository::create(&store, mk(1), 2).await.unwrap();
        assert!(matches!(
            WalletRepository::create(&store, mk(2), 2).await,
            Err(StorageError::LimitExceeded(_))
        ));
    }

    #[tokio::test]
    async fn owned_rejects_foreign_wallet() {
        let store = InMemoryStore::new();
        let owner = UserRepository::create(&store, new_user("a@b.c"))
            .await
            .unwrap();
        let other = UserRepository::create(&store, new_user("x@y.z"))
            .await
            .unwrap();
        let w = WalletRepository::create(
            &store,
            NewWallet {
                user_id: owner.id,
                chain: Chain::Bitcoin,
                address: stub_address(Chain::Bitcoin, 0),
                derivation_path: "m/44'/0'/0'/0/0".into(),
            },
            5,
        )
        .await
        .unwrap();
        assert!(store.owned(w.id, owner.id).await.is_ok());
        assert!(matches!(
            store.owned(w.id, other.id).await,
            Err(StorageError::NotFound)
        ));
    }
}
