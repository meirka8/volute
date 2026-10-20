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
//! The pieces are independent and individually testable:
//!
//! * [`transcript`] parses transcript lines and plans captures without touching
//!   the filesystem or the database;
//! * [`ingest`] applies a plan to a store idempotently, under a per-session
//!   lock so that concurrent runs on one session take turns;
//! * [`settings`] installs and removes the hook entries in the checkout-local
//!   Claude Code settings file;
//! * [`harness_record`] is the repository-wide record that the harness is
//!   wanted, which Git's `post-checkout` hook reads to install the entries into
//!   a new linked worktree.
pub mod harness_record;
pub mod ingest;
pub mod links;
mod session_lock;
pub mod settings;
pub mod transcript;

pub use ingest::{ingest, ingest_waiting, IngestError, IngestMode, IngestReport};
pub use session_lock::SESSION_LOCK_WAIT;
pub use transcript::HARNESS;
