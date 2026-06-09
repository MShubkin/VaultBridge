//! Аналитический сток: append-only поток событий для отчётов и дашбордов.
//! В production пишем в ClickHouse (по HTTP). Запись best-effort: если сток недоступен,
//! основной путь не падает — просто теряем одну запись аналитики.

use serde::Serialize;

#[cfg(any(test, feature = "testing"))]
use std::sync::Mutex;

/// Одна аналитическая запись — намеренно денормализованная и плоская, чтобы по ней удобно
/// было строить агрегаты по сетям и активности без джоинов.
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsRecord {
    /// Тип события (например, `withdraw`, `confirm`).
    pub event: String,
    /// Сеть в строковом виде.
    pub chain: String,
    /// Направление (`incoming`/`outgoing`).
    pub direction: String,
    /// Кошелёк, к которому относится событие.
    pub wallet_id: String,
    /// Транзакция, к которой относится событие.
    pub tx_id: String,
    /// Сумма в минимальных единицах (строкой, чтобы не терять точность U256).
    pub amount_raw: String,
    /// Статус транзакции на момент события.
    pub status: String,
    /// Время события — Unix-секунды.
    pub ts: i64,
}

/// Куда складывать аналитические события. Реализация выбирается по конфигу.
#[async_trait::async_trait]
pub trait AnalyticsSink: Send + Sync {
    /// Записать событие. Семантика best-effort: ошибки не пробрасываются.
    async fn record(&self, rec: AnalyticsRecord);
}

/// Аналитика в памяти — тест-двойник, копит записи в векторе. В production не входит.
#[cfg(any(test, feature = "testing"))]
#[derive(Default)]
pub struct InMemoryAnalytics {
    /// Накопленные записи.
    inner: Mutex<Vec<AnalyticsRecord>>,
}

#[cfg(any(test, feature = "testing"))]
impl InMemoryAnalytics {
    /// Создать пустой сток.
    pub fn new() -> Self {
        Self::default()
    }
    /// Снять копию всех накопленных записей (для проверок в тестах).
    pub fn snapshot(&self) -> Vec<AnalyticsRecord> {
        self.inner.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "testing"))]
#[async_trait::async_trait]
impl AnalyticsSink for InMemoryAnalytics {
    async fn record(&self, rec: AnalyticsRecord) {
        self.inner.lock().unwrap().push(rec);
    }
}

/// ClickHouse через HTTP-интерфейс (порт 8123): INSERT ... FORMAT JSONEachRow.
pub struct ClickHouseAnalytics {
    base_url: String,
    table: String,
    http: reqwest::Client,
}

impl ClickHouseAnalytics {
    /// Подключиться и создать таблицу (идемпотентно). Ошибка connect не критична для запуска.
    pub async fn connect(base_url: &str, table: &str) -> Result<Self, crate::StorageError> {
        let s = Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            table: table.to_string(),
            http: reqwest::Client::new(),
        };
        let ddl = format!(
            "CREATE TABLE IF NOT EXISTS {} (\
                event String, chain String, direction String, wallet_id String, \
                tx_id String, amount_raw String, status String, ts Int64\
             ) ENGINE = MergeTree ORDER BY ts",
            s.table
        );
        s.http
            .post(&s.base_url)
            .body(ddl)
            .send()
            .await
            .map_err(|e| crate::StorageError::Backend(e.to_string()))?;
        Ok(s)
    }
}

#[async_trait::async_trait]
impl AnalyticsSink for ClickHouseAnalytics {
    async fn record(&self, rec: AnalyticsRecord) {
        let Ok(json) = serde_json::to_string(&rec) else {
            return;
        };
        let query = format!("INSERT INTO {} FORMAT JSONEachRow", self.table);
        // Best-effort: аналитика некритична, поэтому ошибку просто глотаем.
        let _ = self
            .http
            .post(&self.base_url)
            .query(&[("query", query.as_str())])
            .body(json)
            .send()
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> AnalyticsRecord {
        AnalyticsRecord {
            event: "withdraw".into(),
            chain: "ethereum".into(),
            direction: "outgoing".into(),
            wallet_id: "w1".into(),
            tx_id: "t1".into(),
            amount_raw: "1000".into(),
            status: "unconfirmed".into(),
            ts: 42,
        }
    }

    #[tokio::test]
    async fn in_memory_records_and_snapshots() {
        let a = InMemoryAnalytics::new();
        a.record(rec()).await;
        a.record(rec()).await;
        let snap = a.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].event, "withdraw");
        assert_eq!(snap[0].chain, "ethereum");
    }
}
