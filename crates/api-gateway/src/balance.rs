//! Чтение баланса с кешем: read-through с TTL; инвалидация при
//! изменении состояния кошелька. Проверка достаточности средств в саге кеш НЕ использует
//! (берёт свежий баланс — шаг 4).

use core_domain::Chain;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::ApiError;
use crate::state::AppState;

const BALANCE_TTL: Duration = Duration::from_secs(30);

/// Кешируемое представление баланса. Суммы — строки (U256 в десятичном виде), чтобы
/// пережить сериализацию в JSON без потери точности.
#[derive(Clone, Serialize, Deserialize)]
pub struct CachedBalance {
    /// Весь баланс.
    pub total_raw: String,
    /// Заморожено под незавершённые операции.
    pub reserved_raw: String,
    /// Доступно к трате (`total - reserved`).
    pub spendable_raw: String,
    /// Число знаков после запятой у монеты (для отображения).
    pub decimals: u8,
}

/// Ключ кеша по сети и адресу.
fn key(chain: Chain, address: &str) -> String {
    format!("balance:{chain}:{address}")
}

/// Баланс из кеша или из сети (с записью в кеш). Сети без клиента → ошибка.
pub async fn read_balance(
    state: &AppState,
    chain: Chain,
    address: &str,
) -> Result<CachedBalance, ApiError> {
    let k = key(chain, address);
    if let Some(s) = state.cache.get(&k).await {
        if let Ok(cached) = serde_json::from_str::<CachedBalance>(&s) {
            return Ok(cached);
        }
    }
    let client = state
        .chains
        .get(&chain)
        .ok_or_else(|| ApiError::Validation("chain not supported".into()))?;
    let b = client.get_balance(address).await?;
    let cached = CachedBalance {
        total_raw: b.total.raw.to_string(),
        reserved_raw: b.reserved.raw.to_string(),
        spendable_raw: b.spendable.raw.to_string(),
        decimals: client.config().decimals,
    };
    if let Ok(s) = serde_json::to_string(&cached) {
        state.cache.put(&k, s, BALANCE_TTL).await;
    }
    Ok(cached)
}

/// Сбросить кеш баланса кошелька (после исходящей/входящей операции).
pub async fn invalidate(state: &AppState, chain: Chain, address: &str) {
    state.cache.invalidate(&key(chain, address)).await;
}
