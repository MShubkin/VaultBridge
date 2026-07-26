//! GraphQL `portfolio`: агрегированный баланс пользователя по всем сетям
//! одним запросом. Чтение балансов — через те же `BlockchainClient` из `AppState`.

use async_graphql::{
    Context, EmptyMutation, EmptySubscription, Object, Result as GqlResult, Schema, SimpleObject,
};

use crate::auth::AuthUser;
use crate::state::AppState;

/// Баланс одного кошелька (зеркало `BalanceView`). Поля → camelCase в схеме.
#[derive(SimpleObject)]
pub struct ChainBalance {
    /// Код сети.
    pub chain: String,
    /// Публичный адрес кошелька.
    pub address: String,
    /// Знаков после запятой у монеты.
    pub decimals: i32,
    /// Весь баланс (десятичная строка).
    pub total_raw: String,
    /// Заморожено под операции.
    pub reserved_raw: String,
    /// Доступно к трате.
    pub spendable_raw: String,
}

/// Портфель пользователя: балансы по всем его кошелькам в одном ответе.
#[derive(SimpleObject)]
pub struct Portfolio {
    /// Владелец.
    pub user_id: String,
    /// По одному элементу на кошелёк.
    pub balances: Vec<ChainBalance>,
}

/// Корень GraphQL-запросов. Мутаций и подписок нет — API только на чтение.
pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// Портфель аутентифицированного пользователя (только свои кошельки).
    async fn portfolio(&self, ctx: &Context<'_>) -> GqlResult<Portfolio> {
        let state = ctx.data::<AppState>()?;
        let auth = ctx.data::<AuthUser>()?;

        let wallets = state
            .wallets
            .list_for_user(auth.id)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        let mut balances = Vec::new();
        for w in wallets {
            // Сети без зарегистрированного клиента (нет RPC) — пропускаем.
            if !state.chains.contains_key(&w.chain) {
                continue;
            }
            // Чтение через кеш балансов.
            let b = crate::balance::read_balance(state, w.chain, &w.address)
                .await
                .map_err(|e| async_graphql::Error::new(format!("{e}")))?;
            balances.push(ChainBalance {
                chain: w.chain.to_string(),
                address: w.address,
                decimals: b.decimals as i32,
                total_raw: b.total_raw,
                reserved_raw: b.reserved_raw,
                spendable_raw: b.spendable_raw,
            });
        }
        Ok(Portfolio {
            user_id: auth.id.to_string(),
            balances,
        })
    }
}

/// Готовая схема: только запросы, без мутаций и подписок.
pub type AppSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

/// Собрать схему. `AppState` кладётся в контекст запроса уже в хендлере, а не здесь.
pub fn schema() -> AppSchema {
    Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish()
}
