//! Strict post-rewrite ingestion.  This module deliberately has no shell-out:
//! Git reachability is checked through libgit2 and persistence through CvcStore.
use crate::db::{CvcStore, DbError};
use crate::{
    CommitSha, DerivationEvent, DerivationOrigin, DerivationRelation, Evidence, EvidenceKind,
};
use git2::{Oid, Repository};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use thiserror::Error;

pub const MAX_REWRITE_BYTES: usize = 1024 * 1024;
pub const MAX_REWRITE_PAIRS: usize = 10_000;
const MAX_INBOX_FILES: usize = 64;
const MAX_INBOX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_QUARANTINE_FILES: usize = 32;
const MAX_QUARANTINE_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteMode {
    Amend,
    Rebase,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewritePair {
    pub old: CommitSha,
    pub new: CommitSha,
}
#[derive(Error, Debug)]
pub enum RewriteError {
    #[error("invalid post-rewrite input: {0}")]
    Permanent(&'static str),
    #[error("retryable post-rewrite operation: {0}")]
    Retryable(&'static str),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("db: {0}")]
    Db(#[from] DbError),
}

impl RewriteError {
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }
}

fn valid_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && s.bytes().any(|b| b != b'0')
}
pub fn parse(mode: &str, input: &[u8]) -> Result<(RewriteMode, Vec<RewritePair>), RewriteError> {
    let mode = match mode {
        "amend" => RewriteMode::Amend,
        "rebase" => RewriteMode::Rebase,
        _ => return Err(RewriteError::Permanent("mode must be amend or rebase")),
    };
    if input.len() > MAX_REWRITE_BYTES
        || input.is_empty()
        || !input.ends_with(b"\n")
        || input.contains(&b'\r')
        || input.contains(&0)
    {
        return Err(RewriteError::Permanent("input limit or forbidden byte"));
    }
    let text = std::str::from_utf8(input).map_err(|_| RewriteError::Permanent("not utf8"))?;
    let mut map = BTreeMap::new();
    for line in text[..text.len() - 1].split('\n') {
        if line.is_empty() {
            return Err(RewriteError::Permanent("blank line"));
        }
        let mut columns = line.split(' ');
        let old = columns.next().unwrap_or("");
        let new = columns.next().unwrap_or("");
        if columns.next().is_some() || !valid_oid(old) || !valid_oid(new) || old.len() != new.len()
        {
            return Err(RewriteError::Permanent(
                "expected two lowercase full matching-width oids",
            ));
        }
        if old == new {
            return Err(RewriteError::Permanent("old and new oid must differ"));
        }
        if map.insert(old.to_owned(), new.to_owned()).is_some() {
            return Err(RewriteError::Permanent("duplicate old oid"));
        }
        if map.len() > MAX_REWRITE_PAIRS {
            return Err(RewriteError::Permanent("too many pairs"));
        }
    }
    let pairs: Vec<_> = map
        .into_iter()
        .map(|(old, new)| RewritePair {
            old: CommitSha::new(old),
            new: CommitSha::new(new),
        })
        .collect();
    if mode == RewriteMode::Amend && pairs.len() != 1 {
        return Err(RewriteError::Permanent("amend requires exactly one pair"));
    }
    Ok((mode, pairs))
}

pub fn apply(
    repo: &Repository,
    store: &mut CvcStore,
    mode: &str,
    raw: &[u8],
) -> Result<usize, RewriteError> {
    let (kind, pairs) = parse(mode, raw)?;
    let head = repo.head()?.peel_to_commit()?;
    let mut events = Vec::new();
    let mut snapshots = Vec::new();
    for pair in &pairs {
        let new = repo.find_commit(Oid::from_str(pair.new.as_str())?)?;
        if !repo.graph_descendant_of(head.id(), new.id())? && head.id() != new.id() {
            return Err(RewriteError::Permanent(
                "new commit is not reachable from HEAD",
            ));
        }
        // old intentionally need not resolve: rebases commonly prune it before hook execution.
        for interaction in store.get_interactions_for_commit(&pair.old)? {
            if !store.has_locally_observed_source(&interaction.id, &pair.old)? {
                continue;
            }
            let sources = store.rewrite_source_snapshots(&interaction.id, &pair.old)?;
            for source in &sources {
                let source_id = match source {
                    crate::SourceSnapshot::Legacy {
                        interaction,
                        commit,
                        ..
                    } => {
                        format!("legacy:{interaction}:{commit}")
                    }
                    crate::SourceSnapshot::Event { event_id, .. } => event_id.clone(),
                };
                let mut e = DerivationEvent {
                    event_id: String::new(),
                    interaction_id: interaction.id.clone(),
                    target_commit: pair.new.clone(),
                    relation: DerivationRelation::RewriteExact,
                    evidence: Evidence {
                        version: 1,
                        kind: EvidenceKind::LocallyObserved,
                    },
                    origin: DerivationOrigin::LocalHook,
                    source_event_ids: vec![source_id],
                    old_oid: Some(pair.old.clone()),
                    new_oid: Some(pair.new.clone()),
                    range_id: None,
                    linked_by: None,
                };
                e.event_id = e.canonical_id();
                events.push(e);
            }
            snapshots.extend(sources);
        }
    }
    let mut h = Sha256::new();
    h.update(raw);
    let hash = format!("{:x}", h.finalize());
    let mut b = Sha256::new();
    b.update(b"cvc.rewrite-batch/v1\0");
    b.update(mode.as_bytes());
    b.update(raw);
    let id = format!("{:x}", b.finalize());
    store.apply_rewrite_events(
        &id,
        match kind {
            RewriteMode::Amend => "amend",
            RewriteMode::Rebase => "rebase",
        },
        &hash,
        &events,
        &snapshots,
    )?;
    Ok(events.len())
}

/// Validate a first delivery against the HEAD observed by the hook. Replays do
/// not use this check: a user may legitimately switch branches before recovery.
pub fn validate_initial_delivery(
    repo: &Repository,
    mode: &str,
    raw: &[u8],
) -> Result<(), RewriteError> {
    let (_, pairs) = parse(mode, raw)?;
    let head = repo.head()?.peel_to_commit()?;
    for pair in pairs {
        let new = repo.find_commit(Oid::from_str(pair.new.as_str())?)?;
        if head.id() != new.id() && !repo.graph_descendant_of(head.id(), new.id())? {
            return Err(RewriteError::Permanent(
                "new commit is not reachable from HEAD",
            ));
        }
    }
    Ok(())
}

/// Persist only pre-validated input.  The rename makes a complete batch visible
/// atomically; mode 0600 prevents rewrite history leaking through the inbox.
pub fn persist_inbox(dir: &Path, mode: &str, raw: &[u8]) -> Result<PathBuf, RewriteError> {
    parse(mode, raw)?;
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(RewriteError::Permanent("unsafe inbox directory"));
    }
    let mut h = Sha256::new();
    h.update(b"cvc.rewrite-inbox/v1\0");
    h.update(mode.as_bytes());
    h.update(raw);
    let key = format!("{:x}", h.finalize());
    let path = dir.join(format!("{key}.json"));
    let mut intended = Vec::with_capacity(mode.len() + raw.len() + 1);
    intended.extend_from_slice(mode.as_bytes());
    intended.push(b'\n');
    intended.extend_from_slice(raw);
    // Idempotency precedes quota accounting: a full inbox must still accept an
    // exact replay of an already durable authoritative batch.
    if path.exists() {
        ensure_private_regular_file(&path)?;
        if fs::read(&path)? != intended {
            return Err(RewriteError::Permanent("inbox identity collision"));
        }
        return Ok(path);
    }
    let entries = fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    // Abandoned temporary files are not active quota. Remove only old files so
    // concurrent writers retain their atomic publication window.
    let mut removed_temp = false;
    for entry in &entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name.ends_with(".tmp") {
            let stale = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > Duration::from_secs(3600));
            if stale && fs::remove_file(entry.path()).is_ok() {
                removed_temp = true;
            }
        }
    }
    #[cfg(unix)]
    if removed_temp {
        OpenOptions::new()
            .read(true)
            .open(dir)
            .and_then(|directory| directory.sync_all())?;
    }
    let active: Vec<_> = entries
        .iter()
        .filter(|entry| entry.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    let bytes = active
        .iter()
        .map(|entry| entry.metadata().map(|m| m.len()))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .sum::<u64>();
    if active.len() >= MAX_INBOX_FILES
        || bytes.saturating_add(intended.len() as u64) > MAX_INBOX_BYTES
    {
        return Err(RewriteError::Retryable("rewrite inbox quota exceeded"));
    }
    let tmp = dir.join(format!(".{key}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut f = match options.open(&tmp) {
        Ok(file) => file,
        // Another hook can publish the same deterministic inbox concurrently.
        // Its completed file is safe to replay; do not turn that benign race
        // into loss of post-rewrite provenance.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.exists() => {
            ensure_private_regular_file(&path)?;
            return Ok(path);
        }
        Err(error) => return Err(error.into()),
    };
    f.write_all(mode.as_bytes())
        .and_then(|_| f.write_all(b"\n"))
        .and_then(|_| f.write_all(raw))
        .and_then(|_| f.sync_all())?;
    fs::rename(&tmp, &path)?;
    // Durability requires the directory entry, not just the file data.
    #[cfg(unix)]
    {
        OpenOptions::new()
            .read(true)
            .open(dir)
            .and_then(|d| d.sync_all())?;
    }
    Ok(path)
}

/// Move a permanently malformed active entry into independently bounded
/// retention. Quarantine can never consume active inbox quota.
pub fn quarantine_inbox(path: &Path) -> Result<PathBuf, RewriteError> {
    let parent = path
        .parent()
        .ok_or(RewriteError::Permanent("bad inbox path"))?;
    ensure_private_regular_file(path)?;
    let quarantine = parent.join("quarantine");
    fs::create_dir_all(&quarantine)?;
    #[cfg(unix)]
    fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700))?;
    let destination = quarantine.join(
        path.file_name()
            .ok_or(RewriteError::Permanent("bad inbox path"))?,
    );
    fs::rename(path, &destination)?;
    let mut entries = fs::read_dir(&quarantine)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| {
        entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
    });
    let mut bytes: u64 = entries
        .iter()
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum();
    while entries.len() > MAX_QUARANTINE_FILES || bytes > MAX_QUARANTINE_BYTES {
        let oldest = entries.remove(0);
        let len = oldest.metadata().map(|m| m.len()).unwrap_or(0);
        fs::remove_file(oldest.path())?;
        bytes = bytes.saturating_sub(len);
    }
    #[cfg(unix)]
    OpenOptions::new()
        .read(true)
        .open(&quarantine)
        .and_then(|directory| directory.sync_all())
        .and_then(|_| OpenOptions::new().read(true).open(parent)?.sync_all())?;
    Ok(destination)
}
pub fn apply_inbox(
    repo: &Repository,
    store: &mut CvcStore,
    path: &Path,
) -> Result<usize, RewriteError> {
    let meta = ensure_private_regular_file(path)?;
    if meta.len() as usize > MAX_REWRITE_BYTES + 32 {
        return Err(RewriteError::Permanent("unsafe inbox"));
    }
    let data = fs::read(path)?;
    let split = data
        .iter()
        .position(|b| *b == b'\n')
        .ok_or(RewriteError::Permanent("bad inbox"))?;
    let mode = std::str::from_utf8(&data[..split])
        .map_err(|_| RewriteError::Permanent("bad inbox mode"))?;
    // Replay deliberately does not require HEAD reachability; the payload was
    // checked at first delivery and DB source checks remain transactional.
    let result = apply_replay(repo, store, mode, &data[split + 1..])?;
    fs::remove_file(path)?;
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            OpenOptions::new()
                .read(true)
                .open(parent)
                .and_then(|d| d.sync_all())?;
        }
    }
    Ok(result)
}

fn apply_replay(
    repo: &Repository,
    store: &mut CvcStore,
    mode: &str,
    raw: &[u8],
) -> Result<usize, RewriteError> {
    let (kind, pairs) = parse(mode, raw)?;
    let mut events = Vec::new();
    let mut snapshots = Vec::new();
    for pair in &pairs {
        repo.find_commit(Oid::from_str(pair.new.as_str())?)?;
        for interaction in store.get_interactions_for_commit(&pair.old)? {
            if !store.has_locally_observed_source(&interaction.id, &pair.old)? {
                continue;
            }
            let sources = store.rewrite_source_snapshots(&interaction.id, &pair.old)?;
            for source in &sources {
                let source_id = match source {
                    crate::SourceSnapshot::Legacy {
                        interaction,
                        commit,
                        ..
                    } => {
                        format!("legacy:{interaction}:{commit}")
                    }
                    crate::SourceSnapshot::Event { event_id, .. } => event_id.clone(),
                };
                let mut e = DerivationEvent {
                    event_id: String::new(),
                    interaction_id: interaction.id.clone(),
                    target_commit: pair.new.clone(),
                    relation: DerivationRelation::RewriteExact,
                    evidence: Evidence {
                        version: 1,
                        kind: EvidenceKind::LocallyObserved,
                    },
                    origin: DerivationOrigin::LocalHook,
                    source_event_ids: vec![source_id],
                    old_oid: Some(pair.old.clone()),
                    new_oid: Some(pair.new.clone()),
                    range_id: None,
                    linked_by: None,
                };
                e.event_id = e.canonical_id();
                events.push(e);
            }
            snapshots.extend(sources);
        }
    }
    let mut h = Sha256::new();
    h.update(raw);
    let hash = format!("{:x}", h.finalize());
    let mut b = Sha256::new();
    b.update(b"cvc.rewrite-batch/v1\0");
    b.update(mode.as_bytes());
    b.update(raw);
    let id = format!("{:x}", b.finalize());
    store.apply_rewrite_events(
        &id,
        match kind {
            RewriteMode::Amend => "amend",
            RewriteMode::Rebase => "rebase",
        },
        &hash,
        &events,
        &snapshots,
    )?;
    Ok(events.len())
}

fn ensure_private_regular_file(path: &Path) -> Result<fs::Metadata, RewriteError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(RewriteError::Permanent("unsafe inbox"));
    }
    #[cfg(unix)]
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(RewriteError::Permanent("inbox is not private"));
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn parser_requires_exact_git_record_framing() {
        assert!(parse("amend", format!("{A} {B}\n").as_bytes()).is_ok());
        assert!(parse(
            "amend",
            b"a0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
        )
        .is_ok());
        for raw in [
            format!("{A} {B}"),
            format!("{A}  {B}\n"),
            format!("{A}\t{B}\n"),
            format!("{A} {B}\n\n"),
            format!("{A} {A}\n"),
            format!("{} {B}\n", "0".repeat(40)),
            format!("{A} {B}\n{A} {B}\n"),
        ] {
            assert!(parse("rebase", raw.as_bytes()).is_err(), "accepted {raw:?}");
        }
        assert!(parse("rebase", b"").is_err());
        assert!(parse("amend", b"\xff\n").is_err());
        assert!(parse("amend", format!("{} {B}\n", "A".repeat(40)).as_bytes()).is_err());
        assert!(parse("fixup", format!("{A} {B}\n").as_bytes()).is_err());
    }

    #[test]
    fn only_typed_validation_errors_are_permanent() {
        assert!(parse("fixup", format!("{A} {B}\n").as_bytes())
            .expect_err("invalid mode")
            .is_permanent());
        let missing = std::path::Path::new("/definitely/missing/cvc-inbox-entry");
        let error = ensure_private_regular_file(missing).expect_err("missing file");
        assert!(!error.is_permanent(), "I/O failures remain retryable");
        assert!(!RewriteError::Retryable("quota").is_permanent());
    }
}
