use agentranch::{config::Mode, core::service::ServiceOptions};

#[test]
fn service_options_debug_redacts_private_key_bytes() {
    let options = ServiceOptions {
        mode: Mode::Real,
        state_dir: "synthetic-state".into(),
        worker_exe: "synthetic-unused-worker".into(),
        gateway_key: Some(vec![1, 2, 255]),
        gateway_model_token: None,
    };

    let debug = format!("{options:?}");
    assert!(debug.contains("[REDACTED]"), "credential must be redacted");
    assert!(!debug.contains("1, 2, 255"), "private key bytes must not appear");
    assert!(debug.contains("synthetic-state"), "safe diagnostic fields remain available");
    assert_eq!(options.gateway_key.as_deref(), Some(&[1, 2, 255][..]));
}

#[test]
fn service_options_debug_distinguishes_absent_private_key() {
    let options = ServiceOptions {
        mode: Mode::Real,
        state_dir: "synthetic-state".into(),
        worker_exe: "synthetic-unused-worker".into(),
        gateway_key: None, gateway_model_token: None,
    };

    let debug = format!("{options:?}");
    assert!(debug.contains("gateway_key: None, gateway_model_token: None"));
    assert!(!debug.contains("[REDACTED]"));
}
