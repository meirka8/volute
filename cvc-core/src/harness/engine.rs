//! The shared ingest: one adapter's record, read from its cursor, planned,
//! sealed and committed, for a whole session family, under one lock.
//!
//! What a run does, in order:
//!
//! 1. Coverage first. Unless the run is one of Claude Code's 0.7.x-compatible
//!    entry points (see below), the repository's capture acknowledgement must
//!    cover the harness's own channel ([`privacy::require_coverage`]) before
//!    any record is read; a channel it does not cover reads nothing, records
//!    nothing and moves no cursor.
//! 2. The family's lock ([`crate::harness::lock`]), on the family root, held
//!    until the run returns.
//! 3. The root's record, then each member the adapter's
//!    [`RecordSource::family`](crate::harness::adapter::RecordSource::family) names, each into its own conversation, each in
//!    one immediate transaction ([`CvcStore::capture_harness_batch`]): its new
//!    captures, its cursor, and the relations its record names. Every batch
//!    asks for a fresh [`privacy::CoverageGrant`] and consumes it.
//! 4. One commit rescue over the whole family
//!    ([`crate::harness::family::link_family_commits`]). If a member fails,
//!    the rescue still runs over what was ingested (the root's cursor is
//!    already past its committing response, which would never be read
//!    again), and the member's error is returned.
//!
//! # Cursors
//!
//! A committed cursor never moves backwards. A rescan reads from the start
//! of the record, records nothing twice (ids are deterministic) and commits
//! no cursor behind the one the read found valid. When the adapter has exact
//! evidence that the record was replaced ([`Resume::replaced`](crate::harness::adapter::Resume::replaced),
//! [`Read::replaced`](crate::harness::adapter::Read::replaced)), the cursor opens the next epoch of the one stored;
//! otherwise the store keeps a stored cursor that is ahead.
//!
//! # Claude Code
//!
//! Claude Code runs through this engine like any harness. Its 0.7.x-compatible
//! entry points (`claude_code::ingest` and its variants) leave the capture
//! decision to their caller, as they did in 0.7.x: cvc's CLI requires the
//! capture acknowledgement before it calls them. Their batches are the
//! crate-private, Claude Code-only kind that carries no grant. Through
//! [`ingest`], Claude Code is gated like every other harness, by its own
//! channel, which every acknowledgement covers.
use crate::db::{
    self, ConversationRelation, CursorProgress, CvcStore, DbError, HarnessBatch,
    HarnessCursorUpdate, HarnessCursorWrite, StoredHarnessCursor,
};
use crate::harness::adapter::{HarnessAdapter, Located, ReadContext, ReadLimit};
use crate::harness::candidate::{Finality, PlanInput};
use crate::harness::cursor::{CursorCodec, Progress};
use crate::harness::drift::Drift;
use crate::harness::family::{self, CommitBearingResponse, Family, FamilyLinks};
use crate::harness::fields;
use crate::harness::lock::FamilyLock;
use crate::harness::seal::{seal, SealContext};
use crate::harness::{derived_interaction_id, HarnessId};
use crate::models::{Conversation, InteractionId};
use crate::privacy::{self, PreparedPolicy, PrivacyError};
use crate::repository::{RepositoryLayout, RepositoryLayoutError};
use chrono::Utc;
use std::any::TypeId;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

/// How one run reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    pub finality: Finality,
    /// Read every record of the family from its start (see the module docs).
    pub rescan: bool,
    /// How long to wait for the family's lock.
    pub lock_wait: Duration,
}

/// What one conversation's batch did.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetReport<C> {
    pub conversation_id: String,
    /// Captures stored, stand-ins included.
    pub inserted: usize,
    /// Responses already stored (or locally deleted), which are never
    /// replaced.
    pub already_present: usize,
    /// Responses still open.
    pub pending: usize,
    /// The cursor the batch committed, or offered when the store kept the
    /// stored one ([`Self::cursor_held`]).
    pub cursor: C,
    pub cursor_held: bool,
    /// How long the record was, as far as the read saw.
    pub record_len: Option<u64>,
    /// The placeholder types of the responses this run newly recorded, in the
    /// order met.
    pub unrecorded: Vec<String>,
    /// What the window held that this version does not record in full.
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
}

/// What one run did.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineReport<C> {
    pub root: TargetReport<C>,
    /// The members the run ingested, in order.
    pub members: Vec<TargetReport<C>>,
    /// The links the family's commit rescue made.
    pub linked: FamilyLinks,
}

/// Why a run failed. Nothing of the conversation whose batch failed was
/// stored; earlier batches of the run are.
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
    /// The adapter claims a harness whose registered adapter is another: it
    /// records nothing.
    #[error("{0} has another adapter in this build; this one records nothing")]
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
/// acknowledgement's coverage of the harness's channel. See the module docs.
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
    if crate::harness::registered_adapter(A::ID).is_some_and(|ours| ours != TypeId::of::<A>()) {
        return Err(EngineError::Unregistered(A::ID));
    }
    if consent == Consent::Covered {
        // Before anything is read; each batch asks again.
        drop(privacy::require_coverage(
            layout.repository(),
            A::ID.channel(),
        )?);
    }
    let _lock = match FamilyLock::acquire(
        &layout.cvc_dir(),
        A::ID.name(),
        &located.family_root,
        options.lock_wait,
    ) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Err(EngineError::Busy {
                harness: A::ID,
                family_root: located.family_root,
                waited: options.lock_wait,
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

    let mut commit_bearing = Vec::new();
    let root = ingest_target(adapter, &run, &mut located, &mut commit_bearing)?;
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
        match ingest_target(adapter, &run, &mut member, &mut commit_bearing) {
            Ok(report) => reports.push(report),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let linked = rescue(&run, &family, &commit_bearing);
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(EngineReport {
        root,
        members: reports,
        linked: linked?,
    })
}

/// Rescues the responses the time-window linker cannot reach: those that
/// created a commit, and the family's reasoning before them. Attribution
/// records who linked, matching the linker, not the historical commit author.
fn rescue<E: std::error::Error + Send + Sync + 'static>(
    run: &Run<'_>,
    family: &Family,
    commit_bearing: &[CommitBearingResponse],
) -> Result<FamilyLinks, EngineError<E>> {
    if commit_bearing.is_empty() {
        return Ok(FamilyLinks::default());
    }
    let capture_worktree = run.layout.worktree_origin()?;
    let linked_by = run
        .layout
        .repository()
        .signature()
        .ok()
        .and_then(|signature| signature.email().map(str::to_owned));
    Ok(family::link_family_commits(
        run.layout.repository(),
        run.store,
        family,
        &capture_worktree,
        commit_bearing,
        linked_by.as_deref(),
    )?)
}

/// Ingests one conversation's record in one batch. The commit links of its
/// responses are left to [`rescue`].
fn ingest_target<A: HarnessAdapter>(
    adapter: &A,
    run: &Run<'_>,
    located: &mut Located<A::Source>,
    commit_bearing: &mut Vec<CommitBearingResponse>,
) -> Result<TargetReport<A::Cursor>, EngineError<A::Error>> {
    let Run { store, policy, .. } = *run;
    let key = located.conversation_id.clone();
    let stored = store.harness_cursor(A::ID, &key)?;
    let resume = adapter.resume(located, stored.as_ref(), store)?;
    let previous = stored
        .as_ref()
        .and_then(|stored| A::Cursor::decode_stored(stored).ok());
    let read = adapter
        .read(
            &mut located.source,
            &resume.from,
            &ReadContext {
                worktree_root: run.worktree_root,
                rescan: run.options.rescan,
                limit: ReadLimit::WHOLE,
            },
        )
        .map_err(EngineError::Adapter)?;
    let plan = adapter.plan(
        &read.window,
        &PlanInput {
            harness: A::ID,
            conversation_id: &key,
            worktree_root: run.worktree_root,
            finality: run.options.finality,
            role: located.role,
            own_history_start: None,
        },
    );

    let existing = store.get_conversation(&key)?;
    let title = located
        .title_seed
        .as_deref()
        .and_then(|seed| fields::derived_title(seed, policy))
        .or_else(|| {
            plan.title
                .as_ref()
                .and_then(|hint| fields::captured_title(&hint.text, policy))
        })
        .or_else(|| {
            existing
                .as_ref()
                .map(|conversation| conversation.title.clone())
                .filter(|title| !title.trim().is_empty())
        })
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
    let mut captures = Vec::new();
    let mut batch_ids: HashSet<InteractionId> = HashSet::new();
    let mut already_present = 0usize;
    let mut unrecorded = Vec::new();
    let mut withheld_fields = 0usize;
    let mut omitted_tool_calls = 0usize;
    let mut omitted_context_items = 0usize;
    for candidate in plan.candidates {
        if !candidate.commit_candidates.is_empty() {
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

    // The cursor never commits behind where the read found it valid: a
    // rescan reads from the start, so its plan may end earlier than an
    // earlier run left the cursor.
    let mut cursor = plan.resume;
    if run.options.rescan && cursor.progress(&read.trusted) == Progress::Backward {
        cursor = read.trusted.clone();
    }
    // A replaced record opens the next epoch of the stored cursor.
    if let (Some(reason), Some(previous)) = (resume.replaced.or(read.replaced), &previous) {
        if cursor.epoch() <= previous.epoch() {
            if let Some(next) = cursor.clone().in_epoch(previous.epoch() + 1, reason) {
                cursor = next;
            }
        }
    }

    let mut relations_skipped = 0usize;
    let mut relations = Vec::new();
    let mut relate =
        |child: Option<String>, parent: Option<String>, relation: Relation| match (child, parent) {
            (Some(child_id), Some(parent_id)) => relations.push(ConversationRelation {
                child_id,
                parent_id,
                kind: relation.kind,
                harness: A::ID,
                spawn_interaction_id: relation.spawn_interaction_id,
                spawn_call_id: relation.spawn_call_id,
                evidence: relation.evidence,
            }),
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

    let state = cursor.encode();
    let legacy = adapter.legacy_cursor(&cursor);
    // Claude Code keeps 0.7.x's row whatever the typed table can hold; a typed
    // cursor outside the table's limits is then left to the store's mirror
    // rule, as 0.7.x's batch left it. Any other harness's cursor goes to the
    // store, which refuses one outside its limits.
    let typed = legacy.is_none() || db::is_storable_cursor(&key, &state);
    let offered = cursor.clone();
    let progress = |stored: &StoredHarnessCursor| match A::Cursor::decode_stored(stored) {
        Ok(stored) => match offered.progress(&stored) {
            Progress::Forward | Progress::NewEpoch => CursorProgress::Forward,
            Progress::Same => CursorProgress::Same,
            Progress::Backward => CursorProgress::Backward,
        },
        // A cursor this codec cannot read is never overwritten in its epoch.
        Err(_) => CursorProgress::Backward,
    };
    let batch = match run.consent {
        Consent::Covered => HarnessBatch::new(
            privacy::require_coverage(run.layout.repository(), A::ID.channel())?,
            A::ID,
            &key,
            policy,
        )?,
        // The store refuses captures of any other harness in this batch.
        Consent::CallerDecided => HarnessBatch::claude_code_caller_decided(&key, policy),
    };
    let mut batch = batch.with_captures(captures).with_relations(relations);
    if typed {
        batch = batch.with_cursor(HarnessCursorWrite {
            cursor_key: &key,
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
            session_id: &key,
            transcript_path: &legacy.transcript_path,
            byte_offset: legacy.byte_offset,
        });
    }
    let outcome = store.capture_harness_batch(batch)?;

    Ok(TargetReport {
        conversation_id: key,
        inserted: outcome.inserted,
        already_present: already_present + outcome.suppressed,
        pending: plan.pending,
        cursor,
        cursor_held: outcome.cursor_held,
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
    })
}

/// A relation as the record names it, before its ids are namespaced.
struct Relation {
    kind: db::RelationKind,
    spawn_interaction_id: Option<InteractionId>,
    spawn_call_id: Option<String>,
    evidence: db::RelationEvidence,
}
