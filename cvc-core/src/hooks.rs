use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn install(repo_root: &Path) -> Result<()> {
    let hooks_dir = repo_root.join(".git").join("hooks");
    if !hooks_dir.exists() {
        fs::create_dir_all(&hooks_dir).context("Failed to create hooks directory")?;
    }

    let post_commit_path = hooks_dir.join("post-commit");
    let hook_cmd = "\n# CVC Hook\ncvc hook post-commit\n";

    if post_commit_path.exists() {
        let content = fs::read_to_string(&post_commit_path)?;
        if !content.contains("cvc hook post-commit") {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(&post_commit_path)?;
            file.write_all(hook_cmd.as_bytes())?;
            println!("Appended CVC hook to existing post-commit");
        } else {
            println!("CVC hook already present in post-commit");
        }
    } else {
        let mut file = fs::File::create(&post_commit_path)?;
        file.write_all(b"#!/bin/sh")?;
        file.write_all(hook_cmd.as_bytes())?;
        println!("Created new post-commit hook");
    }

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&post_commit_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&post_commit_path, perms)?;
    }

    Ok(())
}
