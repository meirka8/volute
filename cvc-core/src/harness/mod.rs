//! Agent harnesses: the agents whose own session records CVC captures from.
//!
//! [`id`] fixes who each harness is (its names, capture-source value,
//! conversation-id namespace and consent channel) for every harness CVC
//! knows, whether or not this build can capture from it yet.
//!
//! An adapter implements the traits in [`adapter`]: it locates and reads the
//! agent's record, and plans [`candidate::CaptureCandidate`]s from it, with a
//! [`cursor`] that says where the next read resumes and the shared [`drift`]
//! rule for what it does not recognise. Everything else is shared: the
//! [`engine`] runs a family's ingest under one [`lock`] and one consent
//! check per batch, every candidate becomes a capture the same way through
//! [`seal`] (scrubbed with the capture's policy, then bounded, then marked),
//! and the [`family`] rescue links commits the session made.
//!
//! # The registry
//!
//! The adapters this build has are listed once, in the `harnesses!`
//! invocation below. Adding a harness is one line there; the traits'
//! associated types rule out trait objects, so dispatch is a generic call
//! through [`with_adapter`]. A harness without a line is known (its id,
//! capture source, namespace and channel are fixed in [`id`]) but not
//! available: [`HarnessId::available`] is false, and nothing can capture from
//! it.
pub mod adapter;
pub mod candidate;
pub mod cursor;
pub mod drift;
pub mod engine;
pub mod family;
pub mod fields;
pub mod id;
pub mod lock;
pub mod record;
pub mod seal;

pub use adapter::HarnessAdapter;
pub use id::{
    derived_interaction_id, is_reserved_conversation_id, HarnessId, IdError, PassiveChannel,
    MAX_CONVERSATION_ID_BYTES, SUBAGENT_MARKER,
};
pub use lock::SESSION_LOCK_WAIT;

/// What [`with_adapter`] calls with the adapter of a harness.
pub trait AdapterVisitor {
    type Output;
    fn visit<A: HarnessAdapter>(self, adapter: &A) -> Self::Output;
}

macro_rules! harnesses {
    ($($id:ident => $adapter:ty),* $(,)?) => {
        impl HarnessId {
            /// Whether this build has an adapter for the harness. One that is
            /// known but not available captures nothing.
            #[allow(unreachable_patterns)]
            pub const fn available(self) -> bool {
                match self {
                    $(HarnessId::$id => true,)*
                    _ => false,
                }
            }
        }

        /// Calls `visitor` with the adapter of `id`, or returns `None` when
        /// this build has none.
        #[allow(unreachable_patterns)]
        pub fn with_adapter<V: AdapterVisitor>(id: HarnessId, visitor: V) -> Option<V::Output> {
            match id {
                $(HarnessId::$id => Some(visitor.visit(&<$adapter as Default>::default())),)*
                _ => None,
            }
        }

        /// The type of the adapter registered for `id`. The engine records
        /// nothing for an adapter that claims a registered harness and is not
        /// its registered adapter.
        #[allow(unreachable_patterns)]
        pub(crate) fn registered_adapter(id: HarnessId) -> Option<std::any::TypeId> {
            match id {
                $(HarnessId::$id => Some(std::any::TypeId::of::<$adapter>()),)*
                _ => None,
            }
        }
    };
}

harnesses! {
    ClaudeCode => crate::claude_code::Adapter,
    // Codex => crate::codex::Adapter,             (#190)
    // OpenCode => crate::opencode::Adapter,       (#194)
    // Cursor => crate::cursor::Adapter,           (#198)
    // Antigravity => crate::antigravity::Adapter, (#202)
}
