//! Model endpoint configuration contract. TLS connectivity itself runs only
//! in the Linux worker (user-confirmed decision); these tests pin the
//! configuration contract on every platform: scheme, bare-origin URL shape,
//! certificate pin payload, timeout bound, credential absence and the JSON
//! file shape staged for Linux workers.
use agentranch::adapters::model_endpoint::ModelEndpoint;
use serde_json::json;

const CERT_A: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";

fn endpoint() -> ModelEndpoint {
    ModelEndpoint {
        schema: "model-endpoint/1".into(),
        base_url: "https://172.28.8.128:18317".into(),
        model: "gpt-6.1-sol".into(),
        certificate_pem: CERT_A.into(),
        timeout_secs: 90,
        connect_addr: None,
    }
}

#[test]
fn trusted_endpoint_requires_https_and_bare_origin() {
    assert!(endpoint().validate().is_ok(), "explicit https bare origin with pinned certificate is accepted");

    let cases = [
        "http://172.28.8.128:18317",           // plain http
        "https://172.28.8.128:18317/path",     // path
        "https://172.28.8.128:18317?x=1",      // query
        "https://172.28.8.128:18317#f",        // fragment
        "https://user:pw@172.28.8.128:18317",  // userinfo
        "not-a-url",
    ];
    for base_url in cases {
        let mut config = endpoint();
        config.base_url = base_url.into();
        assert!(config.validate().is_err(), "endpoint must be rejected: {base_url}");
    }
}

#[test]
fn trusted_endpoint_pins_certificate_pem_shape() {
    let cases = ["", "not a pem", "-----BEGIN CERTIFICATE-----\ntruncated"];
    for certificate_pem in cases {
        let mut config = endpoint();
        config.certificate_pem = certificate_pem.into();
        assert!(config.validate().is_err(), "certificate pin must be rejected: {certificate_pem:?}");
    }
}

#[test]
fn trusted_endpoint_bounds_model_and_timeout() {
    let mut config = endpoint();
    config.model = String::new();
    assert!(config.validate().is_err(), "empty model rejected");

    let mut config = endpoint();
    config.model = "m".repeat(129);
    assert!(config.validate().is_err(), "oversized model rejected");

    // Upper bound raised 120 -> 240 (linux221 run c0277b6f): a real
    // generation observed at ~117 s left only 3 s of headroom, and the
    // next run exceeded 120 s and failed with "operation timed out".
    // 240 s stays inside the 270 s gateway exchange window (240 + ~10 s
    // owner overhead < 270 s), so a slower model call still returns a
    // terminal verdict instead of the run going transport-unknown.
    for timeout_secs in [0_u64, 4, 29, 241, 999] {
        let mut config = endpoint();
        config.timeout_secs = timeout_secs;
        assert!(config.validate().is_err(), "timeout rejected: {timeout_secs}");
    }
    for timeout_secs in [30_u64, 90, 120, 240] {
        let mut config = endpoint();
        config.timeout_secs = timeout_secs;
        assert!(config.validate().is_ok(), "timeout accepted: {timeout_secs}");
    }
}

#[test]
fn trusted_endpoint_requires_exact_schema() {
    let mut config = endpoint();
    config.schema = "model-endpoint/2".into();
    assert!(config.validate().is_err(), "unknown schema rejected");
}

#[test]
fn trusted_endpoint_never_stores_credentials() {
    let config = endpoint();
    let debug = format!("{config:?}");
    assert!(!debug.to_lowercase().contains("token"), "no token field exists on the endpoint config: {debug}");
    assert!(debug.contains("gpt-6.1-sol"), "non-secret fields remain visible");
}

#[test]
fn trusted_endpoint_rejects_unknown_json_fields() {
    // The staged JSON must carry exactly the contract fields; a smuggled
    // token or extra directive fails closed at parse time.
    let raw = json!({
        "schema": "model-endpoint/1",
        "base_url": "https://172.28.8.128:18317",
        "model": "gpt-6.1-sol",
        "certificate_pem": CERT_A,
        "timeout_secs": 90,
        "token": "should-not-parse"
    });
    let parsed: Result<ModelEndpoint, _> = serde_json::from_value(raw);
    assert!(parsed.is_err(), "unknown fields (token) must be rejected");
}

/// The staged JSON shape carried to Linux workers. Runs on every platform
/// so the Windows manager build pins the same contract it stages.
#[test]
fn model_endpoint_json_shape_round_trips() {
    let raw = json!({
        "schema": "model-endpoint/1",
        "base_url": "https://172.28.8.128:18317",
        "model": "gpt-6.1-sol",
        "certificate_pem": CERT_A,
        "timeout_secs": 90
    });
    let config: ModelEndpoint = serde_json::from_value(raw).expect("valid staged shape parses");
    config.validate().expect("valid staged shape validates");
    assert!(config.certificate_pem.starts_with("-----BEGIN CERTIFICATE-----"));
    // The token is deliberately absent: it travels in-process via stdin
    // frames and environment, never inside any configuration file.
}

/// The connect override contract: optional ip:port literal, IPv4 only, and
/// deliberately absent from legacy staged configs (linux221 run 1152214d
/// topology: DNS-name leaf served only at a staged LAN address).
#[test]
fn connect_override_must_be_ipv4_socket_address() {
    // Absent stays valid (legacy staged configs keep working).
    let mut config = endpoint();
    assert!(config.validate().is_ok(), "absent connect override is valid");
    assert!(config.connect_addr.is_none());

    // Valid IPv4 literal accepted.
    config.connect_addr = Some("172.28.8.128:18317".into());
    assert!(config.validate().is_ok(), "IPv4 ip:port override accepted");

    // Hostnames are refused: an override that needs DNS just moves the
    // unresolvable-name problem; IPv6 is refused to keep the staged shape
    // minimal until a deployment needs it.
    for bad in ["boapi.uno:18317", "[::1]:443", "172.28.8.128", "not an addr", "172.28.8.128:99999"] {
        config.connect_addr = Some(bad.into());
        assert!(config.validate().is_err(), "connect override rejected: {bad}");
    }
}

/// A staged config with a connect override must still round-trip through
/// the exact JSON contract (no extra fields smuggled alongside it).
#[test]
fn connect_override_round_trips_through_json() {
    let raw = json!({
        "schema": "model-endpoint/1",
        "base_url": "https://boapi.uno:18317",
        "model": "gpt-6.1-sol",
        "certificate_pem": CERT_A,
        "timeout_secs": 90,
        "connect_addr": "172.28.8.128:18317"
    });
    let config: ModelEndpoint = serde_json::from_value(raw).expect("staged override shape parses");
    config.validate().expect("staged override shape validates");
    assert_eq!(config.connect_addr.as_deref(), Some("172.28.8.128:18317"));
}
