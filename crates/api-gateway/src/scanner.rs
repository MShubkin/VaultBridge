//! Фоновый воркер-реконсилятор (block-scanner).
//!
//! Сага вывода доводит транзакцию до `Unconfirmed` и отдаёт ответ — а дальше её судьбу
//! решает сеть. Воркер периодически спрашивает у адаптера наблюдаемое состояние каждой
//! «живой» транзакции и двигает FSM:
//! - набрала порог подтверждений → `Confirmed`;
//! - провал/истечение/замена → `Failed`/`Expired`/`Replaced`;
//! - реорг (подтверждённая просела по глубине или выпала) → откат в `Unconfirmed`.
//!
//! На каждом переходе сбрасываем кеш баланса, шлём WS-событие и пишем аналитику. Источник
//! истины — БД. Логика отображения «наблюдение → статус» вынесена в чистую [`next_status`]
//! (тестируется без сети), а [`reconcile_once`] применяет её и пишет изменения. Best-effort:
//! ошибки RPC/БД логируются и не валят сервер — повтор на следующем тике.
//!
//! Реорг здесь обрабатывается перепроверкой `Confirmed`-записей: в проде это ограничивают
//! глубиной блока (перепроверять лишь то, что в пределах `reorg_window` от вершины), здесь
//! же перепроверяются все — для портфолио-масштаба объёма это приемлемо.

use std::time::Duration;

use blockchain::TxObservation;
use core_domain::TransactionStatus;
use tokio::time::MissedTickBehavior;

use crate::state::AppState;

/// Куда перевести транзакцию, исходя из текущего статуса и наблюдения сети. `None` —
/// оставить как есть. Чистая функция, вся развилка FSM реконсилятора здесь.
///
/// `needed` — порог подтверждений сети (`None` у Solana: финальность по commitment, поэтому
/// сам факт «видна» уже считается достаточным).
fn next_status(
    current: TransactionStatus,
    obs: TxObservation,
    needed: Option<u32>,
) -> Option<TransactionStatus> {
    use TransactionStatus as S;
    match obs {
        // Пропала из сети. Если была подтверждена — это глубокий реорг/выпадение, откатываем
        // на Unconfirmed (может переподтвердиться). Иначе ещё не видели — ждём дальше.
        TxObservation::NotFound => (current == S::Confirmed).then_some(S::Unconfirmed),
        TxObservation::Pending { confirmations } => {
            let enough = match needed {
                Some(n) => confirmations >= n as u64,
                None => true,
            };
            Some(if enough { S::Confirmed } else { S::Unconfirmed })
        }
        TxObservation::Failed => Some(S::Failed),
        TxObservation::Expired => Some(S::Expired),
        TxObservation::Replaced => Some(S::Replaced),
    }
}

/// Один проход реконсиляции. Возвращает число транзакций, у которых сменился статус.
pub async fn reconcile_once(state: &AppState) -> usize {
    let txs = match state.txs.list_all_outgoing().await {
        Ok(txs) => txs,
        Err(e) => {
            tracing::warn!(error = %e, "scanner: list outgoing failed");
            return 0;
        }
    };

    let mut changed = 0;
    for tx in txs {
        // «Живые» статусы: ещё не финализированные плюс Confirmed (его пересматриваем,
        // чтобы поймать реорг). Терминальные failed/expired/replaced не трогаем.
        if !matches!(
            tx.status,
            TransactionStatus::Broadcast
                | TransactionStatus::Unconfirmed
                | TransactionStatus::Confirmed
        ) {
            continue;
        }
        let Some(hash) = tx.tx_hash.clone() else {
            continue; // нет hash — нечего опрашивать
        };
        let Some(client) = state.chains.get(&tx.chain) else {
            continue; // для этой сети адаптер не сконфигурирован
        };
        // Владелец кошелька нужен и для запроса статуса (адрес-отправитель → детект замены),
        // и дальше для сброса кеша и адресации WS-события.
        let Ok(wallet) = state.wallets.by_id(tx.wallet_id).await else {
            continue;
        };

        let obs = match client
            .tx_status(&hash, &wallet.address, tx.tracking.as_deref())
            .await
        {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!(error = %e, tx = %tx.id, "scanner: status query failed");
                continue; // транзиентная ошибка — повтор на след. тике
            }
        };

        let Some(target) = next_status(tx.status, obs, client.config().confirmations) else {
            continue;
        };
        if target == tx.status {
            continue; // без изменений — не шумим событиями
        }

        let updated = match state
            .txs
            .set_status(tx.id, target, Some(hash.clone()), None)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, tx = %tx.id, "scanner: set_status failed");
                continue;
            }
        };
        changed += 1;
        tracing::info!(tx = %tx.id, status = target.as_str(), "scanner: status changed");

        // Баланс изменился — сбрасываем кеш; событие адресуем владельцу (приватная фильтрация).
        crate::balance::invalidate(state, wallet.chain, &wallet.address).await;
        crate::events::publish(
            state,
            wallet.user_id,
            tx.wallet_id,
            updated.id.to_string(),
            target,
            Some(hash.clone()),
        );

        // Аналитика (best-effort).
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        state
            .analytics
            .record(storage::AnalyticsRecord {
                event: target.as_str().into(),
                chain: tx.chain.as_str().into(),
                direction: "outgoing".into(),
                wallet_id: tx.wallet_id.to_string(),
                tx_id: updated.id.to_string(),
                amount_raw: tx.amount_raw.to_string(),
                status: target.as_str().into(),
                ts,
            })
            .await;
    }
    changed
}

/// Запустить фоновый цикл реконсиляции с заданным интервалом. Tick пропускается, если
/// предыдущий проход затянулся (без накопления «долга», `MissedTickBehavior::Skip`).
pub fn spawn(state: AppState, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let n = reconcile_once(&state).await;
            if n > 0 {
                tracing::info!(changed = n, "scanner: transactions reconciled");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use blockchain::{BlockchainClient, MockChain, TxObservation};
    use core_domain::{Chain, U256};
    use storage::{InMemoryStore, NewOutgoing, NewUser, NewWallet, UserRepository};

    /// Собирает AppState на in-memory + MockChain и возвращает конкретный handle мока,
    /// чтобы тест мог задавать подтверждения через `set_confirmations`.
    fn test_state(mock: Arc<MockChain>) -> AppState {
        let store = Arc::new(InMemoryStore::new());
        let mut chains: HashMap<Chain, Arc<dyn BlockchainClient>> = HashMap::new();
        chains.insert(Chain::Ethereum, mock);
        AppState {
            jwt: Arc::new(crate::auth::JwtKeys::from_secret("k1", b"secret", 3600)),
            users: store.clone(),
            wallets: store.clone(),
            txs: store.clone(),
            signer: Arc::new(
                signing_service::LocalSigner::from_mnemonic(
                    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                    "",
                )
                .unwrap(),
            ),
            chains: Arc::new(chains),
            cache: Arc::new(storage::InMemoryBalanceCache::new()),
            audit: store.clone(),
            analytics: Arc::new(storage::InMemoryAnalytics::new()),
            kyc: Arc::new(kyc_aml::MockKyc::new(core_domain::KycStatus::Approved)),
            aml: Arc::new(kyc_aml::InMemoryBlacklist::new()),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            events: crate::events::channel(),
            locks: Arc::new(storage::InMemoryWalletLock::new()),
            idempotency: crate::idempotency::in_memory(),
            config: crate::state::Config::default(),
        }
    }

    async fn seed_wallet_and_tx(
        state: &AppState,
    ) -> (core_domain::WalletId, core_domain::TransactionId) {
        let user = UserRepository::create(
            &*state.users,
            NewUser {
                email: "u@vaultbridge.dev".into(),
                password_hash: "x".into(),
                role: core_domain::Role::User,
            },
        )
        .await
        .unwrap();
        let wallet = state
            .wallets
            .create(
                NewWallet {
                    user_id: user.id,
                    chain: Chain::Ethereum,
                    address: "0x1111111111111111111111111111111111111111".into(),
                    derivation_path: "m/44'/60'/0'/0/0".into(),
                },
                10,
            )
            .await
            .unwrap();
        let tx = state
            .txs
            .create_outgoing(NewOutgoing {
                wallet_id: wallet.id,
                chain: Chain::Ethereum,
                to_address: "0x2222222222222222222222222222222222222222".into(),
                amount_raw: U256::from(1000u64),
                idempotency_key: "key-1".into(),
            })
            .await
            .unwrap();
        (wallet.id, tx.id)
    }

    #[tokio::test]
    async fn confirms_when_threshold_reached() {
        let mock = Arc::new(MockChain::ethereum()); // confirmations: Some(3)
        let state = test_state(mock.clone());
        let (_wallet, tx_id) = seed_wallet_and_tx(&state).await;

        // Переводим в Unconfirmed с известным hash (как делает сага после broadcast).
        let hash = "0xabc123";
        state
            .txs
            .set_status(
                tx_id,
                TransactionStatus::Unconfirmed,
                Some(hash.into()),
                None,
            )
            .await
            .unwrap();

        // Меньше порога (нужно 3) — статус не двигается.
        mock.set_confirmations(hash, 1);
        assert_eq!(reconcile_once(&state).await, 0);
        assert_eq!(
            state.txs.get(tx_id).await.unwrap().status,
            TransactionStatus::Unconfirmed
        );

        // Достигли порога — переходит в Confirmed.
        mock.set_confirmations(hash, 3);
        assert_eq!(reconcile_once(&state).await, 1);
        assert_eq!(
            state.txs.get(tx_id).await.unwrap().status,
            TransactionStatus::Confirmed
        );

        // Идемпотентность прохода: confirmed-транзакция больше не трогается.
        assert_eq!(reconcile_once(&state).await, 0);
    }

    #[tokio::test]
    async fn skips_when_not_yet_seen() {
        let mock = Arc::new(MockChain::ethereum());
        let state = test_state(mock.clone());
        let (_wallet, tx_id) = seed_wallet_and_tx(&state).await;
        state
            .txs
            .set_status(
                tx_id,
                TransactionStatus::Unconfirmed,
                Some("0xnope".into()),
                None,
            )
            .await
            .unwrap();
        // Hash не засеян в моке → NotFound → пропуск (ждём дальше).
        assert_eq!(reconcile_once(&state).await, 0);
        assert_eq!(
            state.txs.get(tx_id).await.unwrap().status,
            TransactionStatus::Unconfirmed
        );
    }

    /// Завести транзакцию и перевести её в Unconfirmed с заданным hash (как после саги).
    async fn seed_unconfirmed(state: &AppState, hash: &str) -> core_domain::TransactionId {
        let (_wallet, tx_id) = seed_wallet_and_tx(state).await;
        state
            .txs
            .set_status(
                tx_id,
                TransactionStatus::Unconfirmed,
                Some(hash.into()),
                None,
            )
            .await
            .unwrap();
        tx_id
    }

    async fn status_of(state: &AppState, tx_id: core_domain::TransactionId) -> TransactionStatus {
        state.txs.get(tx_id).await.unwrap().status
    }

    #[tokio::test]
    async fn maps_failed_expired_replaced() {
        for (obs, expected) in [
            (TxObservation::Failed, TransactionStatus::Failed),
            (TxObservation::Expired, TransactionStatus::Expired),
            (TxObservation::Replaced, TransactionStatus::Replaced),
        ] {
            let mock = Arc::new(MockChain::ethereum());
            let state = test_state(mock.clone());
            let tx_id = seed_unconfirmed(&state, "0xdead").await;
            mock.set_observation("0xdead", obs);

            assert_eq!(reconcile_once(&state).await, 1);
            assert_eq!(status_of(&state, tx_id).await, expected);
            // Терминальный статус — повторный проход его не трогает.
            assert_eq!(reconcile_once(&state).await, 0);
        }
    }

    #[tokio::test]
    async fn reorg_rolls_confirmed_back_to_unconfirmed() {
        let mock = Arc::new(MockChain::ethereum()); // порог 3
        let state = test_state(mock.clone());
        let tx_id = seed_unconfirmed(&state, "0xreorg").await;

        mock.set_confirmations("0xreorg", 3);
        assert_eq!(reconcile_once(&state).await, 1);
        assert_eq!(status_of(&state, tx_id).await, TransactionStatus::Confirmed);

        // Реорг: глубина просела ниже порога → откат на Unconfirmed.
        mock.set_confirmations("0xreorg", 1);
        assert_eq!(reconcile_once(&state).await, 1);
        assert_eq!(
            status_of(&state, tx_id).await,
            TransactionStatus::Unconfirmed
        );
    }

    #[tokio::test]
    async fn reorg_vanished_confirmed_rolls_back() {
        let mock = Arc::new(MockChain::ethereum());
        let state = test_state(mock.clone());
        let tx_id = seed_unconfirmed(&state, "0xgone").await;

        mock.set_confirmations("0xgone", 5);
        assert_eq!(reconcile_once(&state).await, 1);
        assert_eq!(status_of(&state, tx_id).await, TransactionStatus::Confirmed);

        // Транзакция выпала из цепи (NotFound) при глубоком реорге → откат на Unconfirmed.
        mock.set_observation("0xgone", TxObservation::NotFound);
        assert_eq!(reconcile_once(&state).await, 1);
        assert_eq!(
            status_of(&state, tx_id).await,
            TransactionStatus::Unconfirmed
        );
    }
}
