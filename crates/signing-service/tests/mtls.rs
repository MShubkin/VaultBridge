//! Интеграционный тест взаимного TLS между gateway и signing — целиком в одном процессе:
//! поднимаем tonic-сервер `signing-service` с mTLS на loopback, подключаемся сгенерированным
//! gRPC-клиентом с настоящими сертификатами и проверяем подпись.
//!
//! Сертификаты (CA + server + client) генерируются на лету через `rcgen`, поэтому тест
//! самодостаточен и реально выполняет TLS-рукопожатие, а не «проверен компиляцией».

use std::sync::Arc;
use std::time::Duration;

use core_domain::Chain;
use proto::{DeriveAddressRequest, SignRequest, SignerClient};
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair};
use signing_service::{LocalSigner, Signer, SignerService};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint, Server};

const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

struct Pem {
    cert: String,
    key: String,
}

struct Certs {
    ca: String,
    server: Pem,
    client: Pem,
}

/// CA + серверный (SAN localhost, ServerAuth) + клиентский (ClientAuth) сертификаты.
fn generate_certs() -> Certs {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let server_key = KeyPair::generate().unwrap();
    let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    let client_key = KeyPair::generate().unwrap();
    let mut client_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .unwrap();

    Certs {
        ca: ca_cert.pem(),
        server: Pem {
            cert: server_cert.pem(),
            key: server_key.serialize_pem(),
        },
        client: Pem {
            cert: client_cert.pem(),
            key: client_key.serialize_pem(),
        },
    }
}

/// Поднять mTLS-сервер на ephemeral loopback-порту, вернуть его адрес.
async fn spawn_server(certs: &Certs) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let tls = proto::tls::server_config(
        certs.server.cert.as_bytes(),
        certs.server.key.as_bytes(),
        certs.ca.as_bytes(),
    );
    let signer: Arc<dyn Signer> = Arc::new(LocalSigner::from_mnemonic(MNEMONIC, "").unwrap());
    let service = SignerService::new(signer).into_server();

    tokio::spawn(async move {
        Server::builder()
            .tls_config(tls)
            .unwrap()
            .add_service(service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    addr
}

/// Подключиться с заданным TLS-конфигом, с короткими ретраями на старт сервера.
async fn connect(addr: std::net::SocketAddr, tls: ClientTlsConfig) -> Result<Channel, String> {
    let endpoint: Endpoint = format!("https://127.0.0.1:{}", addr.port())
        .parse::<Endpoint>()
        .unwrap()
        .tls_config(tls)
        .map_err(|e| e.to_string())?;
    let mut last = String::new();
    for _ in 0..50 {
        match endpoint.connect().await {
            Ok(ch) => return Ok(ch),
            Err(e) => {
                last = e.to_string();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    Err(last)
}

#[tokio::test]
async fn mtls_sign_and_derive_roundtrip() {
    let certs = generate_certs();
    let addr = spawn_server(&certs).await;

    let client_tls = proto::tls::client_config(
        certs.client.cert.as_bytes(),
        certs.client.key.as_bytes(),
        certs.ca.as_bytes(),
        "localhost",
    );
    let channel = connect(addr, client_tls).await.expect("mTLS connect");
    let mut client = SignerClient::new(channel);

    // DeriveAddress по сети == локальная деривация.
    let remote_addr = client
        .derive_address(DeriveAddressRequest {
            chain: Chain::Ethereum.as_str().into(),
            path: "m/44'/60'/0'/0/0".into(),
        })
        .await
        .expect("derive over mTLS")
        .into_inner()
        .address;
    assert_eq!(remote_addr, "0x9858effd232b4033e47d90003d41ec34ecaeda94");

    // Sign по сети == локальная подпись (ECDSA детерминирована, RFC6979).
    let prehash = vec![0x22u8; 32];
    let remote_sig = client
        .sign(SignRequest {
            chain: Chain::Ethereum.as_str().into(),
            path: "m/44'/60'/0'/0/0".into(),
            payload: prehash.clone(),
        })
        .await
        .expect("sign over mTLS")
        .into_inner()
        .signature;

    let local = LocalSigner::from_mnemonic(MNEMONIC, "").unwrap();
    let local_sig = local
        .sign(Chain::Ethereum, "m/44'/60'/0'/0/0", &prehash)
        .await
        .unwrap();
    assert_eq!(remote_sig, local_sig);
    assert_eq!(remote_sig.len(), 65);
}

#[tokio::test]
async fn server_rejects_client_without_certificate() {
    let certs = generate_certs();
    let addr = spawn_server(&certs).await;

    // Клиент доверяет серверу, но НЕ предъявляет свой сертификат → сервер обязан отказать.
    let tls = ClientTlsConfig::new()
        .ca_certificate(tonic::transport::Certificate::from_pem(certs.ca.as_bytes()))
        .domain_name("localhost");

    let result = async {
        let channel = connect(addr, tls).await?;
        let mut client = SignerClient::new(channel);
        client
            .derive_address(DeriveAddressRequest {
                chain: Chain::Ethereum.as_str().into(),
                path: "m/44'/60'/0'/0/0".into(),
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
    .await;

    assert!(
        result.is_err(),
        "server must reject client without a valid certificate"
    );
}
