//! Кеш балансов: простой строковый KV с TTL и инвалидацией. Трейт сделан async, чтобы
//! Redis-реализация не блокировала рантайм tokio. In-memory-версия — для тестов и dev.

use std::time::Duration;

#[cfg(any(test, feature = "testing"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "testing"))]
use std::sync::Mutex;
#[cfg(any(test, feature = "testing"))]
use std::time::Instant;

#[async_trait::async_trait]
pub trait BalanceCache: Send + Sync {
    async fn get(&self, key: &str) -> Option<String>;
    async fn put(&self, key: &str, value: String, ttl: Duration);
    async fn invalidate(&self, key: &str);
}

/// Кеш в памяти — тест-двойник `BalanceCache`. В production не входит (фича `testing`).
#[cfg(any(test, feature = "testing"))]
#[derive(Default)]
pub struct InMemoryBalanceCache {
    inner: Mutex<HashMap<String, (String, Instant)>>,
}

#[cfg(any(test, feature = "testing"))]
impl InMemoryBalanceCache {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "testing"))]
#[async_trait::async_trait]
impl BalanceCache for InMemoryBalanceCache {
    async fn get(&self, key: &str) -> Option<String> {
        let mut map = self.inner.lock().unwrap();
        match map.get(key) {
            Some((value, expires_at)) if *expires_at > Instant::now() => Some(value.clone()),
            Some(_) => {
                map.remove(key); // протух — чистим
                None
            }
            None => None,
        }
    }

    async fn put(&self, key: &str, value: String, ttl: Duration) {
        self.inner
            .lock()
            .unwrap()
            .insert(key.to_string(), (value, Instant::now() + ttl));
    }

    async fn invalidate(&self, key: &str) {
        self.inner.lock().unwrap().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_then_get_hits() {
        let c = InMemoryBalanceCache::new();
        c.put("k", "v".into(), Duration::from_secs(30)).await;
        assert_eq!(c.get("k").await, Some("v".to_string()));
    }

    #[tokio::test]
    async fn expired_entry_misses() {
        let c = InMemoryBalanceCache::new();
        c.put("k", "v".into(), Duration::from_millis(0)).await;
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(c.get("k").await, None);
    }

    #[tokio::test]
    async fn invalidate_removes() {
        let c = InMemoryBalanceCache::new();
        c.put("k", "v".into(), Duration::from_secs(30)).await;
        c.invalidate("k").await;
        assert_eq!(c.get("k").await, None);
    }
}
