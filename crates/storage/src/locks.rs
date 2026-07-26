//! Сериализация исходящих операций по кошельку: два вывода с одного кошелька не должны
//! идти параллельно, иначе подерутся за nonce/UTXO.
//!
//! Трейт `WalletLock` асинхронный; guard живёт до конца саги, а release происходит на его
//! drop. In-memory-версия — для одного инстанса и тестов; Postgres advisory lock — для
//! нескольких реплик за общей базой.

use core_domain::WalletId;

#[cfg(any(test, feature = "testing"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "testing"))]
use std::sync::{Arc, Mutex};
#[cfg(any(test, feature = "testing"))]
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// Захваченный лок как непрозрачный owned-guard. Конкретный тип скрыт за `Box<dyn Send>`,
/// чтобы in-memory и Postgres-реализации возвращали разные guard'ы под одним типом.
/// Удерживается до завершения саги; release — на drop.
pub type LockGuard = Box<dyn Send>;

/// Распределённый лок на кошелёк.
#[async_trait::async_trait]
pub trait WalletLock: Send + Sync {
    /// Захватить лок по кошельку, дождавшись освобождения, если он занят.
    async fn lock(&self, wallet_id: WalletId) -> LockGuard;
}

// ---- in-memory (тест-двойник, не входит в production) ----

/// Лок в памяти на одном инстансе — для тестов. В production используется `PgWalletLock`.
#[cfg(any(test, feature = "testing"))]
#[derive(Clone, Default)]
pub struct InMemoryWalletLock {
    inner: Arc<Mutex<HashMap<WalletId, Arc<AsyncMutex<()>>>>>,
}

#[cfg(any(test, feature = "testing"))]
impl InMemoryWalletLock {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "testing"))]
#[async_trait::async_trait]
impl WalletLock for InMemoryWalletLock {
    async fn lock(&self, wallet_id: WalletId) -> LockGuard {
        let m = {
            let mut map = self.inner.lock().unwrap();
            map.entry(wallet_id)
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let guard: OwnedMutexGuard<()> = m.lock_owned().await;
        Box::new(guard)
    }
}

// ---- Postgres advisory lock ----

/// Распределённый лок через `pg_advisory_lock`.
///
/// Сессионный advisory-лок держится тем же соединением до явного unlock. Поскольку сага
/// — не одна БД-транзакция, а Drop не может быть async, применяется паттерн «лок в фоновой
/// задаче»: задача берёт соединение, захватывает лок, ждёт сигнала от guard (oneshot) и
/// затем снимает лок. Drop guard'а закрывает канал → задача делает unlock.
pub struct PgWalletLock {
    pool: crate::pg::PgPool,
}

impl PgWalletLock {
    pub fn new(pool: crate::pg::PgPool) -> Self {
        Self { pool }
    }
}

/// Хендл guard'а: при дропе закрывает release-канал, сигналя задаче снять лок.
struct PgGuard {
    _release: tokio::sync::oneshot::Sender<()>,
}

fn advisory_key(wallet_id: WalletId) -> i64 {
    let b = wallet_id.0.as_bytes();
    i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[async_trait::async_trait]
impl WalletLock for PgWalletLock {
    async fn lock(&self, wallet_id: WalletId) -> LockGuard {
        use diesel::sql_types::BigInt;
        use diesel_async::RunQueryDsl;

        let key = advisory_key(wallet_id);
        let pool = self.pool.clone();
        //сигнал от фоновой задачи к основному потоку: «блокировка захвачена».
        let (acquired_tx, acquired_rx) = tokio::sync::oneshot::channel::<()>();
        //сигнал от основного потока к фоновой задаче: «освобождай блокировку».
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            // ---Owned-соединение живёт до конца задачи (то есть до release)--
            //забираем соединение из пула во владение задачи.
            //Соединение будет жить ровно до завершения задачи, что гарантирует: блокировка удерживается одним соединением,
            //и оно не возвращается в пул раньше времени
            let mut conn = match pool.get_owned().await {
                Ok(c) => c,
                Err(_) => {
                    let _ = acquired_tx.send(()); // не блокируем сагу при сбое пула
                    return;
                }
            };
            let _ = diesel::sql_query("SELECT pg_advisory_lock($1)")
                .bind::<BigInt, _>(key)
                .execute(&mut conn)
                .await;
            let _ = acquired_tx.send(());
            // Ждём дропа guard'а (Ok недостижим — канал только закрывается).
            let _ = release_rx.await;
            let _ = diesel::sql_query("SELECT pg_advisory_unlock($1)")
                .bind::<BigInt, _>(key)
                .execute(&mut conn)
                .await;
        });

        // Возврат из lock() только после фактического захвата лока.
        let _ = acquired_rx.await;
        Box::new(PgGuard {
            _release: release_tx,
        })
    }
}
