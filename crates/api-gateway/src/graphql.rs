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
    pub chain: String,
    pub address: String,
    pub decimals: i32,
    pub total_raw: String,
    pub reserved_raw: String,
    pub spendable_raw: String,
}

#[derive(SimpleObject)]
pub struct Portfolio {
    pub user_id: String,
    pub balances: Vec<ChainBalance>,
}

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

pub type AppSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

pub fn schema() -> AppSchema {
    Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish()
}
