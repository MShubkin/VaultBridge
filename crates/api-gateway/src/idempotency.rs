//! Идемпотентность мутаций. Ключ неймспейснут по `user_id`.
//! Трейт async; в production — Redis (`SET NX`), in-memory остаётся только для тестов.

use core_domain::UserId;

/// Исход попытки начать операцию под ключом.
pub enum Begin {
    /// Свежий ключ — выполняем операцию.
    Fresh,
    /// Та же операция уже идёт параллельно → `409`.
    InFlight,
    /// Операция уже завершена — вернуть сохранённый ответ.
    Done(serde_json::Value),
}

#[async_trait::async_trait]
pub trait Idempotency: Send + Sync {
    async fn begin(&self, user: UserId, key: &str) -> Begin;
    async fn complete(&self, user: UserId, key: &str, value: serde_json::Value);
    /// Снять `InFlight` при ошибке — разрешить повтор тем же ключом.
    async fn abort(&self, user: UserId, key: &str);
}

#[cfg(test)]
#[derive(Clone)]
enum Entry {
    InFlight,
    Done(serde_json::Value),
}

/// Идемпотентность в памяти — тест-двойник. В production используется `RedisIdempotency`.
#[cfg(test)]
#[derive(Default)]
pub struct InMemoryIdempotency {
    inner: std::sync::Mutex<std::collections::HashMap<(UserId, String), Entry>>,
}

#[cfg(test)]
impl InMemoryIdempotency {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Idempotency for InMemoryIdempotency {
    async fn begin(&self, user: UserId, key: &str) -> Begin {
        let mut map = self.inner.lock().unwrap();
        match map.get(&(user, key.to_string())) {
            Some(Entry::InFlight) => Begin::InFlight,
            Some(Entry::Done(v)) => Begin::Done(v.clone()),
            None => {
                map.insert((user, key.to_string()), Entry::InFlight);
                Begin::Fresh
            }
        }
    }

    async fn complete(&self, user: UserId, key: &str, value: serde_json::Value) {
        self.inner
            .lock()
            .unwrap()
            .insert((user, key.to_string()), Entry::Done(value));
    }

    async fn abort(&self, user: UserId, key: &str) {
        self.inner.lock().unwrap().remove(&(user, key.to_string()));
    }
}

/// Redis-реализация: `SET key in_flight NX EX` для атомарного входа.
/// Готовый результат хранится JSON-строкой; ключ неймспейснут по user.
pub struct RedisIdempotency {
    conn: redis::aio::MultiplexedConnection,
    ttl_secs: u64,
}

impl RedisIdempotency {
    pub async fn connect(url: &str, ttl_secs: u64) -> Result<Self, String> {
        let client = redis::Client::open(url).map_err(|e| e.to_string())?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { conn, ttl_secs })
    }

    fn key(user: UserId, key: &str) -> String {
        format!("idemp:{}:{}", user.0, key)
    }
}

#[async_trait::async_trait]
impl Idempotency for RedisIdempotency {
    async fn begin(&self, user: UserId, key: &str) -> Begin {
        use redis::AsyncCommands;
        let mut c = self.conn.clone();
        let k = Self::key(user, key);
        // Атомарный вход: SET NX EX. Успех → свежий ключ.
        let set: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(&k)
            .arg("in_flight")
            .arg("NX")
            .arg("EX")
            .arg(self.ttl_secs)
            .query_async(&mut c)
            .await;
        match set {
            Ok(Some(_)) => Begin::Fresh,
            _ => {
                // Ключ существует: готовый ответ или ещё in_flight.
                let val: Option<String> = c.get(&k).await.ok().flatten();
                match val.as_deref() {
                    Some("in_flight") | None => Begin::InFlight,
                    Some(json) => serde_json::from_str(json)
                        .map(Begin::Done)
                        .unwrap_or(Begin::InFlight),
                }
            }
        }
    }

    async fn complete(&self, user: UserId, key: &str, value: serde_json::Value) {
        use redis::AsyncCommands;
        let mut c = self.conn.clone();
        let k = Self::key(user, key);
        if let Ok(s) = serde_json::to_string(&value) {
            let _: redis::RedisResult<()> = c.set_ex(&k, s, self.ttl_secs).await;
        }
    }

    async fn abort(&self, user: UserId, key: &str) {
        use redis::AsyncCommands;
        let mut c = self.conn.clone();
        let _: redis::RedisResult<()> = c.del(Self::key(user, key)).await;
    }
}

/// Удобный конструктор Arc для in-memory идемпотентности (только для тестов).
#[cfg(test)]
pub fn in_memory() -> std::sync::Arc<dyn Idempotency> {
    std::sync::Arc::new(InMemoryIdempotency::new())
}
