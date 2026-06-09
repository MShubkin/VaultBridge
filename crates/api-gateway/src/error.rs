//! Единый тип ошибки API: наружу — стабильный `code` + безопасное
//! сообщение; внутренние причины (SQL, токены, тексты) — только в логах, не в теле.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use storage::StorageError;

/// Единая ошибка хендлеров. Каждая ветвь знает свой HTTP-статус и безопасное сообщение;
/// внутренние подробности (SQL, тексты RPC) сюда не утекают.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Нет/невалидная аутентификация → `401`.
    #[error("unauthorized")]
    Unauthorized,
    /// Недостаточно прав → `403`. На нём стоит гейт `RequireOperator` (`/v1/ops/*`).
    #[error("forbidden")]
    Forbidden,
    /// Ресурс не найден (или скрыт проверкой владельца) → `404`.
    #[error("not found")]
    NotFound,
    /// Конфликт состояния (например, дубль) → `409`.
    #[error("conflict")]
    Conflict,
    /// Ошибка валидации входных данных → `422`.
    #[error("validation: {0}")]
    Validation(String),
    /// Превышен лимит → `422`.
    #[error("limit exceeded")]
    LimitExceeded,
    /// Внутренняя ошибка → `500`: причина уходит в лог, наружу — только общий текст.
    #[error("internal error")]
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", self.public_msg()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", self.public_msg()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found", self.public_msg()),
            ApiError::Conflict => (StatusCode::CONFLICT, "conflict", self.public_msg()),
            ApiError::Validation(m) => (StatusCode::UNPROCESSABLE_ENTITY, "validation", m.clone()),
            ApiError::LimitExceeded => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "limit_exceeded",
                self.public_msg(),
            ),
            ApiError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error".to_string(),
            ),
        }
    }

    fn public_msg(&self) -> String {
        match self {
            ApiError::Unauthorized => "authentication required",
            ApiError::Forbidden => "insufficient permissions",
            ApiError::NotFound => "resource not found",
            ApiError::Conflict => "resource conflict",
            ApiError::LimitExceeded => "limit exceeded",
            _ => "error",
        }
        .to_string()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        // Внутренние причины — в логи, не в ответ.
        if let ApiError::Internal(cause) = &self {
            tracing::error!(%cause, "internal error");
        }
        (status, Json(json!({ "code": code, "message": message }))).into_response()
    }
}

/// Маппинг ошибок хранилища. `NotFound`/`Conflict` — ожидаемые;
/// `Backend` — внутренняя (детали в лог). Владение → `404` (не раскрываем существование).
impl From<StorageError> for ApiError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::NotFound => ApiError::NotFound,
            StorageError::Conflict(_) => ApiError::Conflict,
            StorageError::LimitExceeded(_) => ApiError::LimitExceeded,
            StorageError::Backend(cause) => ApiError::Internal(cause),
        }
    }
}

/// Ошибки сети: пользовательские (адрес/средства/dust) → `422`; RPC — внутренняя.
impl From<blockchain::ChainError> for ApiError {
    fn from(e: blockchain::ChainError) -> Self {
        use blockchain::ChainError as E;
        match e {
            E::InvalidAddress(_) => ApiError::Validation("invalid address for chain".into()),
            E::BelowDust => ApiError::Validation("amount below dust limit".into()),
            E::InsufficientFunds => ApiError::Validation("insufficient funds".into()),
            E::Unsupported(_) => ApiError::Validation("chain not supported".into()),
            E::Rpc(cause) => ApiError::Internal(format!("rpc: {cause}")),
        }
    }
}

/// Ошибки подписи: неподдерживаемая сеть → `422`, прочее — внутренняя (детали в лог).
impl From<signing_service::SignerError> for ApiError {
    fn from(e: signing_service::SignerError) -> Self {
        match e {
            signing_service::SignerError::Unsupported(_) => {
                ApiError::Validation("chain not supported".into())
            }
            other => ApiError::Internal(format!("signer: {other}")),
        }
    }
}

impl From<core_domain::AmountError> for ApiError {
    fn from(e: core_domain::AmountError) -> Self {
        ApiError::Internal(format!("amount: {e}"))
    }
}
