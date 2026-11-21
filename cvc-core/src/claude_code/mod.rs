//! Claude Code adapter: deterministic capture of Claude Code sessions from the
//! harness's own on-disk JSONL transcripts, driven by its lifecycle hooks.
//!
//! Unlike the MCP path (which depends on the model calling `commit_thought`)
//! and the VS Code path (which parses another vendor's private chat storage
//! from an editor process), this channel is plain infrastructure: a hook fires,
//! a fresh `cvc` process reads the transcript from its per-session cursor, and
//! every completed assistant response becomes one private interaction in the
//! shared local database, attributed to the worktree the hook ran in.
//!
//! Claude Code is the first adapter on the harness registry
//! ([`crate::harness`]): the consent check, the lock, scrub-then-bound, the
//! cursor rules and the commit rescue are the shared ones. The pieces here
//! are what is particular to Claude Code, each testable on its own:
//!
//! * [`transcript`] parses transcript lines and plans captures without touching
//!   the filesystem or the database;
//! * [`adapter`] is Claude Code's implementation of the adapter traits: its
//!   hook payload, its transcript and cursor, its subagent family and its
//!   installer;
//! * [`ingest`](mod@ingest) is the 0.7.x-compatible entry points, which run
//!   the shared engine with the adapter under a per-session lock so that
//!   concurrent runs on one session take turns;
//! * [`settings`] installs and removes the hook entries in the checkout-local
//!   Claude Code settings file;
//! * [`harness_record`] is the repository-wide record that the harness is
//!   wanted, which Git's `post-checkout` hook reads to install the entries into
//!   a new linked worktree.
pub mod adapter;
pub mod harness_record;
pub mod ingest;
pub mod links;
pub mod settings;
pub mod transcript;

pub use crate::harness::SESSION_LOCK_WAIT;
pub use adapter::Adapter;
pub use ingest::{
    ingest, ingest_waiting, ingest_with, sniff_first_entry, IngestError, IngestMode, IngestOptions,
    IngestReport,
};
pub use transcript::{FirstEntry, HARNESS};
