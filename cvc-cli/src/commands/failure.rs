//! Typed failures of the share, unshare, publish and acknowledge flows, and a
//! classifier a caller that is not a command line (the interactive view) can use
//! to word them itself.
//!
//! The commands print `{error:#}` and nothing here changes that text: each typed
//! error's `Display` is exactly the string the flow used to produce, except
//! that a publication bundled with a share (`cvc share --push`) now says in
//! front that the share was recorded ([`BundledPublishFailed`]). Where only
//! `cvc-core` knows the cause, as `DbError::Migration(message)` (a variant that
//! also stands for refusals unrelated to migrations), the classifier matches the
//! exact message, and a unit test pins each against the real code.

use cvc_core::db::DbError;
use cvc_core::privacy::PrivacyError;
use std::error::Error;
use std::fmt;

/// A consent challenge was not confirmed, and why. No consent was recorded.
///
/// Every reason reads the same on the command line, `acknowledgement challenge
/// did not match`: that text is what the commands have always printed and what
/// scripts may match on, and where a reason is also worth saying to a person it
/// is added in front as context (see `answer_error` in `sync.rs`). The reason is
/// for a caller that words the refusal itself, as the interactive view does, so
/// that a challenge that was never asked is not reported as a phrase that did
/// not match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChallengeNotConfirmed {
    /// A phrase was asked and what was typed was not it, or was empty.
    Mismatch,
    /// A phrase was asked and what came back was not text at all: bytes that
    /// are not UTF-8, or a line longer than any phrase. It cannot match either.
    Unreadable,
    /// No phrase was asked: input kept arriving at the terminal, without a quiet
    /// moment, for longer than the prompt waits for one, so the prompt was not
    /// shown. Something may be sending a phrase in advance, and a person who
    /// answers after seeing the prompt cannot be told from it.
    InputKeptArriving,
    /// No phrase was asked: the terminal's line editing and echo, switched off
    /// while waiting for the input to settle, could not be switched back on, and
    /// a phrase typed with echo off is not one a person can see themselves give.
    TerminalNotRestored,
}

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

/// `cvc unshare` found, before it asked for its phrase, that the store would
/// refuse the unshare: some thought of the conversation is published, pending
/// or of unknown state at the destination. No phrase was asked and nothing
/// changed.
///
/// `Display` is the reason, in the words the interactive view gives for the same
/// check (`tui::words::unshare_refusal`, which names `cvc privacy reconcile`
/// when the cause is an unfinished publish). It is the store's own refusal made
/// early, and classifies as that refusal does ([`FlowFailure::UnshareBlocked`]);
/// the store's refusal after the phrase stays the authority.
#[derive(Debug)]
pub(crate) struct UnshareRefused {
    reason: String,
}

impl UnshareRefused {
    pub(crate) fn new(reason: String) -> Self {
        Self { reason }
    }
}

impl fmt::Display for UnshareRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for UnshareRefused {}

/// A publication stopped after its push began, so the local records do not say
/// what the destination holds of it. Either the transport failed in a way that
/// does not say whether the destination received the objects (the thoughts the
/// push carried are marked `unknown`), or the push went through and recording
/// that failed (they stay `pending`). The destination may hold some, all or none
/// of it; `cvc privacy reconcile` settles it against a fresh fetch.
///
/// Transparent: `Display` and `source` are the wrapped error's own, so wrapping
/// changes nothing a command prints. It exists to be found by
/// [`classify_failure`].
#[derive(Debug)]
pub(crate) struct PublicationOutcomeUnknown {
    cause: Box<dyn Error + Send + Sync + 'static>,
}

impl PublicationOutcomeUnknown {
    pub(crate) fn new(cause: impl Error + Send + Sync + 'static) -> Self {
        Self {
            cause: Box::new(cause),
        }
    }
}

impl fmt::Display for PublicationOutcomeUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.cause, f)
    }
}

impl Error for PublicationOutcomeUnknown {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause.source()
    }
}

/// The publication that `cvc share --push` runs after its share did not
/// complete. By then the share is recorded: the `I SHARE` phrase was answered
/// and the thoughts are marked shared. Whatever stopped the publication, a
/// refusal that on its own means nothing happened (`I PUBLISH` not matching,
/// consent missing) included, it must not be read that way here.
///
/// It says so in front of the cause, so the command's `{error:#}` reads, for
/// example, `the share was recorded, but publishing did not complete:
/// acknowledgement challenge did not match`.
#[derive(Debug)]
pub(crate) struct BundledPublishFailed {
    cause: anyhow::Error,
}

impl BundledPublishFailed {
    /// `cause`, the error the bundled publication returned, marked as the failure
    /// of a publication that followed a recorded share.
    pub(crate) fn wrap(cause: anyhow::Error) -> anyhow::Error {
        anyhow::Error::new(Self { cause })
    }
}

impl fmt::Display for BundledPublishFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the share was recorded, but publishing did not complete")
    }
}

impl Error for BundledPublishFailed {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&*self.cause)
    }
}

/// What `cvc-core` says when a share's snapshot no longer matches the
/// conversation (a thought was captured while the challenge was open). Nothing was
/// shared.
const CONVERSATION_CHANGED: &str = "conversation changed during authorization; retry";

/// What `cvc-core` says when an unshare is refused because some thought is
/// pending, published or of unknown state at the destination. Nothing changed.
const UNSHARE_BLOCKED: &str = "cannot unshare pending, published, or unknown content";

/// Why a flow stopped, in terms a caller can translate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FlowFailure {
    /// The typed phrase did not match, was empty, or was not text at all (bytes
    /// that are not UTF-8, a line longer than any phrase). It is what the flow's
    /// own consent gate says when it is not satisfied, and the gate comes before the
    /// flow writes anything it asks consent for, so this guarantees:
    ///
    /// - no consent, share or unshare was recorded, and no publication state was
    ///   set for anything the flow was about to carry;
    /// - nothing was sent to the destination.
    ///
    /// It does not mean the local store is untouched. A publish fetches the
    /// destination's baseline and imports it before it asks, which updates the
    /// local cache and settles earlier uncertain publications against what was
    /// fetched, exactly as `cvc privacy reconcile` does.
    ///
    /// It is never what the second challenge of `cvc share --push` produces: the
    /// `I SHARE` phrase was answered by then and the share is recorded, so that
    /// mismatch is [`FlowFailure::SharedButNotPublished`].
    ///
    /// A phrase was asked. When none was, see [`FlowFailure::InputKeptArriving`]
    /// and [`FlowFailure::TerminalNotRestored`], which guarantee the same.
    NotConfirmed,
    /// No phrase was asked, because input kept arriving at the terminal without
    /// a quiet moment (see [`ChallengeNotConfirmed::InputKeptArriving`]). The
    /// guarantees of [`FlowFailure::NotConfirmed`] hold, and a phrase that
    /// arrived with the input answered nothing.
    InputKeptArriving,
    /// No phrase was asked, because the terminal's line editing and echo could
    /// not be switched back on (see [`ChallengeNotConfirmed::TerminalNotRestored`]).
    /// The guarantees of [`FlowFailure::NotConfirmed`] hold.
    TerminalNotRestored,
    /// The conversation changed while the challenge was open, so the snapshot
    /// the phrase counted is stale. Nothing was shared; try again.
    ConversationChanged,
    /// Unsharing was refused: some thought is already (or may be) published at
    /// the destination. Nothing changed. Either the store refused it, after the
    /// phrase, or [`UnshareRefused`] did, before any phrase was asked.
    UnshareBlocked,
    /// Publishing needs the destination's sharing consent and there is none.
    ConsentMissing,
    /// A publish stopped after its push began, so what the destination holds of
    /// it is not settled in the local records: it may hold some, all or none of
    /// it. The text is the failure's plain message. `cvc privacy reconcile`
    /// settles it. See [`PublicationOutcomeUnknown`].
    OutcomeUnknown(String),
    /// `cvc share --push`: the share was recorded and the publication that
    /// followed it in the same command did not complete, for the reason inside.
    /// The share stands whatever the reason is; a reason that would mean
    /// "nothing was recorded" on its own, as [`FlowFailure::NotConfirmed`] does,
    /// only describes the publication here.
    SharedButNotPublished(Box<FlowFailure>),
    /// Anything else, as one line of text with the misleading prefixes
    /// (`Migration error:`, `invalid .thoughtignore:`) removed.
    Other(String),
}

/// Classifies the error a flow returned. It looks through added context, so a
/// wrapped error classifies the same as the bare one.
///
/// The order matters. A bundled publication's failure is looked for first: the
/// share before it is recorded, so what failed inside is classified as what
/// happened to the publication, not to the flow. Then an unsettled publication,
/// and only then the typed refusals.
pub(crate) fn classify_failure(error: &anyhow::Error) -> FlowFailure {
    if let Some(bundled) = error.downcast_ref::<BundledPublishFailed>() {
        return FlowFailure::SharedButNotPublished(Box::new(classify_failure(&bundled.cause)));
    }
    if error.downcast_ref::<PublicationOutcomeUnknown>().is_some() {
        return FlowFailure::OutcomeUnknown(plain_message(error));
    }
    if let Some(not_confirmed) = error.downcast_ref::<ChallengeNotConfirmed>() {
        return match not_confirmed {
            ChallengeNotConfirmed::Mismatch | ChallengeNotConfirmed::Unreadable => {
                FlowFailure::NotConfirmed
            }
            ChallengeNotConfirmed::InputKeptArriving => FlowFailure::InputKeptArriving,
            ChallengeNotConfirmed::TerminalNotRestored => FlowFailure::TerminalNotRestored,
        };
    }
    if error.downcast_ref::<SharingConsentMissing>().is_some() {
        return FlowFailure::ConsentMissing;
    }
    if error.downcast_ref::<UnshareRefused>().is_some() {
        return FlowFailure::UnshareBlocked;
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
            ChallengeNotConfirmed::Mismatch.to_string(),
            "acknowledgement challenge did not match"
        );
        assert_eq!(
            classify(ChallengeNotConfirmed::Mismatch),
            FlowFailure::NotConfirmed
        );
        let wrapped = anyhow::Error::from(ChallengeNotConfirmed::Mismatch).context("while sharing");
        assert_eq!(classify_failure(&wrapped), FlowFailure::NotConfirmed);
    }

    #[test]
    fn every_reason_a_challenge_was_not_confirmed_reads_the_same_on_the_command_line() {
        for reason in [
            ChallengeNotConfirmed::Mismatch,
            ChallengeNotConfirmed::Unreadable,
            ChallengeNotConfirmed::InputKeptArriving,
            ChallengeNotConfirmed::TerminalNotRestored,
        ] {
            assert_eq!(
                reason.to_string(),
                "acknowledgement challenge did not match"
            );
        }
    }

    #[test]
    fn a_challenge_that_was_never_asked_is_told_apart_from_a_phrase_that_did_not_match() {
        // A phrase was asked, and what came back was wrong or was not text.
        assert_eq!(
            classify(ChallengeNotConfirmed::Mismatch),
            FlowFailure::NotConfirmed
        );
        assert_eq!(
            classify(ChallengeNotConfirmed::Unreadable),
            FlowFailure::NotConfirmed
        );
        // No phrase was asked.
        assert_eq!(
            classify(ChallengeNotConfirmed::InputKeptArriving),
            FlowFailure::InputKeptArriving
        );
        assert_eq!(
            classify(ChallengeNotConfirmed::TerminalNotRestored),
            FlowFailure::TerminalNotRestored
        );
        // Through the context the command line adds in front, and inside a
        // publication bundled with a share.
        let said_why = anyhow::Error::from(ChallengeNotConfirmed::InputKeptArriving)
            .context("input kept arriving at the terminal, so no phrase was asked");
        assert_eq!(classify_failure(&said_why), FlowFailure::InputKeptArriving);
        assert_eq!(
            classify_failure(&BundledPublishFailed::wrap(said_why)),
            FlowFailure::SharedButNotPublished(Box::new(FlowFailure::InputKeptArriving))
        );
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

        // The same refusal, made by `unshare_conversation` before any phrase is
        // asked, classifies the same and says why in the view's words. Nothing
        // here is a terminal, so a phrase that was asked would have failed on
        // that instead, and would not classify as a refusal.
        use cvc_core::privacy::remote_destination;
        let directory = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        repo.remote("origin", "https://example.invalid/r.git")
            .unwrap();
        let destination = remote_destination(&repo, "origin").unwrap();
        let store = CvcStore::open(directory.path().join("index.db")).unwrap();
        let shared = |conversation: &str, state: PublicationState| {
            let id = capture(&store, conversation);
            let snapshot = store.share_snapshot(conversation).unwrap();
            store
                .share_exact_snapshot(
                    conversation,
                    &destination.fingerprint,
                    &snapshot,
                    FutureSharePolicy::Private,
                )
                .unwrap();
            store
                .mark_publication(&[id], &destination.fingerprint, state)
                .unwrap();
        };
        shared("published", PublicationState::Published);
        shared("unfinished", PublicationState::Pending);
        for (conversation, said) in [
            (
                "published",
                "Cannot unshare: 1 thought is already published to 'origin', and published thoughts cannot be recalled.",
            ),
            (
                "unfinished",
                "Cannot unshare yet: an earlier publish to 'origin' did not finish cleanly. Run: cvc privacy reconcile --remote origin",
            ),
        ] {
            let error = crate::commands::sync::unshare_conversation(
                &repo,
                &store,
                &destination,
                conversation,
            )
            .expect_err("the store would refuse this unshare");
            assert_eq!(classify_failure(&error), FlowFailure::UnshareBlocked);
            assert_eq!(format!("{error:#}"), said);
        }
    }

    /// An error with a source of its own, as a transport's errors have.
    #[derive(Debug)]
    struct Transport(Reset);

    #[derive(Debug)]
    struct Reset;

    impl fmt::Display for Transport {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("push failed")
        }
    }

    impl fmt::Display for Reset {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("connection reset by peer")
        }
    }

    impl Error for Reset {}

    impl Error for Transport {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn an_unsettled_publication_prints_exactly_what_it_wrapped() {
        let bare = anyhow::Error::from(Transport(Reset));
        let wrapped = anyhow::Error::from(PublicationOutcomeUnknown::new(Transport(Reset)));
        assert_eq!(format!("{bare:#}"), "push failed: connection reset by peer");
        assert_eq!(format!("{wrapped:#}"), format!("{bare:#}"));
        assert_eq!(wrapped.to_string(), bare.to_string());
        assert_eq!(wrapped.chain().count(), bare.chain().count());
        let again: Vec<_> = wrapped.chain().map(ToString::to_string).collect();
        assert_eq!(again, ["push failed", "connection reset by peer"]);
        // The same through added context, and for a real cvc-core error.
        let context = |error: anyhow::Error| format!("{:#}", error.context("while publishing"));
        assert_eq!(context(wrapped), context(bare));
        let sync = cvc_core::sync::SyncError::Ref("invalid temporary ref".into());
        let expected = format!("{:#}", anyhow::Error::from(sync));
        let sync = cvc_core::sync::SyncError::Ref("invalid temporary ref".into());
        let wrapped = anyhow::Error::from(PublicationOutcomeUnknown::new(sync));
        assert_eq!(format!("{wrapped:#}"), expected);
        assert_eq!(expected, "Reference error: invalid temporary ref");
    }

    #[test]
    fn an_unsettled_publication_classifies_as_outcome_unknown_with_its_message() {
        let error = anyhow::Error::from(PublicationOutcomeUnknown::new(Transport(Reset)));
        assert_eq!(
            classify_failure(&error),
            FlowFailure::OutcomeUnknown("push failed: connection reset by peer".into())
        );
        let context = error.context("while publishing");
        assert_eq!(
            classify_failure(&context),
            FlowFailure::OutcomeUnknown(
                "while publishing: push failed: connection reset by peer".into()
            )
        );
        // A failure that is only a store error is not one.
        assert_eq!(
            classify(DbError::Migration("x".into())),
            FlowFailure::Other("x".into())
        );
    }

    #[test]
    fn a_publication_that_stops_after_a_recorded_share_is_never_read_as_nothing_recorded() {
        let bundled = |cause: anyhow::Error| classify_failure(&BundledPublishFailed::wrap(cause));
        let after_share = |inner| FlowFailure::SharedButNotPublished(Box::new(inner));

        // `I PUBLISH` did not match. On its own that type means that nothing was
        // recorded; after `I SHARE` it does not.
        assert_eq!(
            bundled(ChallengeNotConfirmed::Mismatch.into()),
            after_share(FlowFailure::NotConfirmed)
        );
        assert_eq!(
            classify(ChallengeNotConfirmed::Mismatch),
            FlowFailure::NotConfirmed
        );
        assert_eq!(
            bundled(
                SharingConsentMissing {
                    remote: "origin".into()
                }
                .into()
            ),
            after_share(FlowFailure::ConsentMissing)
        );
        assert_eq!(
            bundled(PublicationOutcomeUnknown::new(Transport(Reset)).into()),
            after_share(FlowFailure::OutcomeUnknown(
                "push failed: connection reset by peer".into()
            ))
        );
        // The message of anything else is the cause's, without the sentence that
        // says the share was recorded.
        assert_eq!(
            bundled(anyhow::anyhow!("disk is full").context("while publishing")),
            after_share(FlowFailure::Other("while publishing: disk is full".into()))
        );
        // Context added further out changes nothing.
        let outer =
            BundledPublishFailed::wrap(ChallengeNotConfirmed::Mismatch.into()).context("share");
        assert_eq!(
            classify_failure(&outer),
            after_share(FlowFailure::NotConfirmed)
        );
    }

    #[test]
    fn a_bundled_failure_says_the_share_was_recorded_in_front_of_its_cause() {
        let error = BundledPublishFailed::wrap(ChallengeNotConfirmed::Mismatch.into());
        assert_eq!(
            format!("{error:#}"),
            "the share was recorded, but publishing did not complete: acknowledgement challenge did not match"
        );
        let error =
            BundledPublishFailed::wrap(PublicationOutcomeUnknown::new(Transport(Reset)).into());
        assert_eq!(
            format!("{error:#}"),
            "the share was recorded, but publishing did not complete: push failed: connection reset by peer"
        );
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
