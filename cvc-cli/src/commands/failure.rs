//! Typed failures of the share, unshare, publish and acknowledge flows, and a
//! classifier a caller that is not a command line (the interactive view) can use
//! to word them itself.
//!
//! The commands print `{error:#}` and nothing here changes that text: each typed
//! error's `Display` is exactly the string the flow used to produce. Where only
//! `cvc-core` knows the cause, as `DbError::Migration(message)` (a variant that
//! also stands for refusals unrelated to migrations), the classifier matches the
//! exact message, and a unit test pins each against the real code.

use cvc_core::db::DbError;
use cvc_core::privacy::PrivacyError;
use std::fmt;

/// The phrase typed at a challenge was not the one displayed, or was empty. No
/// consent was recorded.
#[derive(Debug)]
pub(crate) struct ChallengeNotConfirmed;

impl fmt::Display for ChallengeNotConfirmed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("acknowledgement challenge did not match")
    }
}

impl std::error::Error for ChallengeNotConfirmed {}

/// Publishing to a destination needs its standing sharing consent
/// (`cvc privacy acknowledge-sharing`), and there is none.
#[derive(Debug)]
pub(crate) struct SharingConsentMissing {
    pub remote: String,
}

impl fmt::Display for SharingConsentMissing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sharing consent is required for remote '{0}'; run cvc privacy acknowledge-sharing --remote {0}",
            self.remote
        )
    }
}

impl std::error::Error for SharingConsentMissing {}

/// What `cvc-core` says when a share's snapshot no longer matches the
/// conversation (a turn was captured while the challenge was open). Nothing was
/// shared.
const CONVERSATION_CHANGED: &str = "conversation changed during authorization; retry";

/// What `cvc-core` says when an unshare is refused because some turn is
/// pending, published or of unknown state at the destination. Nothing changed.
const UNSHARE_BLOCKED: &str = "cannot unshare pending, published, or unknown content";

/// Why a flow stopped, in terms a caller can translate.
#[allow(dead_code)] // Consumed by the interactive view (#112, Phase B).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FlowFailure {
    /// The typed phrase did not match, or was empty. Nothing was recorded.
    NotConfirmed,
    /// The conversation changed while the challenge was open, so the snapshot
    /// the phrase counted is stale. Nothing was shared; try again.
    ConversationChanged,
    /// Unsharing was refused: some turn is already (or may be) published at the
    /// destination. Nothing changed.
    UnshareBlocked,
    /// Publishing needs the destination's sharing consent and there is none.
    ConsentMissing,
    /// Anything else, as one line of text with the misleading prefixes
    /// (`Migration error:`, `invalid .thoughtignore:`) removed.
    Other(String),
}

/// Classifies the error a flow returned. It looks through added context, so a
/// wrapped error classifies the same as the bare one.
#[allow(dead_code)] // Consumed by the interactive view (#112, Phase B).
pub(crate) fn classify_failure(error: &anyhow::Error) -> FlowFailure {
    if error.downcast_ref::<ChallengeNotConfirmed>().is_some() {
        return FlowFailure::NotConfirmed;
    }
    if error.downcast_ref::<SharingConsentMissing>().is_some() {
        return FlowFailure::ConsentMissing;
    }
    if let Some(DbError::Migration(message)) = error.downcast_ref::<DbError>() {
        match message.as_str() {
            CONVERSATION_CHANGED => return FlowFailure::ConversationChanged,
            UNSHARE_BLOCKED => return FlowFailure::UnshareBlocked,
            _ => {}
        }
    }
    FlowFailure::Other(plain_message(error))
}

/// The error chain as the commands print it (`outer: inner`), except that the
/// two variants whose `Display` adds a prefix that is wrong for the user
/// (`Migration error:` on a refusal, `invalid .thoughtignore:` on any policy
/// problem) contribute their bare message.
#[allow(dead_code)] // Consumed by the interactive view (#112, Phase B).
fn plain_message(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| {
            if let Some(DbError::Migration(message)) = cause.downcast_ref::<DbError>() {
                message.clone()
            } else if let Some(PrivacyError::Policy(message)) = cause.downcast_ref::<PrivacyError>()
            {
                message.clone()
            } else {
                cause.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use chrono::Utc;
    use cvc_core::db::CvcStore;
    use cvc_core::models::{
        Author, Conversation, FutureSharePolicy, Interaction, InteractionId, PublicationState,
    };
    use cvc_core::privacy::{McpCapture, PreparedPolicy};

    fn capture(store: &CvcStore, conversation: &str) -> InteractionId {
        let id = InteractionId::new();
        store
            .capture_mcp(McpCapture::new(
                Conversation {
                    id: conversation.into(),
                    title: "failure fixture".into(),
                    created_at: Utc::now(),
                },
                Interaction {
                    id: id.clone(),
                    conversation_id: conversation.into(),
                    parent_id: None,
                    timestamp: Utc::now(),
                    author: Author::Agent,
                    user_prompt: "fixture".into(),
                    model_name: None,
                    model_cot: None,
                    model_response: None,
                    source_request_id: None,
                },
                Vec::new(),
                Vec::new(),
                PreparedPolicy::built_ins_only(),
                "a".repeat(64),
            ))
            .unwrap();
        id
    }

    fn classify(error: impl Into<anyhow::Error>) -> FlowFailure {
        classify_failure(&error.into())
    }

    #[test]
    fn a_challenge_mismatch_keeps_its_text_and_classifies_as_not_confirmed() {
        assert_eq!(
            ChallengeNotConfirmed.to_string(),
            "acknowledgement challenge did not match"
        );
        assert_eq!(classify(ChallengeNotConfirmed), FlowFailure::NotConfirmed);
        let wrapped = anyhow::Error::from(ChallengeNotConfirmed).context("while sharing");
        assert_eq!(classify_failure(&wrapped), FlowFailure::NotConfirmed);
    }

    #[test]
    fn missing_sharing_consent_keeps_its_text_and_classifies_as_consent_missing() {
        let error = SharingConsentMissing {
            remote: "origin".into(),
        };
        assert_eq!(
            error.to_string(),
            "sharing consent is required for remote 'origin'; run cvc privacy acknowledge-sharing --remote origin"
        );
        assert_eq!(classify(error), FlowFailure::ConsentMissing);
    }

    #[test]
    fn a_publish_without_consent_fails_with_the_typed_error_before_any_challenge() {
        use cvc_core::privacy::remote_destination;
        let directory = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        repo.remote("origin", "https://example.invalid/r.git")
            .unwrap();
        let store = CvcStore::open(directory.path().join("index.db")).unwrap();
        let destination = remote_destination(&repo, "origin").unwrap();

        let error = crate::commands::sync::publish_manually(&repo, &store, &destination)
            .expect_err("no consent was granted");
        assert_eq!(classify_failure(&error), FlowFailure::ConsentMissing);
        assert_eq!(
            format!("{error:#}"),
            "sharing consent is required for remote 'origin'; run cvc privacy acknowledge-sharing --remote origin"
        );
    }

    #[test]
    fn a_share_that_raced_a_new_turn_classifies_as_conversation_changed() {
        let store = CvcStore::open(":memory:").unwrap();
        let fingerprint = "7".repeat(64);
        capture(&store, "racing");
        let stale = store.share_snapshot("racing").unwrap();
        capture(&store, "racing");

        let error = store
            .share_exact_snapshot("racing", &fingerprint, &stale, FutureSharePolicy::Private)
            .expect_err("the conversation changed after the snapshot");
        // The very message the classifier matches, from the real code.
        assert!(
            matches!(&error, DbError::Migration(message) if message == CONVERSATION_CHANGED),
            "{error:?}"
        );
        assert_eq!(classify(error), FlowFailure::ConversationChanged);
    }

    #[test]
    fn a_refused_unshare_classifies_as_unshare_blocked() {
        let store = CvcStore::open(":memory:").unwrap();
        let fingerprint = "7".repeat(64);
        let id = capture(&store, "published");
        let snapshot = store.share_snapshot("published").unwrap();
        store
            .share_exact_snapshot(
                "published",
                &fingerprint,
                &snapshot,
                FutureSharePolicy::Private,
            )
            .unwrap();
        store
            .mark_publication(&[id], &fingerprint, PublicationState::Published)
            .unwrap();

        let error = store
            .unshare_conversation_for_remote("published", &fingerprint)
            .expect_err("published content cannot be unshared");
        assert!(
            matches!(&error, DbError::Migration(message) if message == UNSHARE_BLOCKED),
            "{error:?}"
        );
        let wrapped = anyhow::Error::from(error).context("unshare failed");
        assert_eq!(classify_failure(&wrapped), FlowFailure::UnshareBlocked);
    }

    #[test]
    fn other_failures_lose_the_misleading_prefixes_but_keep_their_context() {
        let migration = DbError::Migration("something else went wrong".into());
        assert!(migration.to_string().starts_with("Migration error:"));
        assert_eq!(
            classify(migration),
            FlowFailure::Other("something else went wrong".into())
        );

        let policy = PrivacyError::Policy("privacy state permissions are insecure".into());
        assert!(policy.to_string().starts_with("invalid .thoughtignore:"));
        let wrapped = Err::<(), _>(policy)
            .context("cannot read consent")
            .unwrap_err();
        assert_eq!(
            classify_failure(&wrapped),
            FlowFailure::Other(
                "cannot read consent: privacy state permissions are insecure".into()
            )
        );

        // The terminal check is not a typed failure; it keeps its text.
        assert_eq!(
            classify(anyhow::anyhow!(
                "acknowledgement requires an interactive TTY"
            )),
            FlowFailure::Other("acknowledgement requires an interactive TTY".into())
        );
    }
}
