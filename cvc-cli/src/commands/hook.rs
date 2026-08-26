use anyhow::{Context, Result};
use cvc_core::db::CvcStore;
use cvc_core::linker;
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use std::env;
use std::fs;
use std::io::Read;

pub async fn post_commit() -> Result<()> {
    let result = (|| -> Result<()> {
        let current_dir = env::current_dir().context("Failed to get current directory")?;
        run_post_commit_logic(&current_dir)
    })();
    if let Err(e) = result {
        eprintln!("CVC Hook Warning: Failed to link artifacts: {}", e);
    }

    Ok(()) // Explicitly return Ok to ensure we don't fail the hook script
}

/// Pre-push is intentionally advisory: CVC failure must never block a code push.
pub async fn pre_push(remote_name: &str, remote_url: &str) -> Result<()> {
    let input = (|| -> Result<(std::path::PathBuf, Vec<u8>)> {
        let cwd = env::current_dir()?;
        let mut raw = Vec::new();
        std::io::stdin()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut raw)?;
        Ok((cwd, raw))
    })();
    let (cwd, raw) = match input {
        Ok(value) => value,
        Err(error) => {
            eprintln!("CVC: pre-push input skipped ({error})");
            return Ok(());
        }
    };
    if let Err(error) = observe_pre_push(&cwd, remote_name, remote_url, &raw) {
        eprintln!("CVC: range observation skipped ({error})");
    }
    if let Err(error) = crate::commands::sync::auto_push_remote(&cwd, Some(remote_name)).await {
        eprintln!("CVC: sync skipped ({error})");
    }
    Ok(())
}

fn observe_pre_push(
    cwd: &std::path::Path,
    remote: &str,
    remote_url: &str,
    raw: &[u8],
) -> Result<()> {
    if raw.len() > 1024 * 1024 {
        return Ok(());
    }
    if remote.is_empty() || remote_url.is_empty() {
        return Ok(());
    }
    let layout = RepositoryLayout::discover(cwd)?;
    let repo = layout.repository();
    let cvc = layout.cvc_dir();
    if !cvc.exists() {
        return Ok(());
    }
    let store = CvcStore::open_initialized(cvc.join("index.db"))?;
    let text = std::str::from_utf8(raw)?;
    if text.lines().count() > 128 {
        return Ok(());
    }
    let mut tips = Vec::new();
    for line in text.lines().take(129) {
        let cols = line.split(' ').collect::<Vec<_>>();
        if cols.len() != 4
            || !cols[0].starts_with("refs/")
            || !cols[2].starts_with("refs/")
            || ![cols[1], cols[3]].iter().all(|oid| {
                oid.len() == 40
                    && oid
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
        {
            return Ok(());
        }
        if cols[0].starts_with("refs/heads/")
            && cols[1].len() == 40
            && !cols[1].bytes().all(|b| b == b'0')
        {
            tips.push((cols[0], git2::Oid::from_str(cols[1])?));
        }
    }
    if tips.len() != 1 {
        return Ok(());
    }
    let config = repo.config()?;
    let configured = match config.get_string("cvc.targetBranch") {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };
    let resolved = match repo.resolve_reference_from_short_name(configured.trim()) {
        Ok(reference) => reference,
        Err(_) => return Ok(()),
    };
    let target = match resolved.name() {
        Some(name) if name.starts_with("refs/heads/") => name.to_owned(),
        _ => return Ok(()),
    };
    let target_oid = match repo
        .find_reference(&target)
        .and_then(|r| r.peel_to_commit())
    {
        Ok(c) => c.id(),
        Err(_) => return Ok(()),
    };
    let bases = repo.merge_bases(target_oid, tips[0].1)?;
    if bases.len() != 1 {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let _ = cvc_core::squash::observe_pre_push_range_with_abort(
        repo,
        &store,
        bases[0],
        tips[0].1,
        Some(tips[0].0),
        Some(remote),
        None,
        || std::time::Instant::now() >= deadline,
    )?;
    Ok(())
}

pub async fn post_merge(_squash: Option<&str>) -> Result<()> {
    if let Err(e) = crate::commands::sync::pull().await {
        eprintln!("CVC: post-merge pull skipped ({e})");
    }
    let result = (|| -> Result<()> {
        let cwd = env::current_dir()?;
        let layout = RepositoryLayout::discover(&cwd)?;
        let repo = layout.repository();
        let path = layout.db_path();
        let mut store = CvcStore::open_initialized(path)?;
        cvc_core::squash::scan_for(repo, &mut store, false, std::time::Duration::from_secs(30))?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("CVC: squash scan skipped ({e})");
    }
    Ok(())
}

fn replay_inbox(repo: &Repository, store: &mut CvcStore, inbox: &std::path::Path) -> Result<()> {
    if inbox.exists() {
        let mut entries = fs::read_dir(inbox)?.collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) == Some("json") {
                if let Err(error) = cvc_core::rewrite::apply_inbox(repo, store, &path) {
                    if error.is_permanent() {
                        eprintln!(
                            "CVC Hook Warning: quarantining invalid rewrite inbox entry: {error}"
                        );
                        cvc_core::rewrite::quarantine_inbox(&path)?;
                    } else {
                        eprintln!("CVC Hook Warning: rewrite inbox retry deferred: {error}");
                    }
                }
            }
        }
    }
    Ok(())
}

/// Always succeeds: Git must never be blocked by CVC provenance capture.
pub async fn post_rewrite(mode: &str) -> Result<()> {
    let result = (|| -> Result<()> {
        let cwd = env::current_dir()?;
        let mut raw = Vec::new();
        std::io::stdin()
            .take((cvc_core::rewrite::MAX_REWRITE_BYTES + 1) as u64)
            .read_to_end(&mut raw)?;
        let layout = RepositoryLayout::discover(&cwd)?;
        let repo = layout.repository();
        let dir = layout.cvc_dir();
        if !dir.exists() {
            return Ok(());
        }
        let inbox = dir.join("rewrite-inbox");
        // Validate and durably enqueue current input before touching SQLite.
        cvc_core::rewrite::validate_initial_delivery(repo, mode, &raw)?;
        cvc_core::rewrite::persist_inbox(&inbox, mode, &raw)?;
        let mut store = CvcStore::open_initialized(dir.join("index.db"))?;
        replay_inbox(repo, &mut store, &inbox)?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("CVC Hook Warning: post-rewrite skipped: {e}");
    }
    Ok(())
}

fn run_post_commit_logic(current_dir: &std::path::Path) -> Result<()> {
    let layout =
        RepositoryLayout::discover(current_dir).context("Failed to discover Git repository")?;
    let repo = layout.repository();
    let cvc_dir = layout.cvc_dir();
    if !cvc_dir.exists() {
        // Not initialized, do nothing
        return Ok(());
    }

    let db_path = cvc_dir.join("index.db");
    let mut store = CvcStore::open_initialized(&db_path)?; // Hook caller still swallows all errors.

    let count = linker::link_current_commit_to_floating_nodes(repo, &store)?;

    if count > 0 {
        println!("CVC: Linked {} thought(s) to this commit.", count);
    }
    let exact =
        cvc_core::squash::scan_for(repo, &mut store, true, std::time::Duration::from_secs(5))?;
    if exact > 0 {
        println!("CVC: Exactly relinked {exact} squashed thought(s).");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::observe_pre_push;
    use git2::{Repository, Signature};
    use tempfile::TempDir;
    fn commit(
        repo: &Repository,
        reference: &str,
        parent: Option<git2::Oid>,
        body: &[u8],
    ) -> anyhow::Result<git2::Oid> {
        let mut b = repo.treebuilder(None)?;
        b.insert("a", repo.blob(body)?, git2::FileMode::Blob.into())?;
        let tree = repo.find_tree(b.write()?)?;
        let sig = Signature::now("test", "test@example.test")?;
        let parent = parent.map(|oid| repo.find_commit(oid)).transpose()?;
        let parents: Vec<_> = parent.iter().collect();
        Ok(repo.commit(Some(reference), &sig, &sig, "test", &tree, &parents)?)
    }
    fn setup() -> anyhow::Result<(TempDir, Repository, git2::Oid, git2::Oid)> {
        let temp = TempDir::new()?;
        let repo = Repository::init(temp.path())?;
        std::fs::create_dir_all(repo.path().join("cvc"))?;
        let base = commit(&repo, "refs/heads/main", None, b"base")?;
        let tip = commit(&repo, "refs/heads/feature", Some(base), b"feature")?;
        Ok((temp, repo, base, tip))
    }
    #[test]
    fn explicit_local_target_is_trusted_but_remote_head_and_malformed_are_not() -> anyhow::Result<()>
    {
        for configured in [true, false] {
            let (temp, repo, base, tip) = setup()?;
            if configured {
                repo.config()?
                    .set_str("cvc.targetBranch", "refs/heads/main")?;
            } else {
                repo.reference("refs/remotes/origin/main", base, true, "test")?;
                repo.reference_symbolic(
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                    true,
                    "test",
                )?;
            }
            let raw = format!(
                "refs/heads/feature {tip} refs/heads/feature {}\n",
                "0".repeat(40)
            );
            observe_pre_push(
                temp.path(),
                "origin",
                "https://example.test/repo.git",
                raw.as_bytes(),
            )?;
            observe_pre_push(
                temp.path(),
                "origin",
                "https://example.test/repo.git",
                b"malformed\n",
            )?;
            let db = rusqlite::Connection::open(repo.path().join("cvc/index.db"))?;
            let trusted: i64 = db.query_row(
                "SELECT COUNT(*) FROM range_observations WHERE trusted_local=1",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(trusted, configured as i64);
        }
        Ok(())
    }
}
