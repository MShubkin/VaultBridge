//! gRPC-обёртка над [`Signer`]: реализует сгенерированный tonic-сервис, просто делегируя
//! вызовы в крипто-ядро. Это чисто транспортный слой — приватный ключ остаётся в процессе,
//! наружу уходят только адрес и подпись.

use std::sync::Arc;

use core_domain::Chain;
use proto::{
    DeriveAddressRequest, DeriveAddressResponse, SignRequest, SignResponse, SignerRpc, SignerServer,
};
use tonic::{Request, Response, Status};

use crate::Signer;

/// Сервис, оборачивающий любой [`Signer`] (обычно `LocalSigner`) под gRPC.
pub struct SignerService {
    /// Крипто-ядро, куда делегируются вызовы. За `Arc<dyn>`, чтобы подменять реализацию.
    inner: Arc<dyn Signer>,
}

impl SignerService {
    /// Обернуть готовый signer в gRPC-сервис.
    pub fn new(inner: Arc<dyn Signer>) -> Self {
        Self { inner }
    }

    /// Готовый tonic-сервер для монтирования в `Server::builder().add_service(..)`.
    pub fn into_server(self) -> SignerServer<Self> {
        SignerServer::new(self)
    }
}

// `Status` крупный, но это тип ошибки tonic-контракта (его же возвращают методы трейта).
#[allow(clippy::result_large_err)]
fn parse_chain(s: &str) -> Result<Chain, Status> {
    Chain::parse(s).ok_or_else(|| Status::invalid_argument(format!("unknown chain: {s}")))
}

#[tonic::async_trait]
impl SignerRpc for SignerService {
    async fn derive_address(
        &self,
        req: Request<DeriveAddressRequest>,
    ) -> Result<Response<DeriveAddressResponse>, Status> {
        let req = req.into_inner();
        let chain = parse_chain(&req.chain)?;
        let address = self
            .inner
            .derive_address(chain, &req.path)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(DeriveAddressResponse { address }))
    }

    async fn sign(&self, req: Request<SignRequest>) -> Result<Response<SignResponse>, Status> {
        let req = req.into_inner();
        let chain = parse_chain(&req.chain)?;
        let signature = self
            .inner
            .sign(chain, &req.path, &req.payload)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(SignResponse { signature }))
    }
}
