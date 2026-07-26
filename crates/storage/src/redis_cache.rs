//! Redis-реализация кеша балансов: `SET EX` / `GET` / `DEL`.
//! Контракт `BalanceCache` тот же, что и у in-memory-версии, — отличается только бэкенд.
//! Соединение мультиплексированное, поэтому клон клиента дешёвый.

use std::time::Duration;

use redis::AsyncCommands;

use crate::cache::BalanceCache;
use crate::{Result, StorageError};

pub struct RedisBalanceCache {
    /// Мультиплексированное соединение с Redis: клонировать его дёшево, поэтому каждый
    /// вызов берёт свой клон и не сериализует запросы через общий мьютекс.
    conn: redis::aio::MultiplexedConnection,
}

impl RedisBalanceCache {
    /// Подключиться к Redis по URL. Ошибку клиента или соединения заворачиваем в
    /// `StorageError::Backend`.
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).map_err(|e| StorageError::Backend(e.to_string()))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(Self { conn })
    }
}

#[async_trait::async_trait]
impl BalanceCache for RedisBalanceCache {
    async fn get(&self, key: &str) -> Option<String> {
        let mut c = self.conn.clone();
        c.get::<_, Option<String>>(key).await.ok().flatten()
    }

    async fn put(&self, key: &str, value: String, ttl: Duration) {
        let mut c = self.conn.clone();
        // Кеш — best-effort: ошибку Redis не пробрасываем (деградация на miss).
        let _: redis::RedisResult<()> = c.set_ex(key, value, ttl.as_secs()).await;
    }

    async fn invalidate(&self, key: &str) {
        let mut c = self.conn.clone();
        let _: redis::RedisResult<()> = c.del(key).await;
    }
}
