//! `cvc ingest`: record an agent-harness session transcript locally.
//!
//! Not to be confused with sync ingestion (`cvc pull`), which imports another
//! machine's already-shared projection: this is first-party local capture.
use anyhow::{anyhow, bail, Context, Result};
use cvc_core::claude_code::{ingest, IngestMode, IngestReport};
use cvc_core::db::CvcStore;
use cvc_core::privacy::PreparedPolicy;
use serde::Deserialize;
use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_HOOK_PAYLOAD_BYTES: u64 = 1024 * 1024;

/// The subset of Claude Code's hook payload this command relies on.
#[derive(Deserialize)]
struct HookPayload {
    session_id: Option<String>,
    transcript_path: Option<String>,
    cwd: Option<String>,
    hook_event_name: Option<String>,
    agent_id: Option<String>,
}

pub async fn claude_code(
    transcript: Option<PathBuf>,
    session: Option<String>,
    hook: bool,
) -> Result<()> {
    if hook {
        return run_hook();
    }
    let Some(transcript) = transcript else {
        bail!("pass --transcript <path> (optionally --session <id>), or --hook to read a Claude Code hook payload from stdin");
    };
    let current_dir = env::current_dir()?;
    // An operator-driven read treats the transcript as complete: it is the
    // catch-up path for finished sessions and for sessions recorded before
    // the hooks were installed.
    let report = run(
        &current_dir,
        &transcript,
        session.as_deref(),
        IngestMode::Final,
    )?;
    println!(
        "CVC: Claude Code session {}: {} new thought(s), {} already recorded, {} pending; cursor at byte {} of {}",
        report.session_id,
        report.inserted,
        report.already_present,
        report.pending_responses,
        report.cursor,
        report.transcript_len
    );
    Ok(())
}

/// Hook mode reads Claude Code's JSON payload from stdin and writes nothing to
/// stdout, which Claude Code would interpret as hook output. Failures exit
/// non-zero so they are visible in the session, and never with status 2, the
/// only exit status Claude Code treats as blocking.
fn run_hook() -> Result<()> {
    let mut raw = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_HOOK_PAYLOAD_BYTES + 1)
        .read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_HOOK_PAYLOAD_BYTES {
        bail!("hook payload exceeds 1 MiB");
    }
    let payload: HookPayload = serde_json::from_slice(&raw)
        .context("hook payload is not the JSON Claude Code delivers on stdin")?;
    if payload.agent_id.is_some() {
        // Subagent hooks receive the parent session's transcript path; the
        // parent session's own hooks ingest it.
        return Ok(());
    }
    let session = payload
        .session_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("hook payload has no session_id"))?;
    let transcript = payload
        .transcript_path
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("hook payload has no transcript_path"))?;
    let cwd = match payload.cwd.filter(|value| !value.is_empty()) {
        Some(cwd) => PathBuf::from(cwd),
        None => env::current_dir()?,
    };
    let mode = match payload.hook_event_name.as_deref() {
        Some("Stop") | Some("SessionEnd") => IngestMode::Final,
        _ => IngestMode::Incremental,
    };
    run(&cwd, &transcript, Some(&session), mode)?;
    Ok(())
}

fn run(
    repository_path: &Path,
    transcript: &Path,
    session: Option<&str>,
    mode: IngestMode,
) -> Result<IngestReport> {
    let layout = super::harness::discover_initialized_at(repository_path)?;
    super::harness::require_capture_acknowledged(&layout)?;
    let policy = PreparedPolicy::load(layout.policy_root()?)
        .map_err(|error| anyhow!("CVC capture blocked by .thoughtignore: {error}"))?;
    let store = CvcStore::open_initialized(layout.db_path())?;
    Ok(ingest(&layout, &store, &policy, transcript, session, mode)?)
}
