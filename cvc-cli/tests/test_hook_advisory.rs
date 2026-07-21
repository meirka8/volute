use std::process::Command;

#[test]
fn malformed_hook_invocation_is_advisory_even_before_dispatch() {
    let status = Command::new(env!("CARGO_BIN_EXE_cvc"))
        .args(["hook", "pre-push"])
        .status()
        .expect("run cvc hook command");
    assert!(status.success(), "Clap failure must not block Git");
}
