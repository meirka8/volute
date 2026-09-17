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
//! The three pieces are independent and individually testable:
//!
//! * [`transcript`] parses transcript lines and plans captures without touching
//!   the filesystem or the database;
//! * [`ingest`] applies a plan to a store idempotently;
//! * [`settings`] installs and removes the hook entries in the checkout-local
//!   Claude Code settings file.
pub mod ingest;
pub mod settings;
pub mod transcript;

pub use ingest::{ingest, IngestError, IngestMode, IngestReport};
pub use transcript::HARNESS;
