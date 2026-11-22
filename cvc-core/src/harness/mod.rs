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
//! through [`with_adapter`].
//!
//! The engine records only for the adapter registered for its harness: any
//! other type that implements the traits, inside this crate or outside it,
//! records nothing, whichever harness it names. A harness without a line is
//! known (its id, capture source, namespace and channel are fixed in [`id`])
//! but not available: [`HarnessId::available`] is false, and nothing records
//! under its provenance.
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

        /// The type of the adapter registered for `id`, if this build has one.
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

/// Whether `A` is the adapter registered for its harness, the only adapter
/// the engine records for.
pub(crate) fn is_registered<A: HarnessAdapter>() -> bool {
    let registered = registered_adapter(A::ID) == Some(std::any::TypeId::of::<A>());
    #[cfg(test)]
    let registered = registered || test_registry::registered::<A>();
    registered
}

/// This crate's own unit tests register a stand-in adapter for a harness that
/// has none in the build, on the test's own thread. It exists in no other
/// build: nothing outside the crate's unit tests can register an adapter.
#[cfg(test)]
pub(crate) mod test_registry {
    use super::{registered_adapter, HarnessAdapter, HarnessId};
    use std::any::TypeId;
    use std::cell::RefCell;

    thread_local! {
        static ADAPTERS: RefCell<Vec<(HarnessId, TypeId)>> = const { RefCell::new(Vec::new()) };
    }

    /// Registers `A` for its harness on this thread. A harness that has an
    /// adapter in the build cannot be registered again.
    pub(crate) fn register<A: HarnessAdapter>() {
        assert!(
            registered_adapter(A::ID).is_none(),
            "{} has an adapter in this build",
            A::ID
        );
        ADAPTERS.with(|adapters| adapters.borrow_mut().push((A::ID, TypeId::of::<A>())));
    }

    pub(super) fn registered<A: HarnessAdapter>() -> bool {
        ADAPTERS.with(|adapters| adapters.borrow().contains(&(A::ID, TypeId::of::<A>())))
    }
}

#[cfg(test)]
mod port_tests;

#[cfg(test)]
mod registry_tests {
    use super::*;
    use crate::harness::adapter::OutputContract;

    /// A harness's adapter and its channel's notice come together: an adapter
    /// without a notice could never be acknowledged, and its users would be
    /// told to acknowledge again for ever; a notice without an adapter would
    /// let people consent to capture that does not exist.
    #[test]
    fn a_harness_is_available_exactly_when_its_channel_has_a_notice() {
        for harness in HarnessId::ALL {
            assert_eq!(
                harness.available(),
                harness.channel().notice().is_some(),
                "{harness}"
            );
        }
    }

    struct Contract;

    impl AdapterVisitor for Contract {
        type Output = OutputContract;
        fn visit<A: HarnessAdapter>(self, adapter: &A) -> OutputContract {
            adapter.contract(None)
        }
    }

    /// What a hook refused before its adapter runs prints is what the
    /// adapter itself prints.
    #[test]
    fn the_static_hook_contract_is_the_adapters_own() {
        for harness in HarnessId::ALL {
            if let Some(contract) = with_adapter(harness, Contract) {
                assert_eq!(contract, harness.hook_contract(), "{harness}");
            }
        }
    }
}
