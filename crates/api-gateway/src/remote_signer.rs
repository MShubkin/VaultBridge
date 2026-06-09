//! gRPC-клиент к signing-service (`RemoteSigner`).
//!
//! Реализует тот же трейт [`Signer`], что и `LocalSigner`, но за подписью ходит по сети
//! поверх **взаимного TLS**: предъявляет клиентский сертификат и проверяет сервер по CA.
//! Хендлеры/сага не знают, локальный это signer или удалённый — выбор делается в
//! `build_state` по env. Так custodial-граница становится сетевой, а не просто типовой.

use async_trait::async_trait;
use core_domain::Chain;
use proto::{DeriveAddressRequest, SignRequest, SignerClient};
use signing_service::{Signer, SignerError};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

/// Удалённый signer поверх tonic-канала. `SignerClient<Channel>` дёшево клонируется
/// (канал мультиплексирует), поэтому клонируем его на каждый вызов для конкурентности.
pub struct RemoteSigner {
    client: SignerClient<Channel>,
}

impl RemoteSigner {
    /// Подключиться к signing-service. `tls = Some(..)` → mTLS; `None` → plaintext (dev).
    pub async fn connect(
        endpoint: &str,
        tls: Option<ClientTlsConfig>,
    ) -> Result<Self, SignerError> {
        let mut ep = Endpoint::from_shared(endpoint.to_string())
            .map_err(|e| SignerError::Remote(format!("bad endpoint: {e}")))?;
        if let Some(tls) = tls {
            ep = ep
                .tls_config(tls)
                .map_err(|e| SignerError::Remote(format!("tls config: {e}")))?;
        }
        let channel = ep
            .connect()
            .await
            .map_err(|e| SignerError::Remote(format!("connect: {e}")))?;
        Ok(Self {
            client: SignerClient::new(channel),
        })
    }
}

#[async_trait]
impl Signer for RemoteSigner {
    async fn derive_address(&self, chain: Chain, path: &str) -> Result<String, SignerError> {
        let mut client = self.client.clone();
        let resp = client
            .derive_address(DeriveAddressRequest {
                chain: chain.as_str().to_string(),
                path: path.to_string(),
            })
            .await
            .map_err(|e| SignerError::Remote(e.to_string()))?;
        Ok(resp.into_inner().address)
    }

    async fn sign(&self, chain: Chain, path: &str, payload: &[u8]) -> Result<Vec<u8>, SignerError> {
        let mut client = self.client.clone();
        let resp = client
            .sign(SignRequest {
                chain: chain.as_str().to_string(),
                path: path.to_string(),
                payload: payload.to_vec(),
            })
            .await
            .map_err(|e| SignerError::Remote(e.to_string()))?;
        Ok(resp.into_inner().signature)
    }
}
