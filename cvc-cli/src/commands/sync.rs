use crate::commands::failure::{
    BundledPublishFailed, ChallengeNotConfirmed, PublicationOutcomeUnknown, SharingConsentMissing,
};
use crate::terminal_text::sanitize_for_terminal;
use anyhow::{bail, Context, Result};
use cvc_core::repository::RepositoryLayout;
use cvc_core::{
    db::CvcStore,
    models::{FutureSharePolicy, InteractionId, PublicationState, TombstoneReasonCode},
    privacy, sync,
};
use git2::Repository;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    env,
    io::{Read, Write},
    path::Path,
};

const MAX_REDACTION_PLAN_BYTES: u64 = 64 * 1024;

fn remote(repo: &Repository, requested: Option<&str>) -> Result<String> {
    if let Some(name) = requested {
        return Ok(name.to_owned());
    }
    let remotes = repo.remotes()?;
    Ok(if remotes.iter().any(|x| x == Some("origin")) {
        "origin".into()
    } else {
        remotes.get(0).unwrap_or("origin").into()
    })
}
fn open(cwd: &Path) -> Result<(Repository, CvcStore)> {
    let layout = RepositoryLayout::discover(cwd).context("Failed to discover Git repository")?;
    let store = CvcStore::open_initialized(layout.db_path())?;
    Ok((layout.into_repository(), store))
}
pub async fn privacy_status(requested: Option<&str>) -> Result<()> {
    let (repo, _) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let s = privacy::privacy_status_for_fingerprint(&repo, &destination.fingerprint)?;
    println!("capture_acknowledged: {}\nsharing_consented: {}\nauto_push: {}\nremote: {}\ndestination_fingerprint: {}", s.capture_acknowledged, s.sharing_consented, s.auto_push, name, s.remote_fingerprint);
    Ok(())
}
pub async fn acknowledge_capture() -> Result<()> {
    let (repo, _) = open(&env::current_dir()?)?;
    typed_acknowledgement("I UNDERSTAND LOCAL CAPTURE")?;
    privacy::acknowledge_capture(&repo)?;
    println!("Capture acknowledgement saved locally; it is never synced.");
    Ok(())
}
pub async fn acknowledge_sharing(requested: Option<&str>) -> Result<()> {
    let (repo, _) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    acknowledge_sharing_for(&repo, &destination)
}

/// What `cvc privacy acknowledge-sharing --remote <destination.name>` does once
/// the destination is known: show its fingerprint, then require the typed
/// `I AUTHORIZE SHARING <fingerprint>` challenge before recording the consent.
/// The consent is bound to `destination.fingerprint` and grants nothing for
/// any other destination; it does not enable automatic push.
pub(crate) fn acknowledge_sharing_for(
    repo: &Repository,
    destination: &privacy::RemoteDestination,
) -> Result<()> {
    let s = privacy::privacy_status_for_fingerprint(repo, &destination.fingerprint)?;
    println!("Acknowledging immutable publication to remote '{}' (fingerprint {}). This does not enable automatic push.", sanitize_for_terminal(&destination.name), s.remote_fingerprint);
    typed_acknowledgement(&format!("I AUTHORIZE SHARING {}", s.remote_fingerprint))?;
    privacy::acknowledge_sharing_destination(repo, destination)?;
    Ok(())
}

/// How many conversations the share summary lookup and the picker consider,
/// most recent first. Also the ceiling for an interactive listing.
pub(crate) const SUMMARY_SCAN_LIMIT: usize = 10_000;

/// Interactive selection over conversations not yet shared for this
/// destination. TTY-only by construction — non-interactive callers must name
/// a conversation explicitly — so this adds no path around the typed
/// challenge that still follows. Returns `None` when the user aborts.
///
/// A conversation is offered when it has a thought and none of it is shared for
/// this destination, whatever else is true of it, each with the state it reads
/// as. That includes a conversation the destination already holds but this
/// clone does not share (a re-ingested session), which reads
/// `[published, not shared here]`, and one pulled from a remote. A pulled
/// conversation may be shared and published to another remote, as Git lets you
/// push to one remote what you pulled from another: it reads
/// `[published, not shared here]` for the remote it came from and, since this
/// clone has no record of any other remote holding it, `[private]` for the
/// others. Consent stays per destination, so the phrases asked are those of the
/// destination chosen. The interactive view keeps a pulled conversation
/// read-only; this picker, like `cvc share <id>`, does not.
fn pick_unshared_conversation(
    store: &CvcStore,
    remote_name: &str,
    fingerprint: &str,
) -> Result<Option<String>> {
    if !crate::tty::stdio_is_terminal() {
        bail!(
            "a conversation id is required in non-interactive use; run `cvc conversations` to list them, or run `cvc share` from a terminal for an interactive picker"
        );
    }
    let candidates: Vec<_> = store
        .list_conversation_summaries(Some(fingerprint), SUMMARY_SCAN_LIMIT)?
        .into_iter()
        .filter(|summary| !summary.shared && summary.thoughts > 0)
        .collect();
    let remote_name = sanitize_for_terminal(remote_name);
    if candidates.is_empty() {
        println!("No unshared conversations for remote '{remote_name}'.");
        return Ok(None);
    }
    println!("Unshared conversations for remote '{remote_name}' (most recent first):");
    for (index, summary) in candidates.iter().enumerate() {
        println!(
            "{:>3}. {}",
            index + 1,
            crate::commands::conversations::render_line(summary, true)
        );
    }
    print!(
        "Share which conversation? [1-{}, blank to abort]: ",
        candidates.len()
    );
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let answer = answer.trim();
    if answer.is_empty() {
        println!("Aborted; nothing was shared.");
        return Ok(None);
    }
    let choice: usize = answer
        .parse()
        .ok()
        .filter(|n| (1..=candidates.len()).contains(n))
        .ok_or_else(|| anyhow::anyhow!("invalid selection '{answer}'; nothing was shared"))?;
    Ok(Some(candidates[choice - 1].id.clone()))
}

/// A phrase is shown so that it can be read and typed back. One with a control
/// or format character in it can be shown as something else (an escape
/// sequence rewrites the lines printed above the prompt, such as the summary of
/// what is being shared) and cannot be typed, so it is not asked at all.
///
/// A stored conversation id is not how such a character gets in. Every id that
/// is stored passed the capture policy's rule, `privacy::is_safe_identifier`
/// (1 to 256 ASCII letters, digits, `.`, `_`, `-` or `:`), whichever way it was
/// captured: the MCP server, `cvc run`, the editor and a Claude Code transcript
/// all go through it, and so does a pull, which checks an imported id before it
/// namespaces it with `remote:<fingerprint>:`. What can carry one is an id typed
/// on the command line (`cvc share <id>` does not have to name a stored
/// conversation) and a conversation stored before the rule was enforced. This
/// check is for those.
fn ensure_phrase_can_be_shown(challenge: &str) -> Result<()> {
    if sanitize_for_terminal(challenge) != challenge {
        bail!(
            "the phrase to type contains control or format characters and cannot be shown, so it is not asked; nothing was recorded"
        );
    }
    Ok(())
}

/// Consent gate: the human must type `challenge` verbatim after seeing it.
///
/// The answer is read from the controlling terminal itself, not from the
/// process-global stdin, and whatever was queued before the prompt is
/// discarded first, and what is still arriving waited out (the prompt is not
/// shown until input has been quiet for a moment, and not at all if it never
/// is). Stdin is a shared buffered reader, so an earlier `read_line` (the
/// picker, or a preceding challenge in the same command) may already hold the
/// next line; answering from that would let type-ahead or a paste satisfy a
/// challenge that was never displayed. See `crate::tty`.
fn typed_acknowledgement(challenge: &str) -> Result<()> {
    if !crate::tty::stdio_is_terminal() {
        bail!("acknowledgement requires an interactive TTY");
    }
    ensure_phrase_can_be_shown(challenge)?;
    let mut terminal = crate::tty::Terminal::open()
        .context("acknowledgement requires access to the controlling terminal")?;
    terminal.discard_pending_input().map_err(answer_error)?;
    let answer = terminal
        .prompt_line(&format!("Type exactly '{challenge}' to continue: "))
        .map_err(answer_error)?;
    if answer != challenge {
        return Err(ChallengeNotConfirmed::Mismatch.into());
    }
    Ok(())
}

/// What a failed read of the answer to a challenge, or a failure to clear the
/// terminal's input before it, means. Every one that is about the answer or the
/// prompt, and not about the terminal failing, is a challenge that was not
/// confirmed, typed as `ChallengeNotConfirmed` with the reason, which is what a
/// caller that words the refusal itself tells them apart by:
///
/// - Bytes that are not text, or a line longer than any phrase, are an answer
///   that cannot match, the same as a phrase that is merely wrong.
/// - Input that kept arriving, or a terminal that could not be put back to
///   normal input after waiting for it to stop, means no phrase was asked, so
///   none was given. The command says why in front of the usual text.
///
/// A terminal that fails otherwise is a failure and says so.
fn answer_error(error: std::io::Error) -> anyhow::Error {
    if crate::tty::is_input_kept_arriving(&error) {
        anyhow::Error::from(ChallengeNotConfirmed::InputKeptArriving)
            .context("input kept arriving at the terminal, so no phrase was asked")
    } else if crate::tty::is_terminal_not_restored(&error) {
        // The system's own words are in the error's text.
        anyhow::Error::from(ChallengeNotConfirmed::TerminalNotRestored)
            .context(format!("{error}, so no phrase was asked"))
    } else if crate::tty::is_unreadable_answer(&error) {
        ChallengeNotConfirmed::Unreadable.into()
    } else {
        error.into()
    }
}
pub async fn set_auto_push(value: &str, requested: Option<&str>) -> Result<()> {
    let enabled = match value {
        "on" => true,
        "off" => false,
        _ => bail!("auto-push must be 'on' or 'off'"),
    };
    let (repo, _) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    typed_acknowledgement(&format!(
        "I AUTHORIZE AUTO PUSH {} {}",
        if enabled { "ON" } else { "OFF" },
        destination.fingerprint
    ))?;
    privacy::set_auto_push_destination(&repo, &destination, enabled)?;
    println!(
        "Auto-push {} for remote '{}'.",
        if enabled { "enabled" } else { "disabled" },
        name
    );
    Ok(())
}
pub async fn share(
    id: Option<&str>,
    future: bool,
    push: bool,
    requested: Option<&str>,
) -> Result<()> {
    let cwd = env::current_dir()?;
    let (repo, store) = open(&cwd)?;
    // Sharing must never silently select a different authority. Its sole
    // implicit destination is origin; an explicit --remote is authoritative.
    let name = requested.unwrap_or("origin");
    let destination = privacy::remote_destination(&repo, name)?;
    let picked;
    let id = match id {
        Some(id) => id,
        // The picker adds no non-interactive path: selection and the existing
        // typed challenge both require the same TTY.
        None => {
            picked = pick_unshared_conversation(&store, name, &destination.fingerprint)?;
            match &picked {
                Some(id) => id.as_str(),
                None => return Ok(()),
            }
        }
    };
    let future = if future {
        FutureSharePolicy::Shared
    } else {
        FutureSharePolicy::Private
    };
    share_conversation(&repo, &store, &destination, id, future, push).map(|_| ())
}

/// What `cvc share <conversation_id> --remote <destination.name>` does once the
/// conversation and destination are known, with `--future` as
/// `FutureSharePolicy::Shared` and `--push` as `publish_after`.
///
/// It prints the conversation summary, then requires the typed
/// `I SHARE <id> <fingerprint> <count> <token>` challenge for the exact
/// snapshot it just read, and only then records the share. The challenge goes
/// through `typed_acknowledgement`, so it needs an interactive terminal and a
/// human typing the phrase after it is displayed. With `publish_after` the
/// bundled publication follows, behind its own challenge unless the
/// destination's auto-push grant already covers it.
///
/// The destination is whatever the caller passes: this function never
/// resolves a remote. Sharing for a destination does not need that
/// destination's sharing consent (publishing does), exactly as `cvc share`.
/// It does not check where the conversation came from either; callers that
/// must not offer an imported conversation for sharing have to say so
/// themselves.
///
/// On success it reports what it recorded, so a caller that is not a command
/// line can word the result without parsing the output above. `cvc share`
/// ignores it.
pub(crate) fn share_conversation(
    repo: &Repository,
    store: &CvcStore,
    destination: &privacy::RemoteDestination,
    id: &str,
    future: FutureSharePolicy,
    publish_after: bool,
) -> Result<ShareOutcome> {
    // Make the consent legible: title and activity range alongside the
    // snapshot count already embedded in the challenge below.
    if let Some(summary) = store
        .list_conversation_summaries(Some(&destination.fingerprint), SUMMARY_SCAN_LIMIT)?
        .into_iter()
        .find(|summary| summary.id == id)
    {
        println!("{}", share_summary(&destination.name, &summary));
    }
    let ids = store.share_snapshot(id)?;
    let closure = ids.iter().map(|x| x.as_str()).collect::<String>();
    let token = hex::encode(Sha256::digest(closure.as_bytes()));
    typed_acknowledgement(&format!(
        "I SHARE {id} {} {} {}",
        destination.fingerprint,
        ids.len(),
        &token[..12]
    ))?;
    store.share_exact_snapshot(id, &destination.fingerprint, &ids, future)?;
    let count = ids.len();
    println!(
        "{} turn(s) marked shared; future turns are {}.",
        count,
        match future {
            FutureSharePolicy::Shared => "shared",
            FutureSharePolicy::Private => "private",
        }
    );
    if publish_after {
        // The share is recorded from here on. Whatever stops the publication,
        // the error must say so: an `I PUBLISH` phrase that does not match is
        // otherwise indistinguishable from an `I SHARE` that did not.
        push_destination(
            repo,
            store,
            destination,
            PublicationTrigger::BundledWithShare,
        )
        .map_err(BundledPublishFailed::wrap)?;
    }
    Ok(ShareOutcome {
        snapshot_count: count,
        future,
    })
}

/// The lines printed right before the `I SHARE` prompt: where the share goes and
/// the conversation it covers. Both are text a person is about to consent on, so
/// neither the remote's name nor the title may carry anything that rewrites the
/// lines above the prompt.
fn share_summary(remote: &str, summary: &cvc_core::db::ConversationSummary) -> String {
    format!(
        "Sharing conversation for remote '{}':\n{}",
        sanitize_for_terminal(remote),
        crate::commands::conversations::render_line(summary, true)
    )
}

/// What a completed [`share_conversation`] recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShareOutcome {
    /// How many thoughts the shared snapshot covered: the count in the typed
    /// challenge.
    pub snapshot_count: usize,
    /// Whether the conversation's future thoughts are shared as well.
    pub future: FutureSharePolicy,
}

pub async fn unshare(id: &str, requested: Option<&str>) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    unshare_conversation(&repo, &store, &destination, id).map(|_| ())
}

/// What `cvc unshare <conversation_id> --remote <destination.name>` does once
/// the destination is known: under the destination's operation lock, require
/// the typed `I UNSHARE <id> <fingerprint>` challenge, then make the
/// conversation's turns private for that destination. The store refuses, and
/// nothing changes, while any turn of the conversation is pending, published
/// or of unknown state there, because unsharing cannot recall content a
/// destination may already hold.
///
/// On success it returns how many thoughts were made private, the number it
/// prints; `cvc unshare` ignores it.
pub(crate) fn unshare_conversation(
    repo: &Repository,
    store: &CvcStore,
    destination: &privacy::RemoteDestination,
    id: &str,
) -> Result<usize> {
    let _operation_lock = privacy::destination_operation_lock(repo, &destination.fingerprint)?;
    typed_acknowledgement(&format!("I UNSHARE {id} {}", destination.fingerprint))?;
    let made_private = store.unshare_conversation_for_remote(id, &destination.fingerprint)?;
    println!("{made_private} unpublished turn(s) made private.");
    Ok(made_private)
}
pub async fn delete_local(id: &str) -> Result<()> {
    let (_, store) = open(&env::current_dir()?)?;
    let id: InteractionId = id.parse().context("invalid interaction id")?;
    store.tombstone_local(&id, TombstoneReasonCode::UserRequested, None)?;
    println!("Local CVC projection suppressed. This does not erase remote Git objects or history.");
    Ok(())
}
fn write_redaction_plan(path: &Path, plan: &cvc_core::RedactionPlan) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        bail!("plan directory does not exist");
    }
    let bytes = serde_json::to_vec_pretty(plan)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("redaction-plan"),
        uuid::Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn read_redaction_plan(path: &Path) -> Result<cvc_core::RedactionPlan> {
    let metadata = fs::symlink_metadata(path).context("unable to inspect redaction plan")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("redaction plan must be a regular, non-symlink file");
    }
    if metadata.len() > MAX_REDACTION_PLAN_BYTES {
        bail!("redaction plan exceeds size limit");
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() || opened_metadata.len() > MAX_REDACTION_PLAN_BYTES {
        bail!("redaction plan is not a regular file or exceeds size limit");
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_REDACTION_PLAN_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REDACTION_PLAN_BYTES {
        bail!("redaction plan exceeds size limit");
    }
    serde_json::from_slice(&bytes).context("invalid redaction plan")
}

pub async fn redact(
    id: &str,
    remote_name: &str,
    plan_path: &Path,
    apply_local: bool,
) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let id: InteractionId = id.parse().context("invalid interaction id")?;
    let destination = privacy::remote_destination(&repo, remote_name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    // Fresh authenticated advertisement/fetch is both the baseline proof and the
    // stale-plan lease. This command deliberately has no push path.
    sync::fetch_and_pull_destination(&repo, &store, &destination)?;
    let baseline = format!("refs/remotes/{}/cvc/main", destination.name);
    if repo.find_reference(&baseline).is_err() {
        bail!("remote has no cvc baseline to redact");
    }
    // This may only be planned after a tombstone is visible in the fetched
    // baseline. Create the pending tombstone first; the next push projects it.
    // A hard-rewrite plan remains deliberately unavailable until that proof exists.
    if !store
        .tombstones_for_projection(&destination.fingerprint)?
        .iter()
        .any(|t| t.interaction_id == id)
    {
        typed_acknowledgement(&format!("I REDACT {id} {}", destination.fingerprint))?;
        store.authorize_and_tombstone_remote(
            &id,
            &destination.fingerprint,
            TombstoneReasonCode::UserRequested,
        )?;
        println!(
            "Remote tombstone is pending; run `cvc push --manual --remote {}` then rerun redact to build the local-only rewrite plan.",
            destination.name
        );
        return Ok(());
    }
    let candidate =
        sync::build_hard_redaction_plan(&repo, Some(&baseline), &destination.fingerprint, &id)?;
    let plan = candidate.plan.clone();
    typed_acknowledgement(&format!(
        "I PLAN HARD REDACTION {} {} {} {}",
        id,
        destination.fingerprint,
        plan.expected_remote_tip.as_deref().unwrap_or("ABSENT"),
        &plan.replacement_commit[..12]
    ))?;
    write_redaction_plan(plan_path, &plan)?;
    if apply_local {
        sync::apply_hard_redaction_locally(&repo, &plan)?;
        println!("Applied replacement to local refs/cvc/main only.");
    }
    // CandidateRef drops here and removes its temporary ref on success, errors,
    // cancellation, and unwinding. The plan records it only for auditability.
    drop(candidate);
    println!("Hard-redaction plan written to {}.", plan_path.display());
    eprintln!("WARNING: Rotate credentials first if they may be exposed.");
    eprintln!("WARNING: Local current-ref unlinking is not physical deletion; clones, forks, reflogs, caches, backups, and host objects may retain data.");
    eprintln!("WARNING: Remote rewrite is unsupported pending atomic force-with-lease transport.");
    Ok(())
}
pub async fn verify_redaction_plan(path: &Path, remote_name: &str) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let destination = privacy::remote_destination(&repo, remote_name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    let plan = read_redaction_plan(path)?;
    if plan.destination_fingerprint != destination.fingerprint {
        bail!("plan destination does not match remote");
    }
    sync::fetch_and_pull_destination(&repo, &store, &destination)?;
    let tracking = format!("refs/remotes/{}/cvc/main", destination.name);
    if sync::verify_redaction_plan(&repo, &plan, &tracking)? {
        println!("Plan is current; no refs were changed.");
        Ok(())
    } else {
        bail!("plan is stale: remote cvc tip changed")
    }
}
pub async fn push(requested: Option<&str>, manual: bool) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    // Bare push is intentionally safe for legacy hook lines.  Humans must opt in
    // to manual publication; hooks use `auto_push` below.
    let destination = privacy::remote_destination(&repo, &name)?;
    if manual {
        publish_manually(&repo, &store, &destination).map(|_| ())
    } else {
        push_destination(&repo, &store, &destination, PublicationTrigger::Hook).map(|_| ())
    }
}

/// What `cvc push --manual --remote <destination.name>` does once the
/// destination is known: it requires the destination's sharing consent,
/// fetches the destination's baseline, states what it is about to publish and
/// requires the typed `I PUBLISH <fingerprint> <candidate> <count>` challenge
/// whatever the auto-push setting, and only then pushes. When nothing differs
/// from the destination it says so and asks for no challenge.
///
/// On success it reports which of those happened, so a caller that is not a
/// command line can word the result without parsing the output; `cvc push`
/// ignores it.
pub(crate) fn publish_manually(
    repo: &Repository,
    store: &CvcStore,
    destination: &privacy::RemoteDestination,
) -> Result<PublishOutcome> {
    push_destination(repo, store, destination, PublicationTrigger::Manual)
}
pub async fn auto_push_remote(cwd: &Path, requested: Option<&str>) -> Result<()> {
    let (repo, store) = open(cwd)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let status = privacy::privacy_status_for_fingerprint(&repo, &destination.fingerprint)?;
    if !status.sharing_consented || !status.auto_push {
        eprintln!("CVC: auto publication disabled for '{}'.", name);
        return Ok(());
    }
    push_destination(&repo, &store, &destination, PublicationTrigger::Hook).map(|_| ())
}

/// How a publication attempt that did not fail ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PublishOutcome {
    /// An automatic trigger found no auto-push grant for the destination and
    /// did nothing. A manual publication never ends this way.
    Skipped,
    /// The projection already equals the destination's fetched baseline;
    /// nothing was written, and no challenge was asked.
    NothingNew {
        /// Distinct non-tombstoned thoughts this clone shares with the
        /// destination, all of which it already holds.
        shared_total: usize,
    },
    /// The candidate was pushed, and these thoughts and records were added.
    Published(sync::ProjectionPublication),
}

/// Why a publication is being attempted. Each trigger settles two decisions
/// inside `push_destination` that used to be one `automatic` boolean at every
/// call site: whether the destination's standing auto-push grant gates the
/// push at all, and whether a human must answer the `I PUBLISH` challenge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationTrigger {
    /// `cvc hook pre-push`, and a bare `cvc push` (kept inert for legacy hook
    /// lines): publishes only where auto-push was acknowledged, never prompts.
    Hook,
    /// `cvc push --manual`: a human opts in to this publication through the
    /// challenge, whatever the auto-push setting.
    Manual,
    /// `cvc share --push`: the human has just answered `I SHARE` for this
    /// destination in the same command. A destination with auto-push
    /// acknowledged is its standing consent to publish, so no second
    /// challenge is demanded; without it, `I PUBLISH` still follows.
    BundledWithShare,
}

impl PublicationTrigger {
    /// Whether a destination without an auto-push grant is skipped silently.
    /// Only automatic paths are; a human-initiated publication proceeds to
    /// the challenge instead.
    fn skips_without_auto_push(self) -> bool {
        matches!(self, Self::Hook)
    }

    /// Whether the `I PUBLISH` challenge must be answered before pushing.
    fn requires_challenge(self, auto_push: bool) -> bool {
        match self {
            Self::Hook => false,
            Self::Manual => true,
            Self::BundledWithShare => !auto_push,
        }
    }
}

fn push_destination(
    repo: &Repository,
    store: &CvcStore,
    destination: &privacy::RemoteDestination,
    trigger: PublicationTrigger,
) -> Result<PublishOutcome> {
    let _operation_lock = privacy::destination_operation_lock(repo, &destination.fingerprint)?;
    let status = privacy::privacy_status_for_fingerprint(repo, &destination.fingerprint)?;
    if !status.sharing_consented {
        return Err(SharingConsentMissing {
            remote: destination.name.clone(),
        }
        .into());
    }
    if trigger.skips_without_auto_push() && !status.auto_push {
        return Ok(PublishOutcome::Skipped);
    }
    reconcile_destination(repo, store, destination)?;
    // Fetch first. The import returns exactly what it read from the tracking ref (or
    // that it was absent); the projection is built from that state and nothing else,
    // and fails closed, publishing nothing, if the ref has moved since.
    let (_, baseline) = sync::fetch_and_pull_destination_pinned(repo, store, destination)?;
    let projection =
        sync::push_projection_to_ref(repo, store, &baseline, &status.remote_fingerprint)?;
    let (oid, candidate, publication) = match projection {
        sync::ProjectionResult::NoChanges { shared_total } => {
            println!(
                "{}",
                nothing_to_publish_line(&destination.name, shared_total)
            );
            return Ok(PublishOutcome::NothingNew { shared_total });
        }
        sync::ProjectionResult::Candidate {
            oid,
            candidate,
            publication,
        } => (oid, candidate, publication),
    };
    // The one count of interactions this push adds. The pre-challenge line, the
    // typed phrase, and the result line all use it.
    let new_interactions = publication.new_ids.len();
    if trigger.requires_challenge(status.auto_push) {
        // The summary is printed immediately before the prompt, nothing in between,
        // so the count inside the challenge is one the user has just read.
        let prompt = publish_prompt(
            &destination.name,
            &destination.fingerprint,
            oid,
            new_interactions,
            &publication,
        );
        println!("{}", prompt.summary);
        typed_acknowledgement(&prompt.challenge)?;
    }
    // Publication state is recorded only for interactions this candidate newly
    // writes. Rows the baseline already holds are neither re-marked nor demoted
    // if the transport fails.
    let new_ids = publication.new_ids.as_slice();
    store.mark_publication(
        new_ids,
        &status.remote_fingerprint,
        PublicationState::Pending,
    )?;
    let pushed = sync::push_temp_ref(repo, destination, candidate.ref_name());
    // `candidate` cleans up on success, errors, TTY cancellation, and unwinding.
    let result = settle_push(store, new_ids, &status.remote_fingerprint, pushed).map(|()| {
        println!(
            "{}",
            describe_publication(
                &destination.name,
                new_interactions,
                &publication,
                PublicationPhase::Done
            )
        );
        PublishOutcome::Published(publication)
    });
    drop(candidate);
    result
}

/// Records what a finished push means for the thoughts it carried, which were
/// marked `pending` before it began. From here on every error is a
/// [`PublicationOutcomeUnknown`]: the push has been attempted, so nothing the
/// local records can still say settles what the destination holds.
///
/// - The push went through: they become `published`, and so do the tombstones it
///   carried. If recording that fails the destination holds them, but the rows
///   stay `pending` (or the tombstones unrecorded) until a reconciliation or the
///   next push settles it.
/// - The push failed: that is ambiguous, so they become `unknown` and the error
///   is the transport's. Do not make content mutable until a later reconciliation
///   proves the destination did not receive it. If marking them fails too, that
///   error is returned instead and they stay `pending`.
fn settle_push(
    store: &CvcStore,
    new_ids: &[InteractionId],
    fingerprint: &str,
    pushed: sync::Result<()>,
) -> Result<()> {
    match pushed {
        Ok(()) => {
            store
                .mark_publication(new_ids, fingerprint, PublicationState::Published)
                .map_err(PublicationOutcomeUnknown::new)?;
            store
                .mark_remote_tombstones_published(fingerprint)
                .map_err(PublicationOutcomeUnknown::new)?;
            Ok(())
        }
        Err(error) => {
            store
                .mark_publication(new_ids, fingerprint, PublicationState::Unknown)
                .map_err(PublicationOutcomeUnknown::new)?;
            Err(PublicationOutcomeUnknown::new(error).into())
        }
    }
}
/// Shown when the projection already equals the fetched remote baseline.
fn nothing_to_publish_line(remote: &str, shared_total: usize) -> String {
    let remote = sanitize_for_terminal(remote);
    if shared_total > 0 {
        format!(
            "No new shared interactions to publish to '{remote}' ({shared_total} shared in total)."
        )
    } else {
        format!("No new shared interactions to publish to '{remote}'.")
    }
}

/// The typed phrase that authorizes one publication. The trailing number is the
/// count of interactions this push adds to the destination (not the size of the
/// whole shared projection); the candidate commit prefix pins the exact tree.
fn publish_challenge(fingerprint: &str, candidate: git2::Oid, new_interactions: usize) -> String {
    format!(
        "I PUBLISH {fingerprint} {} {new_interactions}",
        &candidate.to_string()[..12]
    )
}

/// The summary line shown right before the challenge and the challenge itself.
/// Both are built from the same `new_interactions` value.
struct PublishPrompt {
    summary: String,
    challenge: String,
}

fn publish_prompt(
    remote: &str,
    fingerprint: &str,
    candidate: git2::Oid,
    new_interactions: usize,
    publication: &sync::ProjectionPublication,
) -> PublishPrompt {
    PublishPrompt {
        summary: describe_publication(
            remote,
            new_interactions,
            publication,
            PublicationPhase::Planned,
        ),
        challenge: publish_challenge(fingerprint, candidate, new_interactions),
    }
}

/// Whether a publication summary is shown before the user confirms it
/// (`Planned`) or reports a push that has completed (`Done`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationPhase {
    Planned,
    Done,
}

/// What tombstone pruning takes out of the ref's current tree, worded so it cannot
/// be read as erasure: the earlier commits of the ref keep those blobs, and it
/// says so (`not erased from history`).
fn removal_phrase(publication: &sync::ProjectionPublication) -> Option<String> {
    const SCOPE: &str = "the removal from the current tree of";
    let removed = match (
        publication.removed_interactions,
        publication.removed_records,
    ) {
        (0, 0) => return None,
        (interactions, 0) => format!("{SCOPE} {interactions} tombstoned interaction(s)"),
        (interactions, records) if interactions > 0 => format!(
            "{SCOPE} {interactions} tombstoned interaction(s) with {records} attached record(s)"
        ),
        (_, records) => format!("{SCOPE} {records} record(s) of tombstoned interaction(s)"),
    };
    Some(format!("{removed} (not erased from history)"))
}

/// Everything a push carries besides new interactions, as a noun phrase such as
/// `3 commit link(s) and 1 tombstone(s)`. `None` when there is nothing else.
fn extra_records_phrase(publication: &sync::ProjectionPublication) -> Option<String> {
    let counted = |count: usize, phrase: String| (count > 0).then_some(phrase);
    let parts: Vec<String> = [
        counted(
            publication.new_link_records,
            format!("{} commit link(s)", publication.new_link_records),
        ),
        counted(
            publication.new_tombstones,
            format!("{} tombstone(s)", publication.new_tombstones),
        ),
        counted(
            publication.new_derivation_records,
            format!(
                "{} derivation record(s)",
                publication.new_derivation_records
            ),
        ),
        removal_phrase(publication),
    ]
    .into_iter()
    .flatten()
    .collect();
    match parts.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        [head @ .., last] => Some(format!("{} and {last}", head.join(", "))),
    }
}

/// The single line describing what a push carries, before (`Planned`) or after
/// (`Done`) it. All wording for these lines lives here. A push that adds no
/// interactions never reads as "Published 0 ...".
fn describe_publication(
    remote: &str,
    new_interactions: usize,
    publication: &sync::ProjectionPublication,
    phase: PublicationPhase,
) -> String {
    let remote = sanitize_for_terminal(remote);
    let total = publication.shared_total;
    let lead = match phase {
        PublicationPhase::Planned => "Publishing",
        PublicationPhase::Done => "Published",
    };
    match (new_interactions, extra_records_phrase(publication)) {
        // Only the ref's format, empty namespaces, or the derived by-commit index
        // differ: no interaction, link, tombstone, or derivation record moves.
        (0, None) => format!(
            "{lead} a format/index update of refs/cvc/main to '{remote}' (no new interactions, links, or tombstones)."
        ),
        (0, Some(extras)) => format!(
            "{lead} {extras} to '{remote}' (no new shared interactions; {total} shared in total)."
        ),
        (new, extras) => {
            // On a first push the total is the new count; say it once.
            let context = if extras.is_none() && new == total {
                String::new()
            } else {
                format!(" ({total} shared in total)")
            };
            let tail = extras
                .map(|extras| format!(", plus {extras}"))
                .unwrap_or_default();
            format!("{lead} {new} new shared interaction(s) to '{remote}'{context}{tail}.")
        }
    }
}
fn reconcile_destination(
    repo: &Repository,
    store: &CvcStore,
    destination: &privacy::RemoteDestination,
) -> Result<()> {
    let uncertain = store.uncertain_publication_ids(&destination.fingerprint)?;
    sync::fetch_and_pull_destination(repo, store, destination)?;
    // fetch/pull atomically records every node observed at the exact fetched
    // baseline as Published. Anything still uncertain was proven absent.
    store.clear_uncertain_publications(&uncertain, &destination.fingerprint)?;
    Ok(())
}
pub async fn reconcile(requested: Option<&str>) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    reconcile_destination(&repo, &store, &destination)?;
    println!("Reconciled publication state for '{}'.", name);
    Ok(())
}
pub async fn pull() -> Result<()> {
    let (repo, mut store) = open(&env::current_dir()?)?;
    let name = remote(&repo, None)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    let n = sync::fetch_and_pull_destination(&repo, &store, &destination)?;
    let _ =
        cvc_core::squash::scan_for(&repo, &mut store, false, std::time::Duration::from_secs(30))?;
    println!("Pulled {n} interaction(s) from '{name}'.");
    Ok(())
}

pub async fn observe_range(base: &str, tip: &str, requested: Option<&str>) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let base = repo.revparse_single(base)?.peel_to_commit()?.id();
    let tip = repo.revparse_single(tip)?.peel_to_commit()?.id();
    let authorization = if let Some(name) = requested {
        let destination = privacy::remote_destination(&repo, name)?;
        typed_acknowledgement(&format!(
            "I AUTHORIZE RANGE {} {} {}",
            base, tip, destination.fingerprint
        ))?;
        Some(destination.fingerprint)
    } else {
        None
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let range = cvc_core::squash::observe_explicit_range_with_abort(
        &repo,
        &store,
        base,
        tip,
        None,
        requested,
        authorization.as_deref(),
        || std::time::Instant::now() >= deadline,
    )?;
    println!("Observed exact range {}.", range.range_id);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{read_redaction_plan, write_redaction_plan, MAX_REDACTION_PLAN_BYTES};
    use cvc_core::{InteractionId, RedactionPlan, RedactionPlanFields};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn redaction_plan_is_written_0600() {
        let directory =
            std::env::temp_dir().join(format!("cvc-plan-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("plan.json");
        let plan = RedactionPlan::v2(
            "repo".into(),
            RedactionPlanFields {
                destination_fingerprint: "destination".into(),
                target_id: InteractionId::new(),
                expected_remote_tip: None,
                replacement_commit: "a".repeat(40),
                temporary_ref: "refs/cvc/candidate-test".into(),
                removed_nodes: 0,
                removed_by_commit_entries: 0,
                removed_link_entries: 0,
                unrelated_entries_retained: 0,
                tombstone_oid: "b".repeat(40),
                created_at: chrono::Utc::now(),
                warning: "test".into(),
            },
        );
        write_redaction_plan(&path, &plan).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn redaction_plan_reader_rejects_oversized_and_symlink_inputs() {
        use std::os::unix::fs::symlink;

        let directory =
            std::env::temp_dir().join(format!("cvc-plan-read-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let oversized = directory.join("oversized.json");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MAX_REDACTION_PLAN_BYTES + 1).unwrap();
        assert!(read_redaction_plan(&oversized).is_err());

        let target = directory.join("target.json");
        std::fs::write(&target, b"{}").unwrap();
        let link = directory.join("plan.json");
        symlink(&target, &link).unwrap();
        assert!(read_redaction_plan(&link).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod publication_trigger_tests {
    use super::PublicationTrigger::{self, *};

    /// (trigger, auto_push) -> (skipped without a grant, challenge required).
    const TABLE: &[(PublicationTrigger, bool, bool, bool)] = &[
        (Hook, true, false, false),
        (Hook, false, true, false),
        (Manual, true, false, true),
        (Manual, false, false, true),
        (BundledWithShare, true, false, false),
        (BundledWithShare, false, false, true),
    ];

    #[test]
    fn triggers_decide_gating_and_challenge_independently() {
        for &(trigger, auto_push, skipped, challenged) in TABLE {
            assert_eq!(
                trigger.skips_without_auto_push() && !auto_push,
                skipped,
                "{trigger:?} auto_push={auto_push}"
            );
            assert_eq!(
                trigger.requires_challenge(auto_push),
                challenged,
                "{trigger:?} auto_push={auto_push}"
            );
        }
    }

    #[test]
    fn share_push_without_auto_push_prompts_rather_than_publishing_nothing() {
        // The trap from the issue: threading `!auto_push` through the old
        // `automatic` flag would have hit the silent early return here.
        assert!(!BundledWithShare.skips_without_auto_push());
        assert!(BundledWithShare.requires_challenge(false));
    }
}

#[cfg(test)]
mod publication_summary_tests {
    use super::PublicationPhase::{Done, Planned};
    use super::{describe_publication, nothing_to_publish_line, publish_challenge, publish_prompt};
    use cvc_core::{sync::ProjectionPublication, InteractionId};

    /// `[links, tombstones, derivations, removed interactions, removed records]`.
    fn publication(new: usize, total: usize, others: [usize; 5]) -> ProjectionPublication {
        ProjectionPublication {
            new_ids: (0..new).map(|_| InteractionId::new()).collect(),
            shared_total: total,
            new_link_records: others[0],
            new_tombstones: others[1],
            new_derivation_records: others[2],
            removed_interactions: others[3],
            removed_records: others[4],
        }
    }

    fn line(p: &ProjectionPublication, phase: super::PublicationPhase) -> String {
        describe_publication("origin", p.new_ids.len(), p, phase)
    }

    #[test]
    fn new_interactions_are_counted_apart_from_the_shared_total() {
        let p = publication(21, 42, [0; 5]);
        assert_eq!(
            line(&p, Planned),
            "Publishing 21 new shared interaction(s) to 'origin' (42 shared in total)."
        );
        assert_eq!(
            line(&p, Done),
            "Published 21 new shared interaction(s) to 'origin' (42 shared in total)."
        );
    }

    #[test]
    fn a_first_push_does_not_repeat_the_count_as_a_total() {
        let p = publication(21, 21, [0; 5]);
        assert_eq!(
            line(&p, Planned),
            "Publishing 21 new shared interaction(s) to 'origin'."
        );
        assert_eq!(
            line(&p, Done),
            "Published 21 new shared interaction(s) to 'origin'."
        );
        // With something else riding along the context stays, so the line keeps
        // one shape.
        let with_links = publication(21, 21, [3, 0, 0, 0, 0]);
        assert_eq!(
            line(&with_links, Done),
            "Published 21 new shared interaction(s) to 'origin' (21 shared in total), plus 3 commit link(s)."
        );
    }

    #[test]
    fn other_records_ride_along_and_zero_categories_are_omitted() {
        assert_eq!(
            line(&publication(21, 42, [3, 0, 0, 0, 0]), Planned),
            "Publishing 21 new shared interaction(s) to 'origin' (42 shared in total), plus 3 commit link(s)."
        );
        assert_eq!(
            line(&publication(21, 42, [3, 1, 0, 0, 0]), Planned),
            "Publishing 21 new shared interaction(s) to 'origin' (42 shared in total), plus 3 commit link(s) and 1 tombstone(s)."
        );
        assert_eq!(
            line(&publication(5, 9, [3, 1, 2, 0, 0]), Done),
            "Published 5 new shared interaction(s) to 'origin' (9 shared in total), plus 3 commit link(s), 1 tombstone(s) and 2 derivation record(s)."
        );
        assert_eq!(
            line(&publication(1, 4, [0, 2, 0, 0, 0]), Done),
            "Published 1 new shared interaction(s) to 'origin' (4 shared in total), plus 2 tombstone(s)."
        );
    }

    #[test]
    fn a_push_without_new_interactions_leads_with_what_it_does_carry() {
        let p = publication(0, 42, [3, 1, 0, 0, 0]);
        assert_eq!(
            line(&p, Planned),
            "Publishing 3 commit link(s) and 1 tombstone(s) to 'origin' (no new shared interactions; 42 shared in total)."
        );
        assert_eq!(
            line(&p, Done),
            "Published 3 commit link(s) and 1 tombstone(s) to 'origin' (no new shared interactions; 42 shared in total)."
        );
        assert_eq!(
            line(&publication(0, 2, [0, 0, 1, 0, 0]), Done),
            "Published 1 derivation record(s) to 'origin' (no new shared interactions; 2 shared in total)."
        );
    }

    #[test]
    fn removals_by_tombstone_pruning_are_named_never_called_a_format_update() {
        let removal = publication(0, 40, [0, 0, 0, 2, 0]);
        assert!(!removal.is_structural_only());
        assert_eq!(
            line(&removal, Planned),
            "Publishing the removal from the current tree of 2 tombstoned interaction(s) (not erased from history) to 'origin' (no new shared interactions; 40 shared in total)."
        );
        let redaction = publication(0, 41, [0, 1, 0, 1, 2]);
        assert_eq!(
            line(&redaction, Done),
            "Published 1 tombstone(s) and the removal from the current tree of 1 tombstoned interaction(s) with 2 attached record(s) (not erased from history) to 'origin' (no new shared interactions; 41 shared in total)."
        );
        let stale = publication(0, 41, [0, 0, 0, 0, 3]);
        assert_eq!(
            line(&stale, Done),
            "Published the removal from the current tree of 3 record(s) of tombstoned interaction(s) (not erased from history) to 'origin' (no new shared interactions; 41 shared in total)."
        );
        // One trailing element, after links, tombstones, and derivations.
        let everything = publication(2, 9, [3, 1, 2, 1, 4]);
        assert_eq!(
            line(&everything, Planned),
            "Publishing 2 new shared interaction(s) to 'origin' (9 shared in total), plus 3 commit link(s), 1 tombstone(s), 2 derivation record(s) and the removal from the current tree of 1 tombstoned interaction(s) with 4 attached record(s) (not erased from history)."
        );
        for p in [&removal, &redaction, &stale] {
            assert!(!line(p, Planned).contains("format/index"));
        }
    }

    #[test]
    fn a_residual_candidate_is_a_format_or_index_update_and_never_zero_published() {
        let p = publication(0, 42, [0; 5]);
        assert!(p.is_structural_only());
        assert_eq!(
            line(&p, Planned),
            "Publishing a format/index update of refs/cvc/main to 'origin' (no new interactions, links, or tombstones)."
        );
        assert_eq!(
            line(&p, Done),
            "Published a format/index update of refs/cvc/main to 'origin' (no new interactions, links, or tombstones)."
        );
        for text in [line(&p, Planned), line(&p, Done)] {
            assert!(!text.contains("Published 0"), "{text}");
            assert!(!text.contains("Publishing 0"), "{text}");
        }
    }

    #[test]
    fn nothing_to_publish_mentions_the_total_only_when_there_is_one() {
        assert_eq!(
            nothing_to_publish_line("origin", 42),
            "No new shared interactions to publish to 'origin' (42 shared in total)."
        );
        assert_eq!(
            nothing_to_publish_line("origin", 0),
            "No new shared interactions to publish to 'origin'."
        );
    }

    #[test]
    fn the_challenge_carries_the_new_count_and_the_candidate_prefix() {
        let oid = git2::Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
        let fingerprint = "f".repeat(64);
        assert_eq!(
            publish_challenge(&fingerprint, oid, 21),
            format!("I PUBLISH {fingerprint} 0123456789ab 21")
        );
        assert!(publish_challenge(&fingerprint, oid, 0).ends_with(" 0123456789ab 0"));
    }

    /// The count the user reads in the summary line.
    fn count_in_summary(summary: &str) -> usize {
        if summary.contains("no new shared interactions") || summary.contains("no new interactions")
        {
            return 0;
        }
        summary
            .strip_prefix("Publishing ")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|count| count.parse().ok())
            .unwrap_or_else(|| panic!("no interaction count in {summary:?}"))
    }

    #[test]
    fn the_challenge_count_is_the_count_in_the_line_printed_before_it() {
        let oid = git2::Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
        let fingerprint = "f".repeat(64);
        let shapes = [
            publication(21, 42, [0; 5]),
            publication(21, 21, [0; 5]),
            publication(1, 1, [0, 1, 0, 1, 0]),
            publication(7, 9, [3, 1, 2, 0, 0]),
            publication(0, 42, [3, 1, 0, 0, 0]),
            publication(0, 40, [0, 0, 0, 2, 0]),
            publication(0, 42, [0; 5]),
            publication(0, 0, [0, 1, 0, 0, 0]),
        ];
        for p in &shapes {
            let prompt = publish_prompt("origin", &fingerprint, oid, p.new_ids.len(), p);
            let token = prompt.challenge.rsplit(' ').next().unwrap();
            assert_eq!(
                token.parse::<usize>().unwrap(),
                count_in_summary(&prompt.summary),
                "{} / {}",
                prompt.summary,
                prompt.challenge
            );
            assert!(prompt.challenge.starts_with("I PUBLISH "));
        }
    }
}

/// What a finished push does to the records of the thoughts it carried, with
/// the transport's result handed in so each outcome can be had without a
/// network.
#[cfg(test)]
mod settle_tests {
    use super::settle_push;
    use crate::commands::failure::{classify_failure, FlowFailure};
    use chrono::Utc;
    use cvc_core::db::CvcStore;
    use cvc_core::models::{Author, Conversation, Interaction, InteractionId, PublicationState};
    use cvc_core::privacy::{McpCapture, PreparedPolicy};
    use cvc_core::sync::SyncError;
    use rusqlite::{params, Connection};
    use std::path::PathBuf;
    use tempfile::TempDir;

    const FINGERPRINT: &str = "7777777777777777777777777777777777777777777777777777777777777777";

    /// A store with two thoughts whose push has begun: both are `pending`.
    struct Begun {
        _directory: TempDir,
        path: PathBuf,
        store: CvcStore,
        ids: Vec<InteractionId>,
    }

    impl Begun {
        fn new() -> Self {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("index.db");
            let store = CvcStore::open(&path).unwrap();
            let ids: Vec<_> = (0..2)
                .map(|_| {
                    let id = InteractionId::new();
                    store
                        .capture_mcp(McpCapture::new(
                            Conversation {
                                id: "settling".into(),
                                title: "settle fixture".into(),
                                created_at: Utc::now(),
                            },
                            Interaction {
                                id: id.clone(),
                                conversation_id: "settling".into(),
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
                })
                .collect();
            store
                .mark_publication(&ids, FINGERPRINT, PublicationState::Pending)
                .unwrap();
            Self {
                _directory: directory,
                path,
                store,
                ids,
            }
        }

        /// The publication state of each thought for the destination.
        fn states(&self) -> Vec<String> {
            let connection = Connection::open(&self.path).unwrap();
            self.ids
                .iter()
                .map(|id| {
                    connection
                        .query_row(
                            "SELECT state FROM publications WHERE interaction_id = ?1 AND remote_fingerprint = ?2",
                            params![id.as_str(), FINGERPRINT],
                            |row| row.get(0),
                        )
                        .unwrap()
                })
                .collect()
        }

        /// Makes recording a publication state fail, as a database that went
        /// read-only or full would.
        fn refuse_publication_updates(&self) {
            Connection::open(&self.path)
                .unwrap()
                .execute_batch(
                    "CREATE TRIGGER refuse_publication_updates BEFORE UPDATE ON publications
                     BEGIN SELECT RAISE(ABORT, 'publication state is read-only'); END;",
                )
                .unwrap();
        }

        fn settle(&self, pushed: cvc_core::sync::Result<()>) -> anyhow::Result<()> {
            settle_push(&self.store, &self.ids, FINGERPRINT, pushed)
        }
    }

    #[test]
    fn a_push_that_went_through_is_recorded_as_published() {
        let begun = Begun::new();
        assert_eq!(begun.states(), ["pending", "pending"]);
        begun.settle(Ok(())).unwrap();
        assert_eq!(begun.states(), ["published", "published"]);
    }

    #[test]
    fn a_failed_transport_marks_the_thoughts_unknown_and_is_an_unknown_outcome() {
        let begun = Begun::new();
        let error = begun
            .settle(Err(SyncError::Ref("transport boom".into())))
            .unwrap_err();
        assert_eq!(begun.states(), ["unknown", "unknown"]);
        assert_eq!(
            classify_failure(&error),
            FlowFailure::OutcomeUnknown("Reference error: transport boom".into())
        );
        // Typing it changed nothing a command prints.
        assert_eq!(format!("{error:#}"), "Reference error: transport boom");
    }

    #[test]
    fn a_push_that_went_through_but_could_not_be_recorded_leaves_the_thoughts_pending() {
        let begun = Begun::new();
        begun.refuse_publication_updates();
        let direct = begun
            .store
            .mark_publication(&begun.ids, FINGERPRINT, PublicationState::Published)
            .unwrap_err();
        let direct = format!("{:#}", anyhow::Error::from(direct));
        assert!(
            direct.contains("publication state is read-only"),
            "{direct}"
        );

        let error = begun.settle(Ok(())).unwrap_err();
        assert_eq!(begun.states(), ["pending", "pending"]);
        assert_eq!(
            classify_failure(&error),
            FlowFailure::OutcomeUnknown(direct.clone())
        );
        assert_eq!(format!("{error:#}"), direct);
    }

    #[test]
    fn tombstones_that_could_not_be_recorded_after_a_push_are_an_unknown_outcome_too() {
        let begun = Begun::new();
        Connection::open(&begun.path)
            .unwrap()
            .execute_batch("DROP TABLE tombstones")
            .unwrap();
        let error = begun.settle(Ok(())).unwrap_err();
        // The thoughts themselves were recorded before the tombstones were.
        assert_eq!(begun.states(), ["published", "published"]);
        assert!(matches!(
            classify_failure(&error),
            FlowFailure::OutcomeUnknown(_)
        ));
    }

    #[test]
    fn a_failed_transport_that_cannot_be_recorded_is_still_an_unknown_outcome() {
        let begun = Begun::new();
        begun.refuse_publication_updates();
        let error = begun
            .settle(Err(SyncError::Ref("transport boom".into())))
            .unwrap_err();
        // They could not be marked `unknown`, so they stay `pending`; the error
        // is the store's, as it was before it was typed.
        assert_eq!(begun.states(), ["pending", "pending"]);
        match classify_failure(&error) {
            FlowFailure::OutcomeUnknown(message) => {
                assert!(
                    message.contains("publication state is read-only"),
                    "{message}"
                );
            }
            other => panic!("{other:?}"),
        }
    }
}

/// What is printed around a consent challenge cannot be rewritten by text that
/// came from the store or the repository's configuration.
#[cfg(test)]
mod output_tests {
    use super::{
        describe_publication, ensure_phrase_can_be_shown, nothing_to_publish_line, share_summary,
        PublicationPhase,
    };
    use crate::terminal_text::is_unsafe_for_terminal;
    use cvc_core::db::ConversationSummary;
    use cvc_core::sync::ProjectionPublication;

    fn acts_on_a_terminal(text: &str) -> bool {
        text.chars().any(|c| c != '\n' && is_unsafe_for_terminal(c))
    }

    fn summary() -> ConversationSummary {
        ConversationSummary {
            id: "c-1".into(),
            title: "Refactor the sync module".into(),
            thoughts: 2,
            linked: 0,
            first_activity: 1_700_000_000,
            last_activity: 1_700_000_000,
            shared: false,
            shared_count: 0,
            published: 0,
            unconfirmed: 0,
            share_future: None,
            imported: false,
            held: 0,
            held_published: 0,
        }
    }

    #[test]
    fn the_share_summary_names_the_remote_and_the_conversation_as_plain_text() {
        let text = share_summary("origin", &summary());
        assert_eq!(
            text,
            "Sharing conversation for remote 'origin':\n2023-11-14 22:13    2 thought(s),   0 linked  c-1  [private]\n      Refactor the sync module"
        );

        // The title is what a prompt-injected agent writes; the remote name is
        // whatever the configuration says.
        let mut hostile = summary();
        hostile.title = "Fix\u{1b}[1A\u{1b}[2KSharing conversation for remote 'origin':".into();
        hostile.id = "c\u{202e}-1".into();
        let text = share_summary("or\u{1b}[2Jigin\n", &hostile);
        assert!(!acts_on_a_terminal(&text), "{text:?}");
        assert!(
            text.starts_with("Sharing conversation for remote 'or?[2Jigin?':\n"),
            "{text:?}"
        );
        assert_eq!(text.matches('\n').count(), 2, "{text:?}");
    }

    #[test]
    fn the_publication_lines_name_the_remote_as_plain_text() {
        let name = "or\u{1b}[2Jigin";
        let nothing = nothing_to_publish_line(name, 3);
        assert_eq!(
            nothing,
            "No new shared interactions to publish to 'or?[2Jigin' (3 shared in total)."
        );
        let publication = ProjectionPublication {
            shared_total: 3,
            ..ProjectionPublication::default()
        };
        for phase in [PublicationPhase::Planned, PublicationPhase::Done] {
            let line = describe_publication(name, 0, &publication, phase);
            assert!(!acts_on_a_terminal(&line), "{line:?}");
            assert!(line.contains("'or?[2Jigin'"), "{line:?}");
        }
        // And an ordinary name is printed as it is.
        assert_eq!(
            nothing_to_publish_line("origin", 0),
            "No new shared interactions to publish to 'origin'."
        );
    }

    #[test]
    fn a_phrase_with_a_control_or_format_character_is_not_asked() {
        for phrase in [
            "I SHARE c-1 fingerprint 3 token",
            "I UNSHARE run-0d2a:agent_1.b fingerprint",
        ] {
            assert!(ensure_phrase_can_be_shown(phrase).is_ok(), "{phrase}");
        }
        for phrase in [
            "I SHARE c\u{1b}[2J-1 fingerprint 3 token",
            "I SHARE c-1\u{202e} fingerprint 3 token",
            "I SHARE c-\u{200b}1 fingerprint 3 token",
            "I SHARE c-1\nfingerprint 3 token",
            "I SHARE c-1\u{2028}x",
        ] {
            let error = ensure_phrase_can_be_shown(phrase).unwrap_err().to_string();
            assert!(error.contains("cannot be shown"), "{error}");
            assert!(error.contains("nothing was recorded"), "{error}");
            // And the refusal does not repeat the phrase it refused to show.
            assert!(!acts_on_a_terminal(&error), "{error:?}");
            assert!(!error.contains("fingerprint"), "{error}");
        }
    }
}

/// A challenge answer that cannot be read is an answer that does not match.
#[cfg(test)]
mod answer_tests {
    use super::answer_error;
    use crate::commands::failure::{classify_failure, ChallengeNotConfirmed, FlowFailure};
    use crate::tty::UnreadableAnswer;
    use std::io;

    #[test]
    fn an_unreadable_answer_is_not_a_confirmation() {
        let not_utf8 = io::Error::new(io::ErrorKind::InvalidData, UnreadableAnswer::NotUtf8);
        let error = answer_error(not_utf8);
        assert_eq!(classify_failure(&error), FlowFailure::NotConfirmed);
        assert_eq!(error.to_string(), "acknowledgement challenge did not match");
    }

    #[cfg(unix)]
    #[test]
    fn a_line_that_is_too_long_is_not_a_confirmation() {
        let too_long = io::Error::new(io::ErrorKind::InvalidData, UnreadableAnswer::TooLong);
        assert_eq!(
            classify_failure(&answer_error(too_long)),
            FlowFailure::NotConfirmed
        );
    }

    #[cfg(unix)]
    #[test]
    fn input_that_kept_arriving_is_a_challenge_that_was_not_asked() {
        let kept_arriving = io::Error::new(
            io::ErrorKind::InvalidData,
            UnreadableAnswer::InputKeptArriving,
        );
        let error = answer_error(kept_arriving);
        // The command says why in front of the usual text, as it always has...
        assert_eq!(
            format!("{error:#}"),
            "input kept arriving at the terminal, so no phrase was asked: acknowledgement challenge did not match"
        );
        // ...and a caller that words the refusal itself is told that it was not
        // a phrase that did not match.
        assert_eq!(classify_failure(&error), FlowFailure::InputKeptArriving);
        assert!(error
            .downcast_ref::<ChallengeNotConfirmed>()
            .is_some_and(|reason| *reason == ChallengeNotConfirmed::InputKeptArriving));
    }

    #[cfg(unix)]
    #[test]
    fn a_terminal_that_could_not_be_put_back_is_a_challenge_that_was_not_asked() {
        let error = answer_error(
            crate::tty::TerminalNotRestored::new("Input/output error (os error 5)").into_io_error(),
        );
        assert_eq!(
            format!("{error:#}"),
            "the terminal could not be put back to normal input (Input/output error (os error 5)), so no phrase was asked: acknowledgement challenge did not match"
        );
        assert_eq!(classify_failure(&error), FlowFailure::TerminalNotRestored);
    }

    #[test]
    fn a_phrase_that_was_asked_and_not_matched_is_not_one_that_was_not_asked() {
        assert_eq!(
            classify_failure(&ChallengeNotConfirmed::Mismatch.into()),
            FlowFailure::NotConfirmed
        );
        assert_eq!(
            classify_failure(&ChallengeNotConfirmed::Unreadable.into()),
            FlowFailure::NotConfirmed
        );
    }

    #[test]
    fn a_failing_terminal_is_a_failure_with_its_own_text() {
        for error in [
            io::Error::new(io::ErrorKind::BrokenPipe, "terminal went away"),
            io::Error::new(io::ErrorKind::InvalidData, "some other invalid data"),
            io::Error::from(io::ErrorKind::UnexpectedEof),
            io::Error::other("some other failure"),
        ] {
            let text = error.to_string();
            let error = answer_error(error);
            assert_eq!(classify_failure(&error), FlowFailure::Other(text));
        }
    }
}
