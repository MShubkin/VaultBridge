//! Аутентификация и RBAC: JWT-claims (`sub`/`role`/`kid`),
//! проверка токена, экстракторы `AuthUser` и `RequireOperator`, хеширование паролей.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use core_domain::{Role, UserId};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// Полезная нагрузка JWT — то, что лежит внутри токена доступа.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — идентификатор пользователя в виде строки (`UserId`).
    pub sub: String,
    /// Роль доступа.
    pub role: Role,
    /// Время истечения токена (Unix-секунды).
    pub exp: usize,
}

/// Набор ключей для подписи/проверки JWT. `kid` маркирует версию ключа, чтобы можно было
/// ротировать секрет, не инвалидируя разом все живые токены.
pub struct JwtKeys {
    /// Идентификатор версии ключа (кладётся в заголовок токена).
    kid: String,
    /// Ключ для подписи (выпуска) токенов.
    encoding: EncodingKey,
    /// Ключ для проверки токенов.
    decoding: DecodingKey,
    /// Срок жизни выпускаемого токена, секунды.
    ttl_secs: i64,
}

impl JwtKeys {
    pub fn from_secret(kid: impl Into<String>, secret: &[u8], ttl_secs: i64) -> Self {
        Self {
            kid: kid.into(),
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            ttl_secs,
        }
    }

    /// Выпустить access-token для пользователя.
    pub fn issue(&self, user_id: UserId, role: Role) -> Result<String, ApiError> {
        let exp = (time::OffsetDateTime::now_utc().unix_timestamp() + self.ttl_secs) as usize;
        let claims = Claims {
            sub: user_id.to_string(),
            role,
            exp,
        };
        let header = Header {
            kid: Some(self.kid.clone()),
            ..Default::default()
        };
        encode(&header, &claims, &self.encoding)
            .map_err(|e| ApiError::Internal(format!("jwt encode: {e}")))
    }

    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }

    /// Проверить токен и вернуть claims. Любая ошибка → `Unauthorized` (без деталей).
    pub fn verify(&self, token: &str) -> Result<Claims, ApiError> {
        decode::<Claims>(token, &self.decoding, &Validation::default())
            .map(|d| d.claims)
            .map_err(|_| ApiError::Unauthorized)
    }
}

/// Аутентифицированный пользователь, извлечённый из `Authorization: Bearer`.
#[derive(Debug, Clone, Copy)]
pub struct AuthUser {
    pub id: UserId,
    pub role: Role,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer_token(parts).ok_or(ApiError::Unauthorized)?;
        let claims = state.jwt.verify(&token)?;
        let id = Uuid::parse_str(&claims.sub)
            .map(UserId)
            .map_err(|_| ApiError::Unauthorized)?;
        Ok(AuthUser {
            id,
            role: claims.role,
        })
    }
}

/// Гейт операторских роутов: пускает только роль `operator`, иначе `403`.
/// На нём стоят эндпоинты `/v1/ops/*`. Это маркер-экстрактор: ценен сам факт успешной
/// проверки, а не какие-то данные, поэтому тип пустой.
#[derive(Debug, Clone, Copy)]
pub struct RequireOperator;

impl FromRequestParts<AppState> for RequireOperator {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.role != Role::Operator {
            return Err(ApiError::Forbidden);
        }
        Ok(RequireOperator)
    }
}

fn bearer_token(parts: &Parts) -> Option<String> {
    let header = parts.headers.get(axum::http::header::AUTHORIZATION)?;
    let value = header.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

/// Хеширование пароля (argon2). Используется при создании пользователя.
pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(format!("argon2 hash: {e}")))
}

/// Проверка пароля против хеша. Неверный пароль → `false` (не ошибка).
pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let hash = hash_password("s3cret").unwrap();
        assert!(verify_password("s3cret", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn jwt_issue_and_verify() {
        let keys = JwtKeys::from_secret("k1", b"test-secret", 3600);
        let uid = UserId::new();
        let token = keys.issue(uid, Role::Operator).unwrap();
        let claims = keys.verify(&token).unwrap();
        assert_eq!(claims.sub, uid.to_string());
        assert_eq!(claims.role, Role::Operator);
    }

    #[test]
    fn jwt_rejects_tampered_token() {
        let keys = JwtKeys::from_secret("k1", b"test-secret", 3600);
        assert!(keys.verify("not.a.jwt").is_err());
    }
}
