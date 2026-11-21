//! Agent harnesses: the agents whose own session records CVC captures from.
//!
//! [`id`] fixes who each harness is (its names, capture-source value,
//! conversation-id namespace and consent channel) for every harness CVC
//! knows, whether or not this build can capture from it yet.
//!
//! An adapter implements the traits in [`adapter`]: it locates and reads the
//! agent's record, and plans [`candidate::CaptureCandidate`]s from it, with a
//! [`cursor`] that says where the next read resumes and the shared [`drift`]
//! rule for what it does not recognise. Every candidate becomes a capture the
//! same way, through [`seal`]: scrubbed with the capture's policy, then
//! bounded, then marked.
pub mod adapter;
pub mod candidate;
pub mod cursor;
pub mod drift;
pub mod family;
pub(crate) mod fields;
pub mod id;
pub(crate) mod seal;

pub use id::{
    derived_interaction_id, is_reserved_conversation_id, HarnessId, IdError, PassiveChannel,
    MAX_CONVERSATION_ID_BYTES, SUBAGENT_MARKER,
};
