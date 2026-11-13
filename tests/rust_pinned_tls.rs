//! Pinned TLS contract for the Linux model endpoint. The live .128:18317
//! endpoint presents a Let's Encrypt **leaf** certificate (CA=false,
//! CN=boapi.uno). Pinning a leaf with a RootCertStore fails closed with
//! UnknownIssuer — a leaf cannot act as its own issuer (linux221 run
//! 72eeb08f). The pinned client must instead compare the presented
//! end-entity certificate byte-for-byte with the staged pin.
//!
//! These tests run a real rustls TLS 1.3 server on 127.0.0.1 presenting the
//! fixture leaf (tests/fixtures/leaf-cert.pem, CA=false, SAN=127.0.0.1) —
//! the same topology as production — and require the pinned client to
//! connect, while refusing a different certificate.
#![cfg(target_os = "linux")]

use ao_reason_workbench::adapters::model_endpoint::ModelEndpoint;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

const CERT: &str = include_str!("fixtures/leaf-cert.pem");
const KEY: &str = include_str!("fixtures/leaf-key.pem");
const OTHER_CERT: &str = include_str!("fixtures/other-leaf-cert.pem");
const OTHER_KEY: &str = include_str!("fixtures/other-leaf-key.pem");
// DNS-name leaf: the boapi.uno production topology (public DNS resolves the
// model domain to an address that does not serve the pinned certificate; the
// pinned certificate is only presented at a staged LAN IP).
const NAME_CERT: &str = include_str!("fixtures/name-leaf-cert.pem");
const NAME_KEY: &str = include_str!("fixtures/name-leaf-key.pem");

fn endpoint(base_url: String, cert_pem: String) -> ModelEndpoint {
    endpoint_with_connect(base_url, cert_pem, None)
}

fn endpoint_with_connect(
    base_url: String,
    cert_pem: String,
    connect_addr: Option<String>,
) -> ModelEndpoint {
    ModelEndpoint {
        schema: "model-endpoint/1".into(),
        base_url,
        model: "gpt-6.1-sol".into(),
        certificate_pem: cert_pem,
        timeout_secs: 30,
        connect_addr,
    }
}

/// Minimal rustls TLS 1.3 server presenting the fixture leaf certificate.
/// Serves one HTTP-ish response then closes; returns the port it bound.
fn spawn_pinned_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let certs = vec![CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap()];
    let key = PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    std::thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        let conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, sock);
        let mut buf = [0_u8; 1024];
        let _ = tls.read(&mut buf);
        let body = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
        let _ = tls.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes());
        let _ = tls.flush();
    });
    port
}

/// The exact production topology: a CA=false leaf certificate is pinned and
/// presented. The pinned client must complete the handshake and read the
/// response. Regression test for linux221 run 72eeb08f (UnknownIssuer).
#[test]
fn pinned_client_accepts_pinned_leaf_certificate() {
    let port = spawn_pinned_server();
    let config = endpoint(format!("https://127.0.0.1:{port}"), CERT.into());
    config.validate().unwrap();
    let client = config.pinned_client().unwrap();
    let response = client
        .post(format!("https://127.0.0.1:{port}/v1/chat/completions"))
        .json(&serde_json::json!({"model": "gpt-6.1-sol", "messages": []}))
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200, "pinned leaf handshake must succeed");
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
}

/// A pinned client configured with one leaf must refuse a server presenting
/// a different leaf: byte-for-byte pinning, not chain-of-trust.
#[test]
fn pinned_client_rejects_unpinned_leaf_certificate() {
    // Different self-signed leaf (generated independently below) presented
    // by the server, while the pin stays the fixture certificate.
    let port = spawn_other_leaf_server();
    let config = endpoint(format!("https://127.0.0.1:{port}"), CERT.into());
    config.validate().unwrap();
    let client = config.pinned_client().unwrap();
    let outcome = client
        .post(format!("https://127.0.0.1:{port}/v1/chat/completions"))
        .json(&serde_json::json!({"model": "gpt-6.1-sol", "messages": []}))
        .send();
    assert!(
        outcome.is_err(),
        "a different leaf certificate must fail closed (got {:?})",
        outcome.map(|r| r.status())
    );
}

/// The presented name must match the URL host: pinning alone does not
/// suspend name verification. (127.0.0.1 fixture SAN covers the positive
/// case above; here the URL lies about the host.)
#[test]
fn pinned_client_rejects_name_mismatch() {
    let port = spawn_pinned_server();
    let config = endpoint(format!("https://127.0.0.1:{port}"), CERT.into());
    let client = config.pinned_client().unwrap();
    let outcome = client
        .post(format!("https://127.0.0.2:{port}/v1/chat/completions"))
        .json(&serde_json::json!({"model": "gpt-6.1-sol", "messages": []}))
        .send();
    assert!(outcome.is_err(), "certificate valid for 127.0.0.1 must not serve 127.0.0.2");
}

fn spawn_other_leaf_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let certs = vec![CertificateDer::from_pem_slice(OTHER_CERT.as_bytes()).unwrap()];
    let key = PrivateKeyDer::from_pem_slice(OTHER_KEY.as_bytes()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    std::thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        let conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, sock);
        let mut buf = [0_u8; 1024];
        let _ = tls.read(&mut buf);
        let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let _ = tls.flush();
    });
    port
}

// ServerName import keeps the pinned-name contract explicit in type space.
#[allow(dead_code)]
fn _name_type_anchor(_n: &ServerName<'_>) {}

/// Minimal rustls TLS 1.3 server presenting the DNS-name fixture leaf.
fn spawn_name_leaf_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let certs = vec![CertificateDer::from_pem_slice(NAME_CERT.as_bytes()).unwrap()];
    let key = PrivateKeyDer::from_pem_slice(NAME_KEY.as_bytes()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    std::thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        let conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, sock);
        let mut buf = [0_u8; 1024];
        let _ = tls.read(&mut buf);
        let body = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
        let _ = tls.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes());
        let _ = tls.flush();
    });
    port
}

/// The linux221 run 1152214d topology: the pinned leaf carries a DNS name
/// (boapi.uno) while the only address presenting it is a staged IP. The
/// endpoint validates a domain URL with an explicit connect override: TLS
/// connects to the staged address but SNI and name verification run against
/// the URL hostname, exactly as a DNS override would behave.
#[test]
fn pinned_client_connects_by_override_and_validates_dns_name() {
    let port = spawn_name_leaf_server();
    let config = endpoint_with_connect(
        format!("https://model.lan:{port}"),
        NAME_CERT.into(),
        Some(format!("127.0.0.1:{port}")),
    );
    config.validate().unwrap();
    let client = config.pinned_client().unwrap();
    let response = client
        .post(format!("https://model.lan:{port}/v1/chat/completions"))
        .json(&serde_json::json!({"model": "gpt-6.1-sol", "messages": []}))
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200, "connect override with name validation must succeed");
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
}

/// The regression that killed run 1152214d: an IP-literal URL against a
/// DNS-only leaf must still fail closed. The fix is the connect override,
/// never a name-verification bypass.
#[test]
fn pinned_client_rejects_ip_url_against_dns_only_leaf() {
    let port = spawn_name_leaf_server();
    let config = endpoint_with_connect(
        format!("https://127.0.0.1:{port}"),
        NAME_CERT.into(),
        None,
    );
    config.validate().unwrap();
    let client = config.pinned_client().unwrap();
    let outcome = client
        .post(format!("https://127.0.0.1:{port}/v1/chat/completions"))
        .json(&serde_json::json!({"model": "gpt-6.1-sol", "messages": []}))
        .send();
    assert!(
        outcome.is_err(),
        "IP-literal URL against a DNS-only leaf must fail closed (got {:?})",
        outcome.map(|r| r.status())
    );
}

/// A connect override is not a name-verification exemption: connecting to
/// the right address with the pinned leaf still fails when the URL hostname
/// is not the certificate's name.
#[test]
fn pinned_client_rejects_name_mismatch_with_connect_override() {
    let port = spawn_name_leaf_server();
    let config = endpoint_with_connect(
        format!("https://other.lan:{port}"),
        NAME_CERT.into(),
        Some(format!("127.0.0.1:{port}")),
    );
    config.validate().unwrap();
    let client = config.pinned_client().unwrap();
    let outcome = client
        .post(format!("https://other.lan:{port}/v1/chat/completions"))
        .json(&serde_json::json!({"model": "gpt-6.1-sol", "messages": []}))
        .send();
    assert!(outcome.is_err(), "connect override must not suspend name verification");
}
