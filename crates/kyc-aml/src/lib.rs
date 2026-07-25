//! Контур комплаенса: KYC при онбординге и AML-скрининг адресов перед выводом.
//!
//! В production оба контура — это HTTP-клиенты к внешним сервисам ([`HttpKyc`],
//! [`HttpAmlScreener`]); адреса сервисов берутся из конфигурации. Поведение при сбое
//! провайдера — «fail-closed»: непройденный KYC и подозрение AML блокируют операцию, а не
//! пропускают её. Для тестов есть лёгкие двойники под фичой `testing`.

use core_domain::{Chain, KycStatus};
use serde::Deserialize;

/// Провайдер KYC. Дёргается при создании пользователя и решает, пускать ли его дальше.
#[async_trait::async_trait]
pub trait KycProvider: Send + Sync {
    /// Отправить пользователя на проверку и получить итоговый статус.
    async fn submit(&self, email: &str) -> KycStatus;
}

/// AML-скрининг адреса получателя — проверка по внешнему сервису до всякой бизнес-логики.
#[async_trait::async_trait]
pub trait AmlScreener: Send + Sync {
    /// Запрещён ли адрес в данной сети.
    async fn is_blacklisted(&self, chain: Chain, address: &str) -> bool;
}

// ---- production: HTTP-провайдеры ----

/// KYC поверх HTTP. Шлёт `POST {url}` с телом `{ "email": ... }` и ждёт в ответ
/// `{ "status": "pending|approved|rejected" }`.
pub struct HttpKyc {
    /// Переиспользуемый HTTP-клиент.
    http: reqwest::Client,
    /// Эндпоинт внешнего KYC-сервиса.
    url: String,
}

impl HttpKyc {
    /// Создать провайдер, указывающий на эндпоинт внешнего KYC-сервиса.
    pub fn new(url: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: url.to_string(),
        }
    }
}

/// Ответ KYC-сервиса: строковый статус `pending|approved|rejected`.
#[derive(Deserialize)]
struct KycResponse {
    status: String,
}

#[async_trait::async_trait]
impl KycProvider for HttpKyc {
    async fn submit(&self, email: &str) -> KycStatus {
        let resp = self
            .http
            .post(&self.url)
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await;
        match resp.and_then(|r| r.error_for_status()) {
            Ok(r) => match r.json::<KycResponse>().await {
                Ok(body) => KycStatus::parse(&body.status).unwrap_or(KycStatus::Pending),
                Err(e) => {
                    tracing::warn!(error = %e, "kyc: bad response body");
                    KycStatus::Pending
                }
            },
            Err(e) => {
                // Fail-closed: провайдер недоступен → считаем непройденным, вывод блокируется.
                tracing::warn!(error = %e, "kyc: provider unavailable");
                KycStatus::Pending
            }
        }
    }
}

/// AML-скрининг поверх HTTP. Шлёт `POST {url}` с телом `{ "chain": ..., "address": ... }`
/// и ждёт `{ "blacklisted": bool }`.
pub struct HttpAmlScreener {
    /// Переиспользуемый HTTP-клиент.
    http: reqwest::Client,
    /// Эндпоинт внешнего AML-сервиса.
    url: String,
}

impl HttpAmlScreener {
    /// Создать скринер, указывающий на эндпоинт внешнего AML-сервиса.
    pub fn new(url: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: url.to_string(),
        }
    }
}

/// Ответ AML-сервиса: под санкциями ли адрес.
#[derive(Deserialize)]
struct AmlResponse {
    blacklisted: bool,
}

#[async_trait::async_trait]
impl AmlScreener for HttpAmlScreener {
    async fn is_blacklisted(&self, chain: Chain, address: &str) -> bool {
        let resp = self
            .http
            .post(&self.url)
            .json(&serde_json::json!({ "chain": chain.as_str(), "address": address }))
            .send()
            .await;
        match resp.and_then(|r| r.error_for_status()) {
            Ok(r) => match r.json::<AmlResponse>().await {
                Ok(body) => body.blacklisted,
                Err(e) => {
                    // Fail-closed: непонятный ответ → блокируем адрес.
                    tracing::warn!(error = %e, "aml: bad response body");
                    true
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "aml: screener unavailable");
                true
            }
        }
    }
}

// ---- тест-двойники (фича `testing`) ----

/// Мок KYC: всегда возвращает заранее заданный статус. Только для тестов.
#[cfg(any(test, feature = "testing"))]
pub struct MockKyc {
    status: KycStatus,
}

#[cfg(any(test, feature = "testing"))]
impl MockKyc {
    /// Создать мок, отвечающий заданным статусом.
    pub fn new(status: KycStatus) -> Self {
        Self { status }
    }
}

#[cfg(any(test, feature = "testing"))]
#[async_trait::async_trait]
impl KycProvider for MockKyc {
    async fn submit(&self, _email: &str) -> KycStatus {
        self.status
    }
}

/// Блэклист в памяти — мок санкционного списка. Только для тестов.
#[cfg(any(test, feature = "testing"))]
#[derive(Default)]
pub struct InMemoryBlacklist {
    entries: std::sync::Mutex<std::collections::HashSet<(Chain, String)>>,
}

#[cfg(any(test, feature = "testing"))]
impl InMemoryBlacklist {
    /// Создать пустой блэклист.
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавить адрес в блэклист для конкретной сети.
    pub fn add(&self, chain: Chain, address: &str) {
        self.entries
            .lock()
            .unwrap()
            .insert((chain, address.to_string()));
    }
}

#[cfg(any(test, feature = "testing"))]
#[async_trait::async_trait]
impl AmlScreener for InMemoryBlacklist {
    async fn is_blacklisted(&self, chain: Chain, address: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .contains(&(chain, address.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_kyc_returns_configured_status() {
        let kyc = MockKyc::new(KycStatus::Approved);
        assert_eq!(kyc.submit("a@b.c").await, KycStatus::Approved);
    }

    #[tokio::test]
    async fn blacklist_flags_known_address() {
        let bl = InMemoryBlacklist::new();
        bl.add(Chain::Ethereum, "0xbad");
        assert!(bl.is_blacklisted(Chain::Ethereum, "0xbad").await);
        assert!(!bl.is_blacklisted(Chain::Ethereum, "0xgood").await);
        assert!(!bl.is_blacklisted(Chain::Bitcoin, "0xbad").await); // сеть учитывается
    }
}
