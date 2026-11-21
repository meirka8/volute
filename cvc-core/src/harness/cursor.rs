//! Typed ingest cursors: where reading an agent's record resumes.
//!
//! A cursor holds positions, never content: offsets, ordinals, sequence
//! numbers, watermarks, the record's path, digests and flags. Each kind of
//! record has its own codec ([`CursorCodec`]) with a `kind` name, a version,
//! and a reader for every version it ever wrote. The state is stored as JSON
//! in `harness_cursors` (see [`crate::db::HarnessCursorWrite`]); every state
//! type rejects fields it does not know (`deny_unknown_fields`), so a state
//! written by another codec is never half-read.
//!
//! # Order and epochs
//!
//! A cursor never moves backwards. The store compares a batch's cursor with
//! the stored one inside the batch's transaction: an earlier epoch or an
//! older version keeps the stored one, the same epoch asks
//! [`CursorCodec::progress`], and exactly the next epoch replaces it. An
//! epoch is opened only on exact evidence that the record was replaced (a
//! Claude Code transcript that was rewritten, a Codex rollout migrated or
//! reverted), and the cursor says why ([`EpochReason`]).
//!
//! # Claude Code
//!
//! Claude Code's cursor is a [`JsonlCursor`]. cvc 0.8 copied every 0.7.x
//! cursor into the typed table as `{path, byte_offset}`; those states decode
//! with every other field at its default, and a cursor with nothing but a
//! path and an offset encodes back to exactly those two fields.
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How a cursor compares with the previous one of the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Forward,
    Same,
    Backward,
    /// A later epoch: the record was replaced, and reading starts again.
    NewEpoch,
}

/// Why a stored cursor state was not read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CursorError {
    #[error("the cursor is of kind {found:?}, not {expected:?}")]
    Kind {
        found: String,
        expected: &'static str,
    },
    #[error("cursor version {0} has no reader in this version")]
    Version(u32),
    #[error("the cursor state does not decode: {0}")]
    State(String),
}

/// Why a cursor opened a new epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpochReason {
    /// The record was rewritten or truncated in place.
    Rewritten,
    /// The record moved, and its identity could not be followed.
    Moved,
    /// The agent reverted the record to an earlier state.
    Reverted,
    /// The agent migrated the record to a new layout.
    Migrated,
    /// The record was created again under the same key, and its numbering
    /// started over (a Cursor spool deleted and begun anew).
    Recreated,
}

/// One kind of cursor. See the module docs.
pub trait CursorCodec: Clone + Sized {
    /// The `kind` stored with the state: 1 to 32 bytes of `[a-z0-9-]`.
    const KIND: &'static str;
    /// The version this codec writes. It is bumped with a reader for every
    /// older version, and stored states are never rewritten in place.
    const VERSION: u32;

    /// The state to store. It must hold positions only.
    fn encode(&self) -> serde_json::Value;
    /// The cursor a stored row holds: `version` and `state` as stored, and the
    /// row's `epoch`.
    fn decode(version: u32, epoch: u64, state: &serde_json::Value) -> Result<Self, CursorError>;
    /// How `self` compares with `previous` (same key).
    fn progress(&self, previous: &Self) -> Progress;
    /// The position in words, for reports: `byte 123`, `ordinal 88`.
    fn describe(&self) -> String;
    /// The epoch the cursor is in. Kinds without epochs are always in 0.
    fn epoch(&self) -> u64 {
        0
    }
    /// The same position in epoch `epoch`, opened for `reason`; `None` for a
    /// kind that has no epochs.
    fn in_epoch(self, _epoch: u64, _reason: EpochReason) -> Option<Self> {
        None
    }

    /// The cursor a stored row holds, when the row is of this codec's kind.
    fn decode_stored(stored: &crate::db::StoredHarnessCursor) -> Result<Self, CursorError> {
        if stored.kind != Self::KIND {
            return Err(CursorError::Kind {
                found: stored.kind.clone(),
                expected: Self::KIND,
            });
        }
        Self::decode(stored.version, stored.epoch, &stored.state)
    }
}

/// The identity of a JSONL record independent of its path: the SHA-256 of
/// its first complete line, and that line's length.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileId {
    /// 64 lowercase hex digits.
    pub sha256: String,
    pub len: u64,
}

/// Planner state that must survive between batches and runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Carry {
    /// The rank of the title the conversation holds, so a title the person
    /// set is never replaced by a generated one from a later window. Used
    /// from PR 4 of #187 (the batched ingest of #149); written by nothing
    /// yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_rank: Option<crate::harness::candidate::TitleRank>,
    /// A final event ran out of time before it reached the end.
    #[serde(default, skip_serializing_if = "is_false")]
    pub backlog: bool,
    /// The read window, grown across runs while a response stays open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_window: Option<u64>,
    /// Adapter-defined positions (a step hash, a regime), at most 4 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Carry {
    pub fn is_empty(&self) -> bool {
        *self == Carry::default()
    }
}

/// A cursor into an append-only JSONL record: Claude Code, Codex, Antigravity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlCursor {
    /// The record's path, as located.
    pub path: String,
    /// The record's identity; `None` on rows copied from 0.7.x, and for
    /// Claude Code, which does not act on it.
    pub file_id: Option<FileId>,
    /// Into the decoded stream (a `.jsonl.zst` is decoded before counting).
    pub byte_offset: u64,
    /// Codex's paginated rollouts.
    pub last_ordinal: Option<u64>,
    pub epoch: u64,
    /// Why `epoch` was opened; `None` in epoch 0.
    pub epoch_reason: Option<EpochReason>,
    pub carry: Carry,
}

impl JsonlCursor {
    /// The start of the record at `path`.
    pub fn start(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            file_id: None,
            byte_offset: 0,
            last_ordinal: None,
            epoch: 0,
            epoch_reason: None,
            carry: Carry::default(),
        }
    }

    /// The same cursor at `byte_offset`.
    pub fn at(&self, byte_offset: u64) -> Self {
        Self {
            byte_offset,
            ..self.clone()
        }
    }
}

/// The stored state of a [`JsonlCursor`], version 1.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonlStateV1 {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_id: Option<FileId>,
    byte_offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_ordinal: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epoch_reason: Option<EpochReason>,
    #[serde(default, skip_serializing_if = "Carry::is_empty")]
    carry: Carry,
}

fn encode_state(state: impl Serialize) -> serde_json::Value {
    // Every state is a struct of plain fields, which always serializes.
    serde_json::to_value(state).unwrap_or(serde_json::Value::Null)
}

fn decode_state<T: for<'de> Deserialize<'de>>(state: &serde_json::Value) -> Result<T, CursorError> {
    T::deserialize(state).map_err(|error| CursorError::State(error.to_string()))
}

fn ordered<T: Ord>(current: T, previous: T) -> Progress {
    match current.cmp(&previous) {
        std::cmp::Ordering::Greater => Progress::Forward,
        std::cmp::Ordering::Equal => Progress::Same,
        std::cmp::Ordering::Less => Progress::Backward,
    }
}

impl CursorCodec for JsonlCursor {
    const KIND: &'static str = "jsonl";
    const VERSION: u32 = 1;

    fn encode(&self) -> serde_json::Value {
        encode_state(JsonlStateV1 {
            path: self.path.clone(),
            file_id: self.file_id.clone(),
            byte_offset: self.byte_offset,
            last_ordinal: self.last_ordinal,
            epoch_reason: self.epoch_reason,
            carry: self.carry.clone(),
        })
    }

    fn decode(version: u32, epoch: u64, state: &serde_json::Value) -> Result<Self, CursorError> {
        if version != 1 {
            return Err(CursorError::Version(version));
        }
        let state: JsonlStateV1 = decode_state(state)?;
        Ok(Self {
            path: state.path,
            file_id: state.file_id,
            byte_offset: state.byte_offset,
            last_ordinal: state.last_ordinal,
            epoch,
            epoch_reason: state.epoch_reason,
            carry: state.carry,
        })
    }

    /// The epoch first. Within one epoch: two different record identities
    /// cannot be ordered, so that is backwards (the stored one is kept);
    /// otherwise the ordinal when both have one, else the byte offset. The
    /// path is not compared: a record that moved is not a step back.
    fn progress(&self, previous: &Self) -> Progress {
        match self.epoch.cmp(&previous.epoch) {
            std::cmp::Ordering::Greater => return Progress::NewEpoch,
            std::cmp::Ordering::Less => return Progress::Backward,
            std::cmp::Ordering::Equal => {}
        }
        if let (Some(current), Some(earlier)) = (&self.file_id, &previous.file_id) {
            if current != earlier {
                return Progress::Backward;
            }
        }
        match (self.last_ordinal, previous.last_ordinal) {
            (Some(current), Some(earlier)) => ordered(current, earlier),
            _ => ordered(self.byte_offset, previous.byte_offset),
        }
    }

    fn describe(&self) -> String {
        match self.last_ordinal {
            Some(ordinal) => format!("ordinal {ordinal}"),
            None => format!("byte {}", self.byte_offset),
        }
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }

    fn in_epoch(self, epoch: u64, reason: EpochReason) -> Option<Self> {
        Some(Self {
            epoch,
            epoch_reason: Some(reason),
            ..self
        })
    }
}

/// A `(time_updated, id)` watermark over a SQLite store's rows (OpenCode
/// 1.x, and the watermark of a scan).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteWatermark {
    /// The store's identity.
    pub db_id: String,
    pub time_updated_ms: i64,
    pub last_id: String,
    /// How far behind the watermark a read starts again, for rows written
    /// with a time just before it.
    pub overlap_ms: u32,
}

impl CursorCodec for SqliteWatermark {
    const KIND: &'static str = "sqlite-watermark";
    const VERSION: u32 = 1;

    fn encode(&self) -> serde_json::Value {
        encode_state(self)
    }
    fn decode(version: u32, _epoch: u64, state: &serde_json::Value) -> Result<Self, CursorError> {
        if version != 1 {
            return Err(CursorError::Version(version));
        }
        decode_state(state)
    }
    /// `(time_updated_ms, last_id)` lexicographically. Another store cannot be
    /// ordered against this one, so that is backwards.
    fn progress(&self, previous: &Self) -> Progress {
        if self.db_id != previous.db_id {
            return Progress::Backward;
        }
        ordered(
            (self.time_updated_ms, self.last_id.as_str()),
            (previous.time_updated_ms, previous.last_id.as_str()),
        )
    }
    fn describe(&self) -> String {
        format!("row {} at {} ms", self.last_id, self.time_updated_ms)
    }
}

/// A sequence number into a SQLite store's event log (OpenCode 2.x).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeqCursor {
    pub db_id: String,
    pub seq: u64,
}

impl CursorCodec for SeqCursor {
    const KIND: &'static str = "sqlite-seq";
    const VERSION: u32 = 1;

    fn encode(&self) -> serde_json::Value {
        encode_state(self)
    }
    fn decode(version: u32, _epoch: u64, state: &serde_json::Value) -> Result<Self, CursorError> {
        if version != 1 {
            return Err(CursorError::Version(version));
        }
        decode_state(state)
    }
    fn progress(&self, previous: &Self) -> Progress {
        if self.db_id != previous.db_id {
            return Progress::Backward;
        }
        ordered(self.seq, previous.seq)
    }
    fn describe(&self) -> String {
        format!("seq {}", self.seq)
    }
}

/// How far a spool of hook payloads has been assembled (Cursor).
///
/// A spool is numbered from its creation, so the number orders rows of one
/// spool only. `spool_id` names the spool the number counts in; a spool
/// created again under the same key has a new id and numbers from the start,
/// and its reader opens the next epoch ([`EpochReason::Recreated`]) rather
/// than have the new spool's rows read as behind the old one's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolCursor {
    /// The spool's identity, fixed when it is created.
    pub spool_id: String,
    pub assembled_through: u64,
    pub epoch: u64,
    /// Why `epoch` was opened; `None` in epoch 0.
    pub epoch_reason: Option<EpochReason>,
}

impl SpoolCursor {
    /// Rows of the spool `spool_id` assembled through `assembled_through`,
    /// in epoch 0.
    pub fn new(spool_id: impl Into<String>, assembled_through: u64) -> Self {
        Self {
            spool_id: spool_id.into(),
            assembled_through,
            epoch: 0,
            epoch_reason: None,
        }
    }
}

/// The stored state of a [`SpoolCursor`], version 1.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpoolStateV1 {
    spool_id: String,
    assembled_through: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epoch_reason: Option<EpochReason>,
}

impl CursorCodec for SpoolCursor {
    const KIND: &'static str = "spool";
    const VERSION: u32 = 1;

    fn encode(&self) -> serde_json::Value {
        encode_state(SpoolStateV1 {
            spool_id: self.spool_id.clone(),
            assembled_through: self.assembled_through,
            epoch_reason: self.epoch_reason,
        })
    }
    fn decode(version: u32, epoch: u64, state: &serde_json::Value) -> Result<Self, CursorError> {
        if version != 1 {
            return Err(CursorError::Version(version));
        }
        let state: SpoolStateV1 = decode_state(state)?;
        Ok(Self {
            spool_id: state.spool_id,
            assembled_through: state.assembled_through,
            epoch,
            epoch_reason: state.epoch_reason,
        })
    }
    /// The epoch first. Within one epoch, rows of two different spools
    /// cannot be ordered, so that is backwards (the stored cursor is kept
    /// until the reader opens the next epoch); within one spool, the number.
    fn progress(&self, previous: &Self) -> Progress {
        match self.epoch.cmp(&previous.epoch) {
            std::cmp::Ordering::Greater => return Progress::NewEpoch,
            std::cmp::Ordering::Less => return Progress::Backward,
            std::cmp::Ordering::Equal => {}
        }
        if self.spool_id != previous.spool_id {
            return Progress::Backward;
        }
        ordered(self.assembled_through, previous.assembled_through)
    }
    fn describe(&self) -> String {
        format!("spool row {}", self.assembled_through)
    }
    fn epoch(&self) -> u64 {
        self.epoch
    }
    fn in_epoch(self, epoch: u64, reason: EpochReason) -> Option<Self> {
        Some(Self {
            epoch,
            epoch_reason: Some(reason),
            ..self
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_migrated_claude_code_state_decodes_and_encodes_back_to_itself() {
        let migrated = json!({"path": "/p/s1.jsonl", "byte_offset": 42});
        let cursor = JsonlCursor::decode(1, 0, &migrated).unwrap();
        assert_eq!(cursor, JsonlCursor::start("/p/s1.jsonl").at(42));
        assert_eq!(cursor.encode(), migrated);
        assert_eq!(cursor.describe(), "byte 42");
    }

    #[test]
    fn unknown_fields_and_versions_are_refused() {
        for state in [
            json!({"path": "/p", "byte_offset": 1, "carry": "kept"}),
            json!({"path": "/p", "byte_offset": 1, "extra": 1}),
            json!({"path": "/p"}),
            json!({"byte_offset": 1}),
            json!("not an object"),
        ] {
            assert!(
                matches!(
                    JsonlCursor::decode(1, 0, &state),
                    Err(CursorError::State(_))
                ),
                "{state}"
            );
        }
        assert_eq!(
            JsonlCursor::decode(2, 0, &json!({"path": "/p", "byte_offset": 1})),
            Err(CursorError::Version(2))
        );
        assert!(matches!(
            SeqCursor::decode(1, 0, &json!({"db_id": "d", "seq": 1, "x": 0})),
            Err(CursorError::State(_))
        ));
    }

    #[test]
    fn every_codec_round_trips() {
        let jsonl = JsonlCursor {
            path: "/r.jsonl".into(),
            file_id: Some(FileId {
                sha256: "ab".repeat(32),
                len: 9,
            }),
            byte_offset: 7,
            last_ordinal: Some(3),
            epoch: 2,
            epoch_reason: Some(EpochReason::Migrated),
            carry: Carry {
                title_rank: Some(crate::harness::candidate::TitleRank::UserSet),
                backlog: true,
                stall_window: Some(1 << 20),
                extra: Some(json!({"regime": "paginated"})),
            },
        };
        assert_eq!(JsonlCursor::decode(1, 2, &jsonl.encode()).unwrap(), jsonl);
        let watermark = SqliteWatermark {
            db_id: "d".into(),
            time_updated_ms: 5,
            last_id: "ses_1".into(),
            overlap_ms: 1000,
        };
        assert_eq!(
            SqliteWatermark::decode(1, 0, &watermark.encode()).unwrap(),
            watermark
        );
        let seq = SeqCursor {
            db_id: "d".into(),
            seq: 41,
        };
        assert_eq!(SeqCursor::decode(1, 0, &seq.encode()).unwrap(), seq);
        let spool = SpoolCursor::new("spool-1", 8);
        assert_eq!(SpoolCursor::decode(1, 0, &spool.encode()).unwrap(), spool);
        let recreated = SpoolCursor::new("spool-2", 3)
            .in_epoch(1, EpochReason::Recreated)
            .unwrap();
        assert_eq!(
            SpoolCursor::decode(1, 1, &recreated.encode()).unwrap(),
            recreated
        );
    }

    /// A recreated spool numbers from the start: in the same epoch it cannot
    /// be ordered against the old one, and in the next it is ahead.
    #[test]
    fn a_recreated_spool_opens_the_next_epoch_rather_than_reading_as_behind() {
        let old = SpoolCursor::new("spool-1", 40);
        assert_eq!(
            SpoolCursor::new("spool-1", 41).progress(&old),
            Progress::Forward
        );
        assert_eq!(
            SpoolCursor::new("spool-1", 39).progress(&old),
            Progress::Backward
        );
        let restarted = SpoolCursor::new("spool-2", 2);
        assert_eq!(restarted.progress(&old), Progress::Backward);
        let reopened = restarted.in_epoch(1, EpochReason::Recreated).unwrap();
        assert_eq!(reopened.progress(&old), Progress::NewEpoch);
        assert_eq!(
            reopened.encode(),
            serde_json::json!({"spool_id": "spool-2", "assembled_through": 2, "epoch_reason": "recreated"})
        );
    }

    #[test]
    fn a_jsonl_cursor_orders_by_epoch_then_ordinal_then_offset() {
        let base = JsonlCursor::start("/a").at(100);
        assert_eq!(base.at(101).progress(&base), Progress::Forward);
        assert_eq!(base.at(100).progress(&base), Progress::Same);
        assert_eq!(base.at(99).progress(&base), Progress::Backward);
        // Moving the record is not a step back.
        let moved = JsonlCursor {
            path: "/b".into(),
            ..base.clone()
        };
        assert_eq!(moved.progress(&base), Progress::Same);
        let next = base
            .clone()
            .at(0)
            .in_epoch(1, EpochReason::Rewritten)
            .unwrap();
        assert_eq!(next.progress(&base), Progress::NewEpoch);
        assert_eq!(base.progress(&next), Progress::Backward);
        // Ordinals win over offsets when both have one.
        let a = JsonlCursor {
            last_ordinal: Some(5),
            ..base.at(900)
        };
        let b = JsonlCursor {
            last_ordinal: Some(6),
            ..base.at(10)
        };
        assert_eq!(b.progress(&a), Progress::Forward);
        // Two record identities cannot be ordered.
        let one = JsonlCursor {
            file_id: Some(FileId {
                sha256: "0".repeat(64),
                len: 1,
            }),
            ..base.clone()
        };
        let other = JsonlCursor {
            file_id: Some(FileId {
                sha256: "1".repeat(64),
                len: 1,
            }),
            ..base.at(500)
        };
        assert_eq!(other.progress(&one), Progress::Backward);
    }

    #[test]
    fn a_stored_row_of_another_kind_is_not_read() {
        let stored = crate::db::StoredHarnessCursor {
            harness: "codex".into(),
            cursor_key: "codex:t".into(),
            kind: "spool".into(),
            version: 1,
            epoch: 0,
            state: json!({"assembled_through": 1}),
            updated_at: 0,
        };
        assert!(matches!(
            JsonlCursor::decode_stored(&stored),
            Err(CursorError::Kind { .. })
        ));
        assert!(matches!(
            SpoolCursor::decode_stored(&stored),
            Err(CursorError::State(_))
        ));
        let stored = crate::db::StoredHarnessCursor {
            state: json!({"spool_id": "s", "assembled_through": 1}),
            ..stored
        };
        assert_eq!(
            SpoolCursor::decode_stored(&stored).unwrap(),
            SpoolCursor::new("s", 1)
        );
    }

    #[test]
    fn watermarks_order_lexicographically_within_one_store() {
        let at = |time, id: &str| SqliteWatermark {
            db_id: "d".into(),
            time_updated_ms: time,
            last_id: id.into(),
            overlap_ms: 0,
        };
        assert_eq!(at(5, "b").progress(&at(5, "a")), Progress::Forward);
        assert_eq!(at(4, "z").progress(&at(5, "a")), Progress::Backward);
        let elsewhere = SqliteWatermark {
            db_id: "e".into(),
            ..at(9, "z")
        };
        assert_eq!(elsewhere.progress(&at(5, "a")), Progress::Backward);
    }
}
