//! `cvc ingest <harness>`: record an agent harness's session locally.
//!
//! Not to be confused with sync ingestion (`cvc pull`), which imports another
//! machine's already-shared projection: this is first-party local capture.
//!
//! Every harness goes through the registry ([`cvc_core::harness::with_adapter`])
//! and the shared engine ([`cvc_core::harness::engine::ingest`]), gated by
//! the capture acknowledgement's coverage of the harness's own channel:
//!
//! * `--hook` is the hook-mode shell ([`shell`]), one code path for every
//!   harness.
//! * `--transcript <path> [--session <id>]`, `--session <id>` and `--scan` are
//!   an operator's runs, which treat what they read as complete and print what
//!   they recorded ([`report`]).
//!
//! A harness CVC knows by name but this build has no adapter for fails with
//! [`super::harness::not_available`].
mod report;
mod shell;

use anyhow::{anyhow, bail, Result};
use clap::{ArgGroup, Args};
use cvc_core::db::{CvcStore, DbError};
use cvc_core::harness::adapter::{HarnessAdapter, Located};
use cvc_core::harness::candidate::Finality;
use cvc_core::harness::engine::{self, EngineError, EngineOptions};
use cvc_core::harness::{self, AdapterVisitor, HarnessId};
use cvc_core::privacy::{PreparedPolicy, PrivacyError};
use std::env;
use std::path::PathBuf;

/// How `cvc ingest <harness>` finds what to read.
#[derive(Args, Debug, Clone, Default)]
#[command(group(
    ArgGroup::new("source")
        .args(["transcript", "session", "scan"])
        .multiple(true)
))]
pub(crate) struct IngestArgs {
    /// Record file to read, for a harness whose record is a file (Claude Code: ~/.claude/projects/<workspace>/<session>.jsonl)
    #[arg(long)]
    pub(crate) transcript: Option<PathBuf>,
    /// Session id: with --transcript, the id to record it under (default: the transcript file name); alone, the session to find, where the harness supports it
    #[arg(long)]
    pub(crate) session: Option<String>,
    /// Read the hook payload the harness delivers on stdin
    #[arg(long, conflicts_with_all = ["transcript", "session", "scan", "rescan"])]
    pub(crate) hook: bool,
    /// Find and read every session of this repository the harness keeps, where the harness supports it
    #[arg(long, conflicts_with_all = ["transcript", "session"])]
    pub(crate) scan: bool,
    /// Read the session and its subagents from the start, ignoring saved cursors: recorded thoughts are kept and commit links are derived again
    #[arg(long, requires = "source")]
    pub(crate) rescan: bool,
}

/// What an operator's run reads.
enum Target {
    /// A record file, with an optional id override.
    File(PathBuf, Option<String>),
    /// A session by the agent's id.
    Session(String),
    /// Every session the harness keeps for this repository.
    Scan,
}

pub(crate) async fn run(harness: HarnessId, args: IngestArgs) -> Result<()> {
    if !harness.available() {
        if args.hook {
            // The agent still gets the output it requires and its payload
            // is read off the pipe.
            return Err(shell::refuse_unavailable(
                harness,
                shell::HookIo {
                    stdin: &mut std::io::stdin().lock(),
                    stdout: &mut std::io::stdout(),
                },
            ));
        }
        return Err(super::harness::not_available(harness));
    }
    if args.hook {
        return shell::run_hook(harness);
    }
    let target = match (args.transcript, args.session, args.scan) {
        (Some(path), session, _) => Target::File(path, session),
        (None, Some(session), _) => Target::Session(session),
        (None, None, true) => Target::Scan,
        (None, None, false) => bail!(
            "pass --transcript <path> (optionally --session <id>), or --hook to read a {} hook payload from stdin",
            harness.display()
        ),
    };
    // An operator-driven read treats the record as complete: it is the
    // catch-up path for finished sessions and for sessions recorded before
    // the hooks were installed.
    let operator = Operator {
        directory: env::current_dir()?,
        target,
        rescan: args.rescan,
    };
    let lines = harness::with_adapter(harness, operator)
        .unwrap_or_else(|| Err(super::harness::not_available(harness)))?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// An operator's run, for whichever adapter the registry has.
struct Operator {
    directory: PathBuf,
    target: Target,
    rescan: bool,
}

impl AdapterVisitor for Operator {
    type Output = Result<Vec<String>>;

    fn visit<A: HarnessAdapter>(self, adapter: &A) -> Result<Vec<String>> {
        // A way of finding sessions the adapter does not offer is refused
        // first, wherever this runs.
        let unsupported = match &self.target {
            Target::Session(_) if !adapter.locates_sessions() => {
                Some("--session without --transcript")
            }
            Target::Scan if !adapter.scans() => Some("--scan"),
            _ => None,
        };
        if let Some(what) = unsupported {
            bail!(
                "{what} is not supported for {}; pass --transcript",
                A::ID.name()
            );
        }
        let layout = super::harness::discover_initialized_at(&self.directory)?;
        // The engine asks again before it reads; this is where the person is
        // told what is missing.
        drop(super::harness::require_capture_covered(
            &layout,
            A::ID,
            adapter.stop_hint(),
        )?);
        // A record file is located and screened before the store is opened
        // (`admit_file`): one that is not this harness's, or not a regular
        // file, leaves nothing behind. An adapter that finds sessions by id
        // or by scanning screens what it finds itself (see
        // `RecordSource::locate_session` and `RecordSource::scan`).
        let located: Vec<Located<A::Source>> = match self.target {
            Target::File(path, id) => vec![adapter.admit_file(&path, id.as_deref())?],
            Target::Session(id) => vec![adapter.locate_session(&id, &layout)?],
            Target::Scan => adapter.scan(&layout)?,
        };
        let policy = load_policy(&layout)?;
        // An operator's run also finishes the store's one-time capture-source
        // upgrade, which a hook never runs.
        let store = CvcStore::open_initialized_upgrading(layout.db_path())?;
        let mut lines = Vec::new();
        for located in located {
            let report = engine::ingest(
                adapter,
                &layout,
                &store,
                &policy,
                located,
                EngineOptions::operator(Finality::Final, self.rescan),
            )
            .map_err(|error| explain(error, &store, A::ID, adapter.stop_hint()))?;
            lines.extend(report::lines(&report::Summary::new(A::ID, report)));
        }
        Ok(lines)
    }
}

/// The repository's `.thoughtignore` snapshot, which every capture of the run
/// is scrubbed with.
fn load_policy(layout: &cvc_core::repository::RepositoryLayout) -> Result<PreparedPolicy> {
    PreparedPolicy::load(layout.policy_root()?)
        .map_err(|error| anyhow!("CVC capture blocked by .thoughtignore: {error}"))
}

/// An engine failure as the CLI reports it. Most read as the engine words
/// them; two get a sentence that says what to do.
fn explain<E>(
    error: EngineError<E>,
    store: &CvcStore,
    harness: HarnessId,
    stop_hint: &str,
) -> anyhow::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    match error {
        EngineError::Db(DbError::CaptureSourceUpgradeRequired { harness }) => upgrade_required(
            harness,
            store.capture_source_upgrade_blocked().unwrap_or(false),
        ),
        // The acknowledgement was withdrawn, or narrowed, while the run was
        // under way: the same message as before the run.
        EngineError::Privacy(PrivacyError::NotCovered(coverage)) => {
            match super::harness::coverage_refusal(harness, coverage, stop_hint) {
                Some(refusal) => refusal,
                None => EngineError::<E>::Privacy(PrivacyError::NotCovered(coverage)).into(),
            }
        }
        other => other.into(),
    }
}

/// What a capture this store cannot hold yet comes to. `blocked` is a table
/// this version will never rebuild
/// ([`CvcStore::capture_source_upgrade_blocked`]).
fn upgrade_required(harness: HarnessId, blocked: bool) -> anyhow::Error {
    if blocked {
        anyhow!(
            "CVC cannot record {harness} sessions in this repository: its database was changed by another cvc build and has columns this version does not know, so this version will not upgrade it. Nothing was recorded; a cvc that knows the database can record {harness} here."
        )
    } else {
        anyhow!(
            "CVC cannot record {harness} sessions until its database is upgraded: run `cvc status` once to finish upgrading CVC's database. Nothing was recorded."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_that_waits_for_its_upgrade_says_how_to_finish_it() {
        assert_eq!(
            upgrade_required(HarnessId::Codex, false).to_string(),
            "CVC cannot record Codex sessions until its database is upgraded: run `cvc status` once to finish upgrading CVC's database. Nothing was recorded."
        );
        let never = upgrade_required(HarnessId::Codex, true).to_string();
        assert!(
            never.contains("this version will not upgrade it"),
            "{never}"
        );
        assert!(!never.contains("cvc status"), "{never}");
    }
}
