#[cfg(unix)]
#[test]
fn installed_pre_push_passes_git_two_args_and_never_blocks() -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use std::process::{Command, Stdio};
    let temp = tempfile::TempDir::new()?;
    git2::Repository::init(temp.path())?;
    cvc_core::hooks::install(temp.path())?;
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin)?;
    symlink(env!("CARGO_BIN_EXE_cvc"), bin.join("cvc"))?;
    let hook = temp.path().join(".git/hooks/pre-push");
    let mut child = Command::new(hook)
        .current_dir(temp.path())
        .arg("origin")
        .arg("https://example.invalid/repo.git")
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"malformed pre-push input\n")?;
    assert!(child.wait()?.success());
    Ok(())
}
