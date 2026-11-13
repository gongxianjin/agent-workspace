use std::process::Command;

#[test]
fn help_lists_controller_entrypoint_with_once_option() {
    let output = Command::new(env!("CARGO_BIN_EXE_aw-cli"))
        .arg("--help")
        .output()
        .expect("aw-cli --help should start");

    assert!(
        output.status.success(),
        "aw-cli --help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let help = String::from_utf8(output.stdout).expect("help output should be UTF-8");
    assert!(
        help.contains("aw-cli controller --config FILE [--once]"),
        "help should list the controller entrypoint, got:\n{help}"
    );
    assert!(
        help.contains("aw-cli --identity"),
        "help should document the sibling identity probe used by the GUI, got:\n{help}"
    );
    assert!(help.contains("aw-cli ssh-gateway --config FILE --request FILE --evidence-dir DIR"), "missing SSH Gateway CLI: {help}");
    assert!(
        help.contains("aw-cli owner-exec --config FILE"),
        "help should document the native owner execution entrypoint, got:\n{help}"
    );
}
