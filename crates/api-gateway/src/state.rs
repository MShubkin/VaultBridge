//! `AppState`: дёшево клонируется (всё под `Arc`), `Send + Sync`.
//! Репозитории/клиенты — за трейт-объектами, чтобы реальные реализации (Diesel, EVM-RPC,
//! gRPC-signer) подменялись без правок хендлеров.

use std::collections::HashMap;
use std::sync::Arc;

use blockchain::BlockchainClient;
use core_domain::Chain;
use kyc_aml::{AmlScreener, KycProvider};
use signing_service::Signer;
use storage::{
    AnalyticsSink, AuditRepository, BalanceCache, TransactionRepository, UserRepository,
    WalletRepository,
};

use crate::auth::JwtKeys;
use crate::events::WalletEvent;
use crate::idempotency::Idempotency;
use crate::metrics::Metrics;
use storage::WalletLock;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    pub jwt: Arc<JwtKeys>,
    pub users: Arc<dyn UserRepository>,
    pub wallets: Arc<dyn WalletRepository>,
    pub txs: Arc<dyn TransactionRepository>,
    pub signer: Arc<dyn Signer>,
    pub chains: Arc<HashMap<Chain, Arc<dyn BlockchainClient>>>,
    pub cache: Arc<dyn BalanceCache>,
    pub audit: Arc<dyn AuditRepository>,
    pub analytics: Arc<dyn AnalyticsSink>,
    pub kyc: Arc<dyn KycProvider>,
    pub aml: Arc<dyn AmlScreener>,
    pub metrics: Arc<Metrics>,
    pub events: broadcast::Sender<WalletEvent>,
    pub locks: Arc<dyn WalletLock>,
    pub idempotency: Arc<dyn Idempotency>,
    pub config: Config,
}

#[derive(Clone)]
pub struct Config {
    pub max_wallets_per_user: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_wallets_per_user: 10,
        }
    }
}
