//! `--version` and `--help` are output, not errors: they go to stdout and
//! exit 0, which installer smoke tests and shell scripts rely on.
use std::process::Command;

fn cvc(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cvc"))
        .args(args)
        .output()
        .expect("run cvc")
}

#[test]
fn version_and_help_are_successful_stdout_output() {
    let output = cvc(&["--version"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("cvc {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());

    let output = cvc(&["--help"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    assert!(output.stderr.is_empty());

    // A usage error is still a failure, reported on stderr.
    let output = cvc(&["no-such-command"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}
