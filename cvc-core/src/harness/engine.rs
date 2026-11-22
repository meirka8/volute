//! The shared ingest: one adapter's record, read from its cursor, planned,
//! sealed and committed in batches, for a whole session family, under one
//! lock.
//!
//! What a run does, in order:
//!
//! 1. Coverage first. Unless the run is one of Claude Code's 0.7.x-compatible
//!    entry points (see below), the repository's capture acknowledgement must
//!    cover the harness's own channel ([`privacy::require_coverage`]) before
//!    the lock is taken, and again before each batch's read; a channel it
//!    does not cover reads nothing, records nothing and moves no cursor.
//! 2. The family's lock ([`crate::harness::lock`]), on the family root, held
//!    until the run returns, all of its batches included.
//! 3. The root's record, then each member the adapter's
//!    [`RecordSource::family`](crate::harness::adapter::RecordSource::family)
//!    names, each into its own conversation, each in batches (see
//!    [Batches](#batches)). Each batch is one immediate transaction
//!    ([`CvcStore::capture_harness_batch`]): its new captures, its cursor,
//!    the relations its window names and the commits its responses reported
//!    (see [Commit evidence](#commit-evidence)). All of it or none of it, and
//!    the cursor advances with each batch, so a run that ends between batches
//!    (stopped by its deadline, failed, or killed) has lost nothing and
//!    recorded nothing twice: the next run goes on from the cursor. Each
//!    batch obtains its own [`privacy::CoverageGrant`] before its read, and
//!    consumes it.
//! 4. One commit rescue over the whole family
//!    ([`crate::harness::family::link_family_commits`]): from the commits the
//!    batches of this run reported, and from the family's stored evidence of
//!    earlier batches and runs. If a member fails, the rescue still runs over
//!    what was ingested (the root's cursor is already past its committing
//!    response, which would never be read again), and the member's error is
//!    returned. The rescue writes links, so on the gated path it runs only
//!    while the acknowledgement still covers the channel: it is skipped when
//!    coverage no longer holds, or when a member failed because it did not.
//!    It is also left to a later run when the deadline left a member of the
//!    family unread (or read in part): a commit in that member's record may
//!    be earlier than the commits read so far, and would claim, as one run of
//!    the whole family does, responses a later commit must not claim first.
//!    The evidence is stored, so the next run that reads the whole family
//!    links from it, exactly as after a run that was killed.
//!
//! # Batches
//!
//! An operator's run ([`EngineOptions::operator`]) reads each record to its
//! end in one batch, as cvc always did. A hook's run
//! ([`EngineOptions::hook`]) reads windows of [`HOOK_BATCH`] and must be over
//! by its deadline, the hook's start plus its harness's budget (#149): a
//! large backlog (a long session resumed after the hooks were installed)
//! then makes progress with every event, instead of being killed at the
//! hook's timeout with nothing committed, run after run.
//!
//! * Only a window that reaches the end of the record is planned with the
//!   run's finality. A window the limit cut ([`Read::limited`]) is planned
//!   [`Finality::Incremental`], so that a response it cut, or whose tool
//!   results lie past it, stays open and its missing results are not
//!   recorded as failures: the cursor stays before it, and the next batch
//!   reads it whole.
//! * A window in which no response closes (one larger than the window, or
//!   one whose results lie far past it) is read again twice as large. The
//!   size it grew to is kept with the cursor
//!   ([`Carry::stall_window`](crate::harness::cursor::Carry::stall_window)),
//!   so that the next run starts with it rather than growing again from the
//!   start. A run whose first estimate does not fit that window reads one
//!   window of the base size first, to time its batches, and keeps the grown
//!   window. A response that needs more than a hook's budget to read is read
//!   by an operator's run (and a final event leaves the backlog flag, below).
//! * Ids, chain parents and stimuli are derived from each window exactly as
//!   for a hook run that read the record when it ended there.
//! * A title the person set is kept against a generated title found in a
//!   later window ([`Carry::title_rank`](crate::harness::cursor::Carry::title_rank)), as one read of the whole record
//!   keeps it.
//!
//! # Time
//!
//! A run with a deadline keeps a reserve for the commit rescue and the
//! commit of a batch already under way: two seconds, or an eighth of the
//! time it had when it started if that is less (two seconds of a 20-second
//! budget, a quarter of a second of Codex's two-second `SessionEnd`). It
//! starts a batch only while what is left beyond the reserve covers twice
//! the batch's estimated time: the time per byte of its last batch that
//! stopped at its limit, or one second per MiB before it has one (cvc 0.7.0
//! took about 0.8 s per MB end to end). A window that does not fit is read
//! smaller, down to [`MIN_WINDOW_BYTES`], unless it grew for a response that
//! a smaller one does not close. The run waits for the lock until four
//! seconds (twice the largest reserve) before its deadline, so that a run
//! that waited still has the time to read a record the holder caught up, or
//! for an eighth of its time if that is longer: 16 s of a 20-second budget,
//! a quarter of a second of a two-second one. Every wait of its writes for
//! another writer is capped by the deadline too
//! ([`CvcStore::cap_write_waits`]). A run without a deadline has none of
//! these limits and waits for the lock as long as its options say.
//!
//! A run the deadline stopped reports it ([`TargetReport::cut_short`],
//! [`EngineReport::cut_short`]). A final run (the agent stopped, or its
//! session ended) that it stopped may be the session's last, so it sets the
//! family's backlog flag on the root's cursor ([`Carry::backlog`](crate::harness::cursor::Carry::backlog),
//! [`backlog`]), which the next run that reads every record of the family to
//! its end clears.
//!
//! # Commit evidence
//!
//! Each batch commits, with its captures, the commits its responses
//! reported, each resolved against the repository first by the rescue's own
//! checks ([`family::resolve_commit`]) and stored by its full id with the
//! stored response's time ([`db::HarnessCommitEvidence`]). Once a batch has
//! committed, the cursor is past its committing response, which is never read
//! again; the stored evidence is what lets a later batch's or run's rescue
//! link a response (a subagent's, say) recorded after it, as one run would
//! have: hook runs reach the links a `--rescan` reaches. A commit replayed
//! from the store links only responses made within `cvc.linkWindow` before
//! its own time, and none when that is 0. Claude Code's 0.7.x-compatible
//! entry points store their evidence but replay none, so that what they
//! link stays exactly what 0.7.x linked.
//!
//! # Cursors
//!
//! A committed cursor never moves backwards. A rescan reads from the start
//! of the record, records nothing twice (ids are deterministic) and commits
//! no cursor behind the one the read found valid. When the adapter has exact
//! evidence that the record was replaced ([`Resume::replaced`](crate::harness::adapter::Resume::replaced),
//! [`Read::replaced`]), the cursor opens the next epoch of the one stored;
//! otherwise the store keeps a stored cursor that is ahead.
//!
//! # A typed cursor of another codec
//!
//! A stored typed row that the adapter's codec cannot read (another kind, a
//! later version) is never overwritten in its epoch. For Claude Code, whose
//! 0.7.x row is the cursor it resumes from, such a row is left alone and the
//! batch writes the 0.7.x row as 0.7.x does, so the session's cursor keeps
//! advancing beside a typed row a newer cvc wrote.
//!
//! # Claude Code
//!
//! Claude Code runs through this engine like any harness. Its 0.7.x-compatible
//! entry points (`claude_code::ingest` and its variants, deprecated) leave the
//! capture decision to their caller, as they did in 0.7.x; cvc's own CLI no
//! longer calls them. Their batches are the
//! crate-private, Claude Code-only kind that carries no grant. Through
//! [`ingest`], Claude Code is gated like every other harness, by its own
//! channel, which every acknowledgement covers.
//!
//! [`Read::limited`]: crate::harness::adapter::Read::limited
//! [`Read::replaced`]: crate::harness::adapter::Read::replaced
use crate::db::{
    self, ConversationRelation, CursorProgress, CvcStore, DbError, HarnessBatch,
    HarnessBatchOutcome, HarnessCommitEvidence, HarnessCursorUpdate, HarnessCursorWrite,
    StoredHarnessCursor, HARNESS_REASONS_SHOWN,
};
use crate::harness::adapter::{HarnessAdapter, Located, ReadContext, ReadLimit, RecordSource};
use crate::harness::candidate::{Finality, PlanInput, TitleRank};
use crate::harness::cursor::{CursorCodec, EpochReason, Progress};
use crate::harness::drift::Drift;
use crate::harness::family::{self, CommitBearingResponse, Family, FamilyLinks, ReplayedCommit};
use crate::harness::fields;
use crate::harness::lock::{FamilyLock, SESSION_LOCK_WAIT};
use crate::harness::seal::{seal, SealContext};
use crate::harness::{derived_interaction_id, HarnessId};
use crate::linker::LinkPolicy;
use crate::models::{CommitSha, Conversation, InteractionId};
use crate::privacy::{self, PreparedPolicy, PrivacyError};
use crate::repository::{RepositoryLayout, RepositoryLayoutError};
use chrono::Utc;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};
use thiserror::Error;

/// How much of a record one batch of a hook's run reads at first: 4 MiB,
/// about three seconds at the 0.8 s per MB cvc 0.7.0 took, so that several
/// batches fit a 20-second budget.
pub const HOOK_BATCH: ReadLimit = ReadLimit {
    bytes: Some(4 * 1024 * 1024),
    units: None,
};

/// The smallest window a batch is shrunk to so that it fits the time left:
/// a batch below it would cost more in its commit than it reads.
pub const MIN_WINDOW_BYTES: u64 = 64 * 1024;

/// The most a run with a deadline keeps for its commit rescue and the commit
/// of a batch already under way.
const RESERVE_MAX: Duration = Duration::from_secs(2);
/// A run with a deadline keeps at most this share of its time as the
/// reserve, and waits for the lock for at least this share of it.
const RESERVE_SHARE: u32 = 8;
/// What a batch is estimated to take per MiB before the run has timed one.
const PRIOR_SECS_PER_MIB: f64 = 1.0;
/// A batch starts only while the time left beyond the reserve covers this
/// many times its estimate.
const ESTIMATE_MARGIN: f64 = 2.0;
const MIB: f64 = 1024.0 * 1024.0;

/// How one run reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    pub finality: Finality,
    /// Read every record of the family from its start (see the module docs).
    pub rescan: bool,
    /// How long to wait for the family's lock, at most: a run with a
    /// deadline waits less (see [Time](self#time)).
    pub lock_wait: Duration,
    /// When the run must be over: a hook's start plus its harness's budget
    /// ([`HookCaller::budget`](crate::harness::adapter::HookCaller::budget)).
    /// `None` is an operator's run, which has no limit.
    pub deadline: Option<Instant>,
    /// What a batch reads at first ([`ReadLimit::WHOLE`]: each record in
    /// one batch).
    pub batch: ReadLimit,
}

impl EngineOptions {
    /// An operator's run: no deadline, each record in one batch, and the
    /// usual wait for the lock.
    pub fn operator(finality: Finality, rescan: bool) -> Self {
        Self {
            finality,
            rescan,
            lock_wait: SESSION_LOCK_WAIT,
            deadline: None,
            batch: ReadLimit::WHOLE,
        }
    }

    /// A hook's run, over by `deadline`, in batches of [`HOOK_BATCH`],
    /// always from the cursor.
    pub fn hook(finality: Finality, deadline: Instant) -> Self {
        Self {
            finality,
            rescan: false,
            lock_wait: SESSION_LOCK_WAIT,
            deadline: Some(deadline),
            batch: HOOK_BATCH,
        }
    }
}

/// What one conversation's batches did, summed.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetReport<C> {
    pub conversation_id: String,
    /// Captures stored, stand-ins included.
    pub inserted: usize,
    /// Responses already stored (or locally deleted), which are never
    /// replaced.
    pub already_present: usize,
    /// Responses still open after the last batch.
    pub pending: usize,
    /// Where the next read of this conversation resumes: the cursor the
    /// last batch committed, or the stored one when the store kept it
    /// ([`Self::cursor_held`]).
    pub cursor: C,
    /// The store kept a stored cursor that is ahead of the one the last
    /// batch offered, so the cursor did not move.
    pub cursor_held: bool,
    /// How long the record was, as far as the last read saw.
    pub record_len: Option<u64>,
    /// The placeholder types of the responses this run newly recorded, in the
    /// order met.
    pub unrecorded: Vec<String>,
    /// What the windows held that this version does not record in full.
    pub drift: Drift,
    /// Of `inserted`, the stand-ins the capture boundary stored.
    pub withheld: usize,
    /// Responses that could not be stored at all.
    pub lost: usize,
    /// Why, each reason once.
    pub withheld_reasons: Vec<String>,
    /// Text fields the seal stored as the withheld note.
    pub withheld_fields: usize,
    pub omitted_tool_calls: usize,
    pub omitted_context_items: usize,
    pub relations_recorded: usize,
    /// Relations the record named whose ids are not conversation ids of the
    /// harness: not recorded.
    pub relations_skipped: usize,
    /// The run's deadline came before this conversation's record was read to
    /// its end (or before it was read at all): the rest is read by a later
    /// run, from [`Self::cursor`].
    pub cut_short: bool,
}

impl<C> TargetReport<C> {
    /// A conversation nothing was read of yet, resuming from `cursor`.
    fn unread(conversation_id: String, cursor: C) -> Self {
        Self {
            conversation_id,
            inserted: 0,
            already_present: 0,
            pending: 0,
            cursor,
            cursor_held: false,
            record_len: None,
            unrecorded: Vec::new(),
            drift: Drift::default(),
            withheld: 0,
            lost: 0,
            withheld_reasons: Vec::new(),
            withheld_fields: 0,
            omitted_tool_calls: 0,
            omitted_context_items: 0,
            relations_recorded: 0,
            relations_skipped: 0,
            cut_short: false,
        }
    }
}

/// What one run did.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineReport<C> {
    pub root: TargetReport<C>,
    /// The members the run ingested or was due to, in order.
    pub members: Vec<TargetReport<C>>,
    /// The links the family's commit rescue made.
    pub linked: FamilyLinks,
    /// The rescue did not run: the acknowledgement no longer covered the
    /// harness's channel when it was due, or the deadline left a member of
    /// the family unread (the next run that reads the whole family rescues
    /// from the stored evidence; see the module docs).
    pub rescue_skipped: bool,
    /// The deadline came before every record of the family was read to its
    /// end: a later run goes on from the cursors.
    pub cut_short: bool,
    /// The family's backlog flag as this run left it ([`Carry::backlog`](crate::harness::cursor::Carry::backlog)).
    pub backlog: bool,
}

/// Why a run failed. Nothing of the batch that failed was stored; earlier
/// batches of the run are.
#[derive(Debug, Error)]
pub enum EngineError<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    /// The adapter's own failure: a record that cannot be read, or one of a
    /// major version this build does not read.
    #[error(transparent)]
    Adapter(E),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Layout(#[from] RepositoryLayoutError),
    /// The capture acknowledgement does not cover the harness's channel here,
    /// or its state could not be read.
    #[error(transparent)]
    Privacy(#[from] PrivacyError),
    /// Another run held the family's lock for the whole wait. It is making
    /// progress; the next run resumes from the cursors.
    #[error("another cvc process is still ingesting {harness} session {family_root} and did not finish within {} s; run this again once it has", waited.as_secs())]
    Busy {
        harness: HarnessId,
        family_root: String,
        waited: Duration,
    },
    /// The lock could not be taken for another reason (the CVC directory is
    /// a link or someone else's, say).
    #[error("cannot lock {harness} session {family_root} for ingestion: {source}")]
    Lock {
        harness: HarnessId,
        family_root: String,
        source: std::io::Error,
    },
    /// The adapter is not the one registered for the harness it names (or
    /// the harness has none in this build): it records nothing.
    #[error("this adapter is not the one registered for {0}; it records nothing")]
    Unregistered(HarnessId),
}

/// Who decided that this run may capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Consent {
    /// The acknowledgement must cover the harness's channel, asked before
    /// anything is read and again for every batch.
    Covered,
    /// Claude Code's 0.7.x-compatible entry points: their caller decided.
    CallerDecided,
}

/// Ingests the family of `located` with `adapter`, gated by the capture
/// acknowledgement's coverage of the harness's channel, as `options` say.
/// See the module docs.
///
/// `policy` is the repository's `.thoughtignore` snapshot, `store` the
/// repository's own (a grant binds the store it was given for), and
/// `located` comes from the adapter, after the caller has sniffed it.
pub fn ingest<A: HarnessAdapter>(
    adapter: &A,
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    located: Located<A::Source>,
    options: EngineOptions,
) -> Result<EngineReport<A::Cursor>, EngineError<A::Error>> {
    run(
        adapter,
        layout,
        store,
        policy,
        located,
        options,
        Consent::Covered,
    )
}

/// [`ingest`] for Claude Code's 0.7.x-compatible entry points, whose caller
/// decided capture (see the module docs).
pub(crate) fn ingest_claude_code_caller_decided(
    adapter: &crate::claude_code::Adapter,
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    located: Located<crate::claude_code::adapter::Source>,
    options: EngineOptions,
) -> Result<
    EngineReport<crate::harness::cursor::JsonlCursor>,
    EngineError<crate::claude_code::IngestError>,
> {
    run(
        adapter,
        layout,
        store,
        policy,
        located,
        options,
        Consent::CallerDecided,
    )
}

/// The conversations of `A`'s harness whose cursor carries the backlog flag
/// ([`Carry::backlog`](crate::harness::cursor::Carry::backlog)): the roots of families whose last final hook run ran
/// out of time before it read every record to its end. A cursor this
/// codec cannot read is not listed.
pub fn backlog<A: RecordSource>(store: &CvcStore) -> Result<Vec<String>, DbError> {
    Ok(store
        .harness_cursors(A::ID)?
        .into_iter()
        .filter(|stored| {
            A::Cursor::decode_stored(stored).is_ok_and(|cursor| cursor.carry().backlog)
        })
        .map(|stored| stored.cursor_key)
        .collect())
}

// ---- time -----------------------------------------------------------------------------

/// What a run with a deadline may still spend, and what its batches have
/// cost (see [Time](self#time)). Pure: the caller says what time it is.
#[derive(Debug, Clone)]
struct Budget {
    deadline: Option<Instant>,
    /// Kept at the end, for the rescue and the commit of a batch under way.
    reserve: Duration,
    /// What one byte of the record took, over the last batch of this run
    /// that stopped at its limit; [`PRIOR_SECS_PER_MIB`] before one has.
    secs_per_byte: f64,
    /// A batch of this run has been timed.
    timed: bool,
}

impl Budget {
    fn new(deadline: Option<Instant>, now: Instant) -> Self {
        let available = deadline.map_or(Duration::ZERO, |deadline| {
            deadline.saturating_duration_since(now)
        });
        Self {
            deadline,
            reserve: RESERVE_MAX.min(available / RESERVE_SHARE),
            secs_per_byte: PRIOR_SECS_PER_MIB / MIB,
            timed: false,
        }
    }

    /// How long to wait for the lock, of the `wanted` wait.
    fn lock_wait(&self, wanted: Duration, now: Instant) -> Duration {
        let Some(deadline) = self.deadline else {
            return wanted;
        };
        let available = deadline.saturating_duration_since(now);
        wanted.min(
            available
                .saturating_sub(2 * RESERVE_MAX)
                .max(available / RESERVE_SHARE),
        )
    }

    /// The limit the next batch reads with, or `None` when no batch may
    /// start now. `window` is the bytes it would read (`None`: all of the
    /// record); a batch that would not fit reads less, unless `grown` (the
    /// window grew for a response a smaller one does not close).
    fn next_limit(&self, window: Option<u64>, grown: bool, now: Instant) -> Option<ReadLimit> {
        let limit = |bytes| ReadLimit { bytes, units: None };
        let Some(deadline) = self.deadline else {
            return Some(limit(window));
        };
        let spare = deadline
            .saturating_duration_since(now)
            .saturating_sub(self.reserve);
        if spare.is_zero() {
            return None;
        }
        let Some(window) = window else {
            return Some(limit(None));
        };
        // Saturating: a rate too small to measure affords any window.
        let affordable = (spare.as_secs_f64() / (ESTIMATE_MARGIN * self.secs_per_byte)) as u64;
        if window <= affordable {
            Some(limit(Some(window)))
        } else if !grown && affordable >= MIN_WINDOW_BYTES {
            Some(limit(Some(affordable)))
        } else {
            None
        }
    }

    /// A batch that stopped at its limit of `bytes` took `took`.
    fn spent(&mut self, bytes: u64, took: Duration) {
        if bytes > 0 {
            self.secs_per_byte = (took.as_secs_f64() / bytes as f64).max(f64::MIN_POSITIVE);
            self.timed = true;
        }
    }
}

// ---- a run ----------------------------------------------------------------------------

/// What stays the same for every conversation of one run.
struct Run<'a> {
    layout: &'a RepositoryLayout,
    store: &'a CvcStore,
    policy: &'a PreparedPolicy,
    options: EngineOptions,
    consent: Consent,
    worktree_root: &'a Path,
    capture_worktree: &'a str,
}

fn run<A: HarnessAdapter>(
    adapter: &A,
    layout: &RepositoryLayout,
    store: &CvcStore,
    policy: &PreparedPolicy,
    mut located: Located<A::Source>,
    options: EngineOptions,
    consent: Consent,
) -> Result<EngineReport<A::Cursor>, EngineError<A::Error>> {
    // Only the adapter registered for its harness records, so a harness's
    // provenance is never claimed by any other code.
    if !crate::harness::is_registered::<A>() {
        return Err(EngineError::Unregistered(A::ID));
    }
    if consent == Consent::Covered {
        // Before the lock is waited for; each batch asks again before its
        // record is read.
        drop(privacy::require_coverage(
            layout.repository(),
            A::ID.channel(),
        )?);
    }
    let started = Instant::now();
    let mut budget = Budget::new(options.deadline, started);
    let lock_wait = budget.lock_wait(options.lock_wait, started);
    let _lock = match FamilyLock::acquire(
        &layout.cvc_dir(),
        A::ID.name(),
        &located.family_root,
        lock_wait,
    ) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Err(EngineError::Busy {
                harness: A::ID,
                family_root: located.family_root,
                waited: lock_wait,
            })
        }
        Err(source) => {
            return Err(EngineError::Lock {
                harness: A::ID,
                family_root: located.family_root,
                source,
            })
        }
    };
    // No write of the run waits for another writer past its deadline.
    let _waits = store.cap_write_waits(options.deadline);
    let worktree_root = layout.worktree_root()?.to_path_buf();
    let capture_worktree = layout.worktree_origin()?;
    let run = Run {
        layout,
        store,
        policy,
        options,
        consent,
        worktree_root: &worktree_root,
        capture_worktree: &capture_worktree,
    };

    // Only the evidence of batches that committed.
    let mut commit_bearing = Vec::new();
    let root = ingest_target(adapter, &run, &mut budget, &mut located)?;
    commit_bearing.extend(root.commit_bearing);
    let carried_backlog = root.carried_backlog;
    let root = root.report;
    // Whatever happens to the members, the root is in the store now, so the
    // family is rescued before the outcome is returned.
    let (family, members, mut failure) = match adapter.family(&located, store) {
        Ok(read) => (read.family, read.members, None),
        Err(error) => (
            Family::new(A::ID, located.family_root.clone()),
            Vec::new(),
            Some(EngineError::Adapter(error)),
        ),
    };
    let mut reports = Vec::new();
    for mut member in members {
        match ingest_target(adapter, &run, &mut budget, &mut member) {
            Ok(target) => {
                commit_bearing.extend(target.commit_bearing);
                reports.push(target.report);
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let cut_short = root.cut_short || reports.iter().any(|report| report.cut_short);
    // The backlog flag: set by a final run the deadline stopped, cleared by
    // a run that read everything; a failed run leaves it as it was.
    let backlog = match (&failure, cut_short) {
        (Some(_), _) => carried_backlog,
        (None, false) => false,
        (None, true) => options.finality == Finality::Final || carried_backlog,
    };
    let backlog = if backlog != carried_backlog && write_backlog(adapter, &run, &located, backlog)?
    {
        backlog
    } else {
        carried_backlog
    };
    // The rescue writes links: on the gated path, only while the channel is
    // still covered. And only once every member is read: a member's commit
    // may be earlier than the ones read so far.
    let member_left = reports.iter().any(|report| report.cut_short);
    let rescue_skipped = member_left
        || consent == Consent::Covered
            && (matches!(
                failure,
                Some(EngineError::Privacy(PrivacyError::NotCovered(_)))
            ) || !privacy::capture_covers(layout.repository(), A::ID.channel())
                .unwrap_or(false));
    let linked = if rescue_skipped {
        Ok(FamilyLinks::default())
    } else {
        rescue(&run, A::ID, &family, &commit_bearing)
    };
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(EngineReport {
        root,
        members: reports,
        linked: linked?,
        rescue_skipped,
        cut_short,
        backlog,
    })
}

/// Rescues the responses the time-window linker cannot reach: those that
/// created a commit, and the family's reasoning before them, from this run's
/// evidence and, on the gated path, the family's stored evidence.
/// Attribution records who linked, matching the linker, not the historical
/// commit author.
///
/// Claude Code's 0.7.x-compatible entry points keep 0.7.x's rescue, from the
/// commits their own run read only: what they link stays exactly what 0.7.x
/// linked. Their batches still store their evidence, for the gated runs
/// after them.
fn rescue<E: std::error::Error + Send + Sync + 'static>(
    run: &Run<'_>,
    harness: HarnessId,
    family: &Family,
    commit_bearing: &[CommitBearingResponse],
) -> Result<FamilyLinks, EngineError<E>> {
    let repo = run.layout.repository();
    let replayed: Vec<ReplayedCommit> = match run.consent {
        Consent::Covered => {
            let window_secs = LinkPolicy::from_repository(repo).window_secs();
            run.store
                .harness_commit_evidence(harness, family.root())?
                .into_iter()
                .map(|evidence| ReplayedCommit {
                    commit: evidence.commit.as_str().to_owned(),
                    response_at: evidence.response_at,
                    window_secs,
                })
                .collect()
        }
        Consent::CallerDecided => Vec::new(),
    };
    if commit_bearing.is_empty() && replayed.is_empty() {
        return Ok(FamilyLinks::default());
    }
    let linked_by = repo
        .signature()
        .ok()
        .and_then(|signature| signature.email().map(str::to_owned));
    Ok(family::link_family_evidence(
        repo,
        run.store,
        family,
        run.capture_worktree,
        commit_bearing,
        &replayed,
        linked_by.as_deref(),
    )?)
}

/// Writes the family's backlog flag on the root's cursor, where it stands,
/// in a batch of its own: `Ok(false)` when it cannot be written (the
/// channel is no longer covered, the stored cursor is another codec's or
/// one this codec keeps no flag in, the record was replaced since).
fn write_backlog<A: HarnessAdapter>(
    adapter: &A,
    run: &Run<'_>,
    root: &Located<A::Source>,
    backlog: bool,
) -> Result<bool, EngineError<A::Error>> {
    let grant = match run.consent {
        Consent::Covered => {
            match privacy::require_coverage(run.layout.repository(), A::ID.channel()) {
                Ok(grant) => Some(grant),
                // A write needs the channel covered; nothing else is wrong.
                Err(PrivacyError::NotCovered(_)) => return Ok(false),
                Err(error) => return Err(error.into()),
            }
        }
        Consent::CallerDecided => None,
    };
    let key = root.conversation_id.as_str();
    let stored = run.store.harness_cursor(A::ID, key)?;
    let foreign = stored
        .as_ref()
        .is_some_and(|stored| A::Cursor::decode_stored(stored).is_err());
    let resume = adapter.resume(root, stored.as_ref(), run.store)?;
    if foreign || resume.replaced.is_some() {
        return Ok(false);
    }
    let mut carry = resume.from.carry();
    carry.backlog = backlog;
    let cursor = resume.from.with_carry(carry.clone());
    if cursor.carry() != carry {
        return Ok(false);
    }
    let batch = new_batch::<A>(grant, key, run.policy)?;
    let (outcome, typed) = commit_with_cursor(adapter, run.store, key, batch, &cursor, foreign)?;
    Ok(typed && !outcome.cursor_held)
}

/// An empty batch for `key`: under `grant`, or Claude Code's 0.7.x-compatible
/// kind without one.
fn new_batch<'a, A: HarnessAdapter>(
    grant: Option<privacy::CoverageGrant>,
    key: &'a str,
    policy: &'a PreparedPolicy,
) -> Result<HarnessBatch<'a>, DbError> {
    match grant {
        Some(grant) => HarnessBatch::new(grant, A::ID, key, policy),
        // Claude Code's 0.7.x-compatible entry points only; the store refuses
        // captures of any other harness in this batch.
        None => Ok(HarnessBatch::claude_code_caller_decided(key, policy)),
    }
}

/// Commits `batch` with `cursor` as the conversation `key`'s cursor, and
/// says whether it carried the typed cursor.
///
/// An adapter with a 0.7.x row (Claude Code) resumes from that row, so it
/// writes it whatever the typed table holds: when the typed row is another
/// codec's (`foreign`), or the typed cursor is outside the table's limits,
/// the batch carries the 0.7.x row alone, and the store's mirror rule leaves
/// another codec's row as it is. Any other harness's cursor goes to the
/// store, which keeps a stored cursor it cannot read and refuses one outside
/// its limits.
fn commit_with_cursor<A: HarnessAdapter>(
    adapter: &A,
    store: &CvcStore,
    key: &str,
    batch: HarnessBatch<'_>,
    cursor: &A::Cursor,
    foreign: bool,
) -> Result<(HarnessBatchOutcome, bool), DbError> {
    let state = cursor.encode();
    let legacy = adapter.legacy_cursor(cursor);
    let typed = match &legacy {
        None => true,
        Some(_) => !foreign && db::is_storable_cursor(key, &state),
    };
    let progress = |stored: &StoredHarnessCursor| match A::Cursor::decode_stored(stored) {
        Ok(stored) => match cursor.progress(&stored) {
            Progress::Forward | Progress::NewEpoch => CursorProgress::Forward,
            Progress::Same => CursorProgress::Same,
            Progress::Backward => CursorProgress::Backward,
        },
        // A cursor this codec cannot read is never overwritten in its epoch.
        Err(_) => CursorProgress::Backward,
    };
    let mut batch = batch;
    if typed {
        batch = batch.with_cursor(HarnessCursorWrite {
            cursor_key: key,
            kind: A::Cursor::KIND,
            version: A::Cursor::VERSION,
            epoch: cursor.epoch(),
            state,
            progress: &progress,
        });
    }
    if let Some(legacy) = &legacy {
        batch = batch.with_legacy_cursor(HarnessCursorUpdate {
            harness: A::ID.name(),
            session_id: key,
            transcript_path: &legacy.transcript_path,
            byte_offset: legacy.byte_offset,
        });
    }
    Ok((store.capture_harness_batch(batch)?, typed))
}

// ---- one conversation -----------------------------------------------------------------

/// One conversation's committed batches: its report, the responses of the
/// batches that reported creating commits, and the backlog flag its stored
/// cursor carried.
struct Target<C> {
    report: TargetReport<C>,
    commit_bearing: Vec<CommitBearingResponse>,
    carried_backlog: bool,
}

/// Where a conversation's next batch reads, and what it carries over from
/// the batches before it.
struct Position<C> {
    /// Where the next read starts.
    from: C,
    /// Exact evidence, from the resume, that the record was replaced: the
    /// first batch's cursor opens the next epoch.
    replaced: Option<EpochReason>,
    /// The next read is a rescan's first, from the start of the record.
    rescan: bool,
    /// A rescan's floor: the cursor its first read found valid, which no
    /// batch of the run commits behind.
    floor: Option<C>,
    /// The typed cursor stored now, when this codec reads it.
    stored: Option<C>,
    /// The stored typed row is another codec's.
    foreign: bool,
}

/// Ingests one conversation's record in batches until it reaches the end,
/// or until the run's deadline leaves no time for the next. The commit links
/// of its responses are left to [`rescue`], which gets them only once their
/// batch has committed.
fn ingest_target<A: HarnessAdapter>(
    adapter: &A,
    run: &Run<'_>,
    budget: &mut Budget,
    located: &mut Located<A::Source>,
) -> Result<Target<A::Cursor>, EngineError<A::Error>> {
    let key = located.conversation_id.clone();
    let stored = run.store.harness_cursor(A::ID, &key)?;
    let resume = adapter.resume(located, stored.as_ref(), run.store)?;
    let decoded = stored
        .as_ref()
        .and_then(|stored| A::Cursor::decode_stored(stored).ok());
    let mut position = Position {
        // A typed row this codec cannot read: another kind, or a later
        // version.
        foreign: stored.is_some() && decoded.is_none(),
        stored: decoded,
        from: resume.from,
        replaced: resume.replaced,
        rescan: run.options.rescan,
        floor: None,
    };
    let carried = position.from.carry();
    let base = run.options.batch.bytes;
    // The window to read, and whether it grew for a response that a smaller
    // one does not close.
    let mut window = match (base, carried.stall_window) {
        (Some(base), Some(grown)) if grown > base => (Some(grown), true),
        _ => (base, false),
    };
    let mut report = TargetReport::unread(key, position.from.clone());
    let mut commit_bearing = Vec::new();
    loop {
        let now = Instant::now();
        let limit = match budget.next_limit(window.0, window.1, now) {
            Some(limit) => limit,
            // A window an earlier run grew, which the estimate before any
            // batch is timed does not fit: one window of the base size first
            // times this run's batches (it closes nothing, and keeps the
            // grown window), and the grown one is tried at that rate.
            None if window.1 && !budget.timed => match budget.next_limit(base, false, now) {
                Some(limit) => limit,
                None => {
                    report.cut_short = true;
                    break;
                }
            },
            None => {
                report.cut_short = true;
                break;
            }
        };
        // What the window grows to when this batch closes nothing: never
        // less than the window wanted, which a timing batch may be below.
        let grow_to = limit
            .bytes
            .map(|bytes| bytes.saturating_mul(2).max(window.0.unwrap_or(0)));
        let started = Instant::now();
        let batch = ingest_batch(adapter, run, located, &mut position, limit, grow_to)?;
        commit_bearing.extend(batch.commit_bearing);
        add_batch(&mut report, batch.report);
        if !batch.limited {
            break;
        }
        // A window that cannot grow any more and closed nothing: an adapter
        // that will not read further is not read again.
        if batch.stalled && limit.bytes.is_none_or(|bytes| bytes == u64::MAX) {
            report.cut_short = true;
            break;
        }
        budget.spent(limit.bytes.unwrap_or(0), started.elapsed());
        window = if batch.stalled {
            (grow_to, true)
        } else {
            (base, false)
        };
    }
    Ok(Target {
        report,
        commit_bearing,
        carried_backlog: carried.backlog,
    })
}

/// Adds what one batch did to what the conversation's batches did before.
fn add_batch<C>(total: &mut TargetReport<C>, batch: TargetReport<C>) {
    total.inserted += batch.inserted;
    total.already_present += batch.already_present;
    total.pending = batch.pending;
    total.cursor = batch.cursor;
    total.cursor_held = batch.cursor_held;
    total.record_len = batch.record_len;
    total.unrecorded.extend(batch.unrecorded);
    total.drift.merge(&batch.drift);
    total.withheld += batch.withheld;
    total.lost += batch.lost;
    for reason in batch.withheld_reasons {
        if total.withheld_reasons.len() < HARNESS_REASONS_SHOWN
            && !total.withheld_reasons.contains(&reason)
        {
            total.withheld_reasons.push(reason);
        }
    }
    total.withheld_fields += batch.withheld_fields;
    total.omitted_tool_calls += batch.omitted_tool_calls;
    total.omitted_context_items += batch.omitted_context_items;
    total.relations_recorded += batch.relations_recorded;
    total.relations_skipped += batch.relations_skipped;
}

/// One committed batch.
struct Batch<C> {
    report: TargetReport<C>,
    commit_bearing: Vec<CommitBearingResponse>,
    /// The read stopped at its limit: there is more to read now.
    limited: bool,
    /// No response closed in the window, which the next batch reads again,
    /// larger.
    stalled: bool,
}

/// Reads, plans, seals and commits one window of a conversation's record,
/// from `position`, and moves `position` past it. `grow_to` is the window
/// the cursor carries when no response closes in this one.
fn ingest_batch<A: HarnessAdapter>(
    adapter: &A,
    run: &Run<'_>,
    located: &mut Located<A::Source>,
    position: &mut Position<A::Cursor>,
    limit: ReadLimit,
    grow_to: Option<u64>,
) -> Result<Batch<A::Cursor>, EngineError<A::Error>> {
    let Run { store, policy, .. } = *run;
    // Before anything of the window is read.
    let grant = match run.consent {
        Consent::Covered => Some(privacy::require_coverage(
            run.layout.repository(),
            A::ID.channel(),
        )?),
        Consent::CallerDecided => None,
    };
    let key = located.conversation_id.clone();
    let rescan = std::mem::take(&mut position.rescan);
    let read = adapter
        .read(
            &mut located.source,
            &position.from,
            &ReadContext {
                worktree_root: run.worktree_root,
                rescan,
                limit,
            },
        )
        .map_err(EngineError::Adapter)?;
    if rescan {
        position.floor = Some(read.trusted.clone());
    }
    // A window the limit cut may end inside a response: only the window that
    // reaches the end of the record is planned with the run's finality.
    let finality = if read.limited {
        Finality::Incremental
    } else {
        run.options.finality
    };
    let plan = adapter.plan(
        &read.window,
        &PlanInput {
            harness: A::ID,
            conversation_id: &key,
            worktree_root: run.worktree_root,
            finality,
            role: located.role,
            own_history_start: read.own_history_start.as_ref(),
        },
    );
    // The window closed no response past where the read began. A rescan's
    // first read begins at the start of the record, which any closed
    // response is past.
    let stalled = read.limited
        && if rescan {
            plan.candidates.is_empty()
        } else {
            !matches!(
                plan.resume.progress(&read.trusted),
                Progress::Forward | Progress::NewEpoch
            )
        };

    let carried = position.from.carry();
    let existing = store.get_conversation(&key)?;
    let existing_title = existing
        .as_ref()
        .map(|conversation| conversation.title.clone())
        .filter(|title| !title.trim().is_empty());
    let seeded = located
        .title_seed
        .as_deref()
        .and_then(|seed| fields::derived_title(seed, policy));
    // A title the record gives, unless the conversation holds one the
    // person set and this one was generated.
    let hinted = match (&seeded, &plan.title) {
        (None, Some(hint))
            if existing_title.is_none()
                || carried.title_rank.is_none_or(|held| hint.rank >= held) =>
        {
            fields::captured_title(&hint.text, policy).map(|title| (title, hint.rank))
        }
        _ => None,
    };
    let title = seeded
        .or_else(|| hinted.as_ref().map(|(title, _)| title.clone()))
        .or(existing_title)
        .or_else(|| {
            plan.first_prompt
                .as_deref()
                .and_then(|prompt| fields::derived_title(prompt, policy))
        })
        .unwrap_or_else(|| A::DEFAULT_TITLE.to_owned());
    let created_at = existing
        .as_ref()
        .map(|conversation| conversation.created_at)
        .or_else(|| plan.candidates.first().map(|candidate| candidate.timestamp))
        .unwrap_or_else(Utc::now);
    let conversation = Conversation {
        id: key.clone(),
        title,
        created_at,
    };

    let seal_context = SealContext {
        harness: A::ID,
        conversation: &conversation,
        policy,
        capture_worktree: run.capture_worktree,
    };
    let now = Utc::now().timestamp();
    let mut captures = Vec::new();
    let mut batch_ids: HashSet<InteractionId> = HashSet::new();
    let mut already_present = 0usize;
    let mut unrecorded = Vec::new();
    let mut withheld_fields = 0usize;
    let mut omitted_tool_calls = 0usize;
    let mut omitted_context_items = 0usize;
    let mut commit_bearing = Vec::new();
    let mut evidence: Vec<HarnessCommitEvidence> = Vec::new();
    for candidate in plan.candidates {
        if !candidate.commit_candidates.is_empty() {
            let response_at = candidate.timestamp.timestamp();
            // Stored only once resolved, by the rescue's own checks; the
            // store keeps it only for a response it holds.
            for token in &candidate.commit_candidates {
                let Some((commit, _)) =
                    family::resolve_commit(run.layout.repository(), token, response_at, now)
                else {
                    continue;
                };
                let item = HarnessCommitEvidence {
                    family_root: located.family_root.clone(),
                    interaction_id: candidate.id.clone(),
                    commit: CommitSha::new(commit),
                    response_at,
                };
                if !evidence.contains(&item) {
                    evidence.push(item);
                }
            }
            commit_bearing.push(CommitBearingResponse {
                interaction_id: candidate.id.clone(),
                timestamp: candidate.timestamp,
                candidates: candidate.commit_candidates.clone(),
            });
        }
        if store.interaction_exists(&candidate.id)? {
            already_present += 1;
            continue;
        }
        unrecorded.extend(candidate.unrecorded.iter().cloned());
        // A parent outside this batch and outside the store (suppressed, or
        // never recorded) breaks the chain rather than failing the batch.
        let parent = match candidate.parent.clone() {
            Some(parent) if batch_ids.contains(&parent) || store.interaction_exists(&parent)? => {
                Some(parent)
            }
            _ => None,
        };
        let id = candidate.id.clone();
        let sealed = seal(candidate, parent, &seal_context);
        withheld_fields += sealed.withheld.len();
        omitted_tool_calls += sealed.omitted_tool_calls;
        omitted_context_items += sealed.omitted_context_items;
        batch_ids.insert(id);
        captures.push(sealed.capture);
    }

    // The cursor never commits behind where a rescan's first read found it
    // valid: a rescan reads from the start, so its plan may end earlier than
    // an earlier run left the cursor. The next read starts where the plan
    // ends.
    let mut cursor = plan.resume.clone();
    let mut next = plan.resume;
    if let Some(floor) = &position.floor {
        if cursor.progress(floor) == Progress::Backward {
            cursor = floor.clone();
        }
    }
    // A replaced record opens the next epoch of the stored cursor.
    if let (Some(reason), Some(stored)) =
        (position.replaced.take().or(read.replaced), &position.stored)
    {
        if cursor.epoch() <= stored.epoch() {
            let epoch = stored.epoch() + 1;
            if let Some(opened) = cursor.clone().in_epoch(epoch, reason) {
                cursor = opened;
                next = next.clone().in_epoch(epoch, reason).unwrap_or(next);
                position.floor = position
                    .floor
                    .take()
                    .and_then(|floor| floor.in_epoch(epoch, reason));
            }
        }
    }
    // What the cursor carries: the title rank once a title the person set
    // is stored (with this batch's captures), and the grown window while no
    // response closes.
    let mut carry = carried.clone();
    if !captures.is_empty() {
        if let Some((_, TitleRank::UserSet)) = hinted {
            carry.title_rank = Some(TitleRank::UserSet);
        }
    }
    carry.stall_window = if stalled { grow_to } else { None };
    let cursor = cursor.with_carry(carry.clone());
    let next = next.with_carry(carry);

    // Relations are checked here against what the store accepts, so that one
    // the store would refuse is left out and counted rather than refusing
    // the batch: both ids must be the harness's conversation ids the store
    // can hold, and different; a spawn call id the store cannot hold is
    // dropped and the relation kept.
    let mut relations_skipped = 0usize;
    let mut relations = Vec::new();
    let mut relate =
        |child: Option<String>, parent: Option<String>, relation: Relation| match (child, parent) {
            (Some(child_id), Some(parent_id))
                if child_id != parent_id
                    && db::is_storable_identifier(&child_id)
                    && db::is_storable_identifier(&parent_id) =>
            {
                relations.push(ConversationRelation {
                    child_id,
                    parent_id,
                    kind: relation.kind,
                    harness: A::ID,
                    spawn_interaction_id: relation.spawn_interaction_id,
                    spawn_call_id: relation
                        .spawn_call_id
                        .filter(|id| db::is_storable_identifier(id)),
                    evidence: relation.evidence,
                })
            }
            _ => relations_skipped += 1,
        };
    for child in &plan.children {
        relate(
            A::ID.conversation_id(&child.agent_id).ok(),
            Some(key.clone()),
            Relation {
                kind: child.kind,
                spawn_interaction_id: child
                    .spawn_key
                    .as_deref()
                    .map(|spawn| derived_interaction_id(A::ID, &key, spawn)),
                spawn_call_id: child.spawn_call_id.clone(),
                evidence: child.evidence,
            },
        );
    }
    if let Some(parent) = &plan.parent {
        relate(
            Some(key.clone()),
            A::ID.conversation_id(&parent.agent_id).ok(),
            Relation {
                kind: parent.kind,
                spawn_interaction_id: None,
                spawn_call_id: parent.spawn_call_id.clone(),
                evidence: parent.evidence,
            },
        );
    }

    let batch = new_batch::<A>(grant, &key, policy)?
        .with_captures(captures)
        .with_relations(relations)
        .with_evidence(evidence);
    let (outcome, typed) =
        commit_with_cursor(adapter, store, &key, batch, &cursor, position.foreign)?;
    // A batch that carried a typed cursor and had it kept did not move the
    // cursor this harness resumes from: report the one the next read resumes
    // from. (A batch with the 0.7.x row alone always writes that row.)
    let cursor_held = typed && outcome.cursor_held;
    let stored = store.harness_cursor(A::ID, &key)?;
    position.stored = stored
        .as_ref()
        .and_then(|stored| A::Cursor::decode_stored(stored).ok());
    let cursor = if cursor_held {
        adapter.resume(located, stored.as_ref(), store)?.from
    } else {
        cursor
    };
    // A rescan goes on reading where its plan ended; any other run goes on
    // from the cursor the next run would resume from.
    position.from = if cursor_held && position.floor.is_none() {
        cursor.clone()
    } else {
        next
    };

    let report = TargetReport {
        inserted: outcome.inserted,
        already_present: already_present + outcome.suppressed,
        pending: plan.pending,
        cursor_held,
        record_len: read.record_len,
        unrecorded,
        drift: plan.drift,
        withheld: outcome.withheld,
        lost: outcome.lost,
        withheld_reasons: outcome.reasons,
        withheld_fields,
        omitted_tool_calls,
        omitted_context_items,
        relations_recorded: outcome.relations_recorded,
        relations_skipped,
        ..TargetReport::unread(key, cursor)
    };
    Ok(Batch {
        report,
        commit_bearing,
        limited: read.limited,
        stalled,
    })
}

/// A relation as the record names it, before its ids are namespaced.
struct Relation {
    kind: db::RelationKind,
    spawn_interaction_id: Option<InteractionId>,
    spawn_call_id: Option<String>,
    evidence: db::RelationEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;
    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn window(budget: &Budget, bytes: u64, grown: bool, now: Instant) -> Option<u64> {
        budget
            .next_limit(Some(bytes), grown, now)
            .map(|limit| limit.bytes.unwrap())
    }

    /// The numbers of the module docs: a 20-second budget keeps 2 s and
    /// waits 16 s for the lock; Codex's two-second `SessionEnd` keeps a
    /// quarter of a second and waits a quarter of a second.
    #[test]
    fn reserves_and_lock_waits_follow_the_budget() {
        let now = Instant::now();
        let hook = Budget::new(Some(now + S(20)), now);
        assert_eq!(hook.reserve, S(2));
        assert_eq!(hook.lock_wait(SESSION_LOCK_WAIT, now), S(16));
        let session_end = Budget::new(Some(now + S(2)), now);
        assert_eq!(session_end.reserve, MS(250));
        assert_eq!(session_end.lock_wait(SESSION_LOCK_WAIT, now), MS(250));
        // Later in the run, the wait is what is left of the same rule.
        assert_eq!(hook.lock_wait(SESSION_LOCK_WAIT, now + S(10)), S(6));
        // A deadline already past waits for nothing but one try.
        let late = Budget::new(Some(now), now + S(1));
        assert_eq!(late.reserve, Duration::ZERO);
        assert_eq!(
            late.lock_wait(SESSION_LOCK_WAIT, now + S(1)),
            Duration::ZERO
        );
        // Without a deadline, everything is as asked.
        let operator = Budget::new(None, now);
        assert_eq!(
            operator.lock_wait(SESSION_LOCK_WAIT, now),
            SESSION_LOCK_WAIT
        );
        assert_eq!(
            operator.next_limit(None, false, now + S(3600)),
            Some(ReadLimit::WHOLE)
        );
    }

    /// Before it has timed a batch, a run estimates one second per MiB and
    /// starts a batch only while twice that fits what is left beyond the
    /// reserve; the base window shrinks to fit, a grown one does not.
    #[test]
    fn a_batch_starts_only_while_twice_its_estimate_fits() {
        let now = Instant::now();
        let base = HOOK_BATCH.bytes.unwrap();
        let mib = 1024 * 1024;
        let hook = Budget::new(Some(now + S(20)), now);
        assert_eq!(window(&hook, base, false, now), Some(base));
        // 18 s beyond the reserve affords 9 MiB.
        assert_eq!(window(&hook, 8 * mib, true, now), Some(8 * mib));
        assert_eq!(window(&hook, 16 * mib, true, now), None);
        // With 6 s left, 2 MiB fit: the base window shrinks, a grown one
        // waits for a run with the time.
        assert_eq!(window(&hook, base, false, now + S(14)), Some(2 * mib));
        assert_eq!(window(&hook, base, true, now + S(14)), None);
        // Inside the reserve, nothing starts.
        assert_eq!(window(&hook, MIN_WINDOW_BYTES, false, now + S(18)), None);
        // Codex's two-second SessionEnd reads one smaller batch.
        let session_end = Budget::new(Some(now + S(2)), now);
        let first = window(&session_end, base, false, now).unwrap();
        assert!((800 * 1024..=900 * 1024).contains(&first), "{first}");
        // A window that would shrink below the smallest one does not start.
        assert_eq!(window(&session_end, base, false, now + MS(1700)), None);
    }

    /// Once a batch is timed, its rate replaces the estimate.
    #[test]
    fn a_timed_batch_sets_the_rate() {
        let now = Instant::now();
        let mut budget = Budget::new(Some(now + S(20)), now);
        // 1 MiB in 2 s: 18 s afford 4.5 MiB.
        budget.spent(1024 * 1024, S(2));
        assert_eq!(
            window(&budget, 4 * 1024 * 1024, true, now),
            Some(4 * 1024 * 1024)
        );
        assert_eq!(window(&budget, 5 * 1024 * 1024, true, now), None);
        // A batch too quick to time affords any window.
        budget.spent(1024, Duration::ZERO);
        assert_eq!(window(&budget, u64::MAX, true, now), Some(u64::MAX));
    }
}
