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
        "CVC: Claude Code session {}: {} new thought(s), {} already recorded, {} pending, {} linked to session commits; cursor at byte {} of {}",
        report.session_id,
        report.inserted,
        report.already_present,
        report.pending_responses,
        report.linked_from_commits,
        report.cursor,
        report.transcript_len
    );
    if report.subagent_sessions > 0 {
        println!(
            "  plus {} subagent session(s): {} new thought(s), {} linked to commits",
            report.subagent_sessions, report.subagent_inserted, report.subagent_linked
        );
    }
    if report.unrecorded_blocks > 0 {
        println!(
            "  {} content block(s) of a type this version does not record were kept as placeholders: {}",
            report.unrecorded_blocks,
            report.unrecorded_block_types.join(", ")
        );
    }
    for line in left_out_lines(&report) {
        println!("{line}");
    }
    Ok(())
}

/// What the capture boundary could not keep, as lines to print, so that a
/// response recorded as a stand-in, or left out, is never silent. None of the
/// text of such a response is in them, only the limit that was hit.
fn left_out_lines(report: &IngestReport) -> Vec<String> {
    let mut lines = Vec::new();
    if report.withheld > 0 || report.lost > 0 {
        let mut parts = Vec::new();
        if report.withheld > 0 {
            parts.push(format!(
                "{} recorded as a stand-in that says its response was withheld",
                report.withheld
            ));
        }
        if report.lost > 0 {
            parts.push(format!("{} not recorded at all", report.lost));
        }
        lines.push(format!(
            "  could not scrub {} response(s) within the limits: {} ({})",
            report.withheld + report.lost,
            parts.join(", "),
            report.withheld_reasons.join("; ")
        ));
    }
    if report.omitted_tool_calls > 0 || report.omitted_context_items > 0 {
        lines.push(format!(
            "  left out of the responses recorded now: {} tool call(s) beyond the 255 a response records, {} context item(s) (a path that cannot be scrubbed, or beyond the 256 a response records); each such response ends with a cvc-omitted record that says so",
            report.omitted_tool_calls, report.omitted_context_items
        ));
    }
    lines
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

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> IngestReport {
        IngestReport {
            session_id: "s".into(),
            inserted: 3,
            already_present: 0,
            pending_responses: 0,
            linked_from_commits: 0,
            cursor: 10,
            transcript_len: 10,
            subagent_sessions: 0,
            subagent_inserted: 0,
            subagent_linked: 0,
            unrecorded_blocks: 0,
            unrecorded_block_types: Vec::new(),
            withheld: 0,
            lost: 0,
            withheld_reasons: Vec::new(),
            omitted_tool_calls: 0,
            omitted_context_items: 0,
        }
    }

    #[test]
    fn a_clean_ingest_prints_nothing_about_what_was_left_out() {
        assert!(left_out_lines(&report()).is_empty());
    }

    #[test]
    fn withheld_lost_and_omitted_responses_are_said_with_their_reasons_and_no_text() {
        let lines = left_out_lines(&IngestReport {
            withheld: 2,
            lost: 1,
            withheld_reasons: vec![
                "capture collection exceeds limit".into(),
                "too many secret matches".into(),
            ],
            omitted_tool_calls: 45,
            omitted_context_items: 44,
            ..report()
        });
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            lines[0],
            "  could not scrub 3 response(s) within the limits: 2 recorded as a stand-in that says its response was withheld, 1 not recorded at all (capture collection exceeds limit; too many secret matches)"
        );
        assert!(lines[1].contains("45 tool call(s)"), "{lines:?}");
        assert!(lines[1].contains("44 context item(s)"), "{lines:?}");
        assert!(lines[1].contains("cvc-omitted"), "{lines:?}");
        // Only counts and limits: never any text of a response.
        let only_omitted = left_out_lines(&IngestReport {
            omitted_context_items: 1,
            ..report()
        });
        assert_eq!(only_omitted.len(), 1);
    }
}
