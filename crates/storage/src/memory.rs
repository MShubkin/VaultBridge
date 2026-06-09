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

#[derive(Default)]
pub struct InMemoryStore {
    users: Mutex<HashMap<UserId, User>>,
    wallets: Mutex<HashMap<WalletId, Wallet>>,
    txs: Mutex<HashMap<TransactionId, Transaction>>,
    audit: Mutex<Vec<AuditEntry>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl UserRepository for InMemoryStore {
    async fn create(&self, new: NewUser) -> Result<User> {
        let mut users = self.users.lock().unwrap();
        if users.values().any(|u| u.email == new.email) {
            return Err(StorageError::Conflict("email already exists".into()));
        }
        let user = User {
            id: UserId::new(),
            email: new.email,
            password_hash: new.password_hash,
            kyc_status: KycStatus::Pending,
            role: new.role,
            hd_account_index: users.len() as u32,
            created_at: OffsetDateTime::now_utc(),
        };
        users.insert(user.id, user.clone());
        Ok(user)
    }

    async fn by_id(&self, id: UserId) -> Result<User> {
        self.users
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    async fn by_email(&self, email: &str) -> Result<User> {
        self.users
            .lock()
            .unwrap()
            .values()
            .find(|u| u.email == email)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    async fn set_kyc(&self, id: UserId, status: KycStatus) -> Result<()> {
        let mut users = self.users.lock().unwrap();
        let user = users.get_mut(&id).ok_or(StorageError::NotFound)?;
        user.kyc_status = status;
        Ok(())
    }
}

#[async_trait::async_trait]
impl WalletRepository for InMemoryStore {
    async fn create(&self, new: NewWallet, max_per_user: usize) -> Result<Wallet> {
        let mut wallets = self.wallets.lock().unwrap();
        let count = wallets
            .values()
            .filter(|w| w.user_id == new.user_id)
            .count();
        if count >= max_per_user {
            return Err(StorageError::LimitExceeded(format!(
                "max {max_per_user} wallets per user"
            )));
        }
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

    async fn owned(&self, id: WalletId, user_id: UserId) -> Result<Wallet> {
        self.wallets
            .lock()
            .unwrap()
            .get(&id)
            .filter(|w| w.user_id == user_id)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

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

    async fn set_tracking(&self, id: TransactionId, tracking: &str) -> Result<()> {
        let mut txs = self.txs.lock().unwrap();
        let tx = txs.get_mut(&id).ok_or(StorageError::NotFound)?;
        tx.tracking = Some(tracking.to_string());
        Ok(())
    }

    async fn get(&self, id: TransactionId) -> Result<Transaction> {
        self.txs
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

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
    async fn record(&self, entry: NewAudit) -> Result<()> {
        let mut log = self.audit.lock().unwrap();
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
