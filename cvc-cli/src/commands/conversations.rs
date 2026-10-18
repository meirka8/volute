use anyhow::{anyhow, bail, Context, Result};
use chrono::{TimeZone, Utc};
use cvc_core::db::{ConversationSummary, CvcStore};
use cvc_core::privacy::{self, PrivacyError};
use cvc_core::repository::RepositoryLayout;
use git2::Repository;
use std::env;

/// How many conversations the plain listing shows when `--limit` is absent.
const PLAIN_DEFAULT_LIMIT: usize = 20;

/// Renders one summary line. Kept width-stable so the picker and the listing
/// read identically.
pub(crate) fn render_line(summary: &ConversationSummary, with_destination: bool) -> String {
    let when = |seconds: i64| {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".into())
    };
    let state = if !with_destination {
        String::new()
    } else if summary.published > 0 && summary.published == summary.thoughts {
        format!("  [shared, {} published]", summary.published)
    } else if summary.shared {
        format!(
            "  [shared, {}/{} published]",
            summary.published, summary.thoughts
        )
    } else {
        "  [private]".into()
    };
    format!(
        "{}  {:>3} thought(s), {:>3} linked  {}{}\n      {}",
        when(summary.last_activity),
        summary.thoughts,
        summary.linked,
        summary.id,
        state,
        summary.title,
    )
}

/// Resolve the destination fingerprint for display, tolerating repositories
/// with no usable remote: the listing is read-only and must still work there.
pub(crate) fn display_destination(
    repo: &Repository,
    requested: Option<&str>,
) -> Option<(String, String)> {
    let name = requested.unwrap_or("origin").to_owned();
    privacy::remote_destination(repo, &name)
        .ok()
        .map(|destination| (name, destination.fingerprint))
}

/// Whether `cvc conversations` may start its interactive view instead of
/// printing the plain listing: not when `--plain` was given, and only when
/// stdin and stdout are both terminals. That is the very check
/// (`tty::stdio_is_terminal`) that lets a consent challenge be asked at all,
/// so a script, a pipe or an IDE tool never gets the interactive view and
/// the view can never be a way around a challenge.
pub(crate) fn interactive_ui_allowed(plain: bool) -> bool {
    !plain && crate::tty::stdio_is_terminal()
}

/// `cvc conversations`. `limit` is `--limit` as given: the plain listing falls
/// back to [`PLAIN_DEFAULT_LIMIT`], the interactive view to
/// `sync::SUMMARY_SCAN_LIMIT`. `plain` is `--plain`.
pub async fn run(requested: Option<&str>, limit: Option<usize>, plain: bool) -> Result<()> {
    let current_dir = env::current_dir()?;
    let layout =
        RepositoryLayout::discover(&current_dir).context("Failed to discover Git repository")?;
    layout.worktree_root()?;
    if !layout.cvc_dir().exists() {
        println!("CVC is not initialized in this repository. Run 'cvc init' to setup.");
        return Ok(());
    }
    let store = CvcStore::open_initialized(layout.db_path())?;
    let repo = layout.into_repository();

    // DISPATCH POINT for the interactive view (#112, Phase B).
    //
    // When this holds, Phase B starts the view here and returns its result
    // instead of falling through, passing `&repo`, `&store`, `requested` and
    // `limit.unwrap_or(sync::SUMMARY_SCAN_LIMIT)`. Until the view exists the
    // plain listing below is printed in every case, and `--plain` changes
    // nothing.
    if interactive_ui_allowed(plain) {
        // Intentionally empty: see above.
    }

    let destination = display_destination(&repo, requested);
    let summaries = store.list_conversation_summaries(
        destination
            .as_ref()
            .map(|(_, fingerprint)| fingerprint.as_str()),
        limit.unwrap_or(PLAIN_DEFAULT_LIMIT),
    )?;
    if summaries.is_empty() {
        println!("No conversations captured yet.");
        return Ok(());
    }
    match &destination {
        Some((name, fingerprint)) => println!(
            "Conversations (most recent first); share state for remote '{}' ({}…):",
            name,
            &fingerprint[..12]
        ),
        None => println!("Conversations (most recent first); no remote resolved for share state:"),
    }
    for summary in &summaries {
        println!("{}", render_line(summary, destination.is_some()));
    }
    println!(
        "\nShare one with: cvc share <conversation-id> --remote <name>  (or run `cvc share` interactively)"
    );
    Ok(())
}

/// One remote of the repository as the interactive view shows it, in the
/// header and the remote picker. It is a read-only description and grants
/// nothing: consent is whatever `privacy.json` already records for the
/// destination fingerprint, and only the typed challenges change that.
///
/// It holds the fingerprint, not the destination itself, so no raw URL (which
/// may embed credentials) is kept around or printed by a stray `{:?}`; the only
/// trace of the URL is the sanitized `location`. To act on a remote, call
/// [`RemoteOverview::resolve`], which re-resolves it the way
/// `cvc share --remote <name>` would.
#[allow(dead_code)] // Consumed by the interactive view (#112, Phase B).
#[derive(Clone, Debug)]
pub(crate) struct RemoteOverview {
    /// The remote's name as configured.
    pub name: String,
    /// The destination fingerprint (the SHA-256 of the effective push URL and
    /// ref that consent is bound to), or the reason the remote cannot be a
    /// destination, such as `remote has no push URL`. The reason is plain text
    /// meant for display.
    pub fingerprint: std::result::Result<String, String>,
    /// Where the remote points, for display only: `host[:port]/path`,
    /// `host:path` or a local path, with user information (the user and any
    /// password or token), query and fragment removed. `None` when the remote
    /// is unusable or no safe form exists (see `privacy::display_location`).
    /// The raw URL is never a fallback. Do not resolve, compare or fetch
    /// through it; the fingerprint is the identity.
    pub location: Option<String>,
    /// Whether `cvc privacy acknowledge-sharing` was completed for exactly
    /// this fingerprint. `false` when `fingerprint` is an error or
    /// `consent_error` is set, so it never reads as consent that could not be
    /// established.
    pub sharing_consented: bool,
    /// Whether `cvc privacy set-auto-push on` was completed for exactly this
    /// fingerprint. Same fail-closed rule as `sharing_consented`.
    pub auto_push: bool,
    /// Why `sharing_consented` and `auto_push` could not be read (an unreadable
    /// or insecure `privacy.json`), when that is the case. Then both are
    /// `false`, but that is "unknown", not "declined".
    pub consent_error: Option<String>,
}

#[allow(dead_code)] // Consumed by the interactive view (#112, Phase B).
impl RemoteOverview {
    /// The destination to hand to the share, unshare, publish and
    /// acknowledge flows in `sync`, resolved now rather than when the
    /// overview was built, exactly as the subcommands resolve `--remote`.
    ///
    /// Fails closed, with nothing resolved, when the remote is unusable or its
    /// fingerprint is no longer the one this overview showed (its URL or push
    /// URL was edited while the view was open): the person decided on the
    /// fingerprint on screen, so an action must not silently move to another
    /// destination.
    pub(crate) fn resolve(&self, repo: &Repository) -> Result<privacy::RemoteDestination> {
        let shown = match &self.fingerprint {
            Ok(fingerprint) => fingerprint,
            Err(reason) => bail!("remote '{}' is unusable: {reason}", self.name),
        };
        let destination = privacy::remote_destination(repo, &self.name)
            .map_err(|error| anyhow!("remote '{}': {}", self.name, display_reason(error)))?;
        if &destination.fingerprint != shown {
            bail!(
                "remote '{}' now resolves to a different destination than the one listed; refresh the remote list before acting",
                self.name
            );
        }
        Ok(destination)
    }
}

/// The repository's remotes in the order libgit2 lists them (the order
/// `git remote` prints), each with its destination fingerprint and consent
/// state. Read-only: nothing is written, fetched or resolved implicitly.
///
/// A remote that cannot be a destination, or whose consent state cannot be
/// read, is reported on its own entry and never fails the call; only being
/// unable to list the remotes at all is an error.
#[allow(dead_code)] // Consumed by the interactive view (#112, Phase B).
pub(crate) fn remote_overview(repo: &Repository) -> Result<Vec<RemoteOverview>> {
    let names = repo
        .remotes()
        .context("Failed to list the repository's remotes")?;
    Ok(names
        .iter_bytes()
        .map(|raw| match std::str::from_utf8(raw) {
            Ok(name) => overview_of(repo, name),
            // Not addressable by any command, but still worth showing.
            Err(_) => RemoteOverview {
                name: String::from_utf8_lossy(raw).into_owned(),
                fingerprint: Err("remote name is not valid UTF-8".into()),
                location: None,
                sharing_consented: false,
                auto_push: false,
                consent_error: None,
            },
        })
        .collect())
}

fn overview_of(repo: &Repository, name: &str) -> RemoteOverview {
    let destination = privacy::remote_destination(repo, name);
    let location = destination
        .as_ref()
        .ok()
        .and_then(|destination| privacy::display_location(&destination.effective_url));
    let fingerprint = destination
        .map(|destination| destination.fingerprint)
        .map_err(display_reason);
    let (sharing_consented, auto_push, consent_error) = match &fingerprint {
        Ok(fingerprint) => match privacy::privacy_status_for_fingerprint(repo, fingerprint) {
            Ok(status) => (status.sharing_consented, status.auto_push, None),
            Err(error) => (false, false, Some(display_reason(error))),
        },
        Err(_) => (false, false, None),
    };
    RemoteOverview {
        name: name.to_owned(),
        fingerprint,
        location,
        sharing_consented,
        auto_push,
        consent_error,
    }
}

/// The message of a privacy error without the `invalid .thoughtignore:` prefix
/// its `Display` puts on every policy failure, which is wrong for a remote or
/// consent problem.
fn display_reason(error: PrivacyError) -> String {
    match error {
        PrivacyError::Policy(reason) => reason,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repository_with(remotes: &[(&str, &str)]) -> (TempDir, Repository) {
        let directory = TempDir::new().unwrap();
        let repo = Repository::init(directory.path()).unwrap();
        for (name, url) in remotes {
            repo.remote(name, url).unwrap();
        }
        (directory, repo)
    }

    fn entry<'a>(overview: &'a [RemoteOverview], name: &str) -> &'a RemoteOverview {
        overview
            .iter()
            .find(|remote| remote.name == name)
            .unwrap_or_else(|| panic!("no remote {name} in {overview:?}"))
    }

    /// Remotes git lists (they have a `url` key) but that cannot be a
    /// destination: an empty URL, and one that is only whitespace.
    fn add_unusable_remotes(repo: &Repository) {
        let mut config = repo.config().unwrap();
        config.set_str("remote.emptyurl.url", "").unwrap();
        config.set_str("remote.blankurl.url", "   ").unwrap();
    }

    fn fingerprint_of(repo: &Repository, name: &str) -> String {
        privacy::remote_destination(repo, name).unwrap().fingerprint
    }

    #[test]
    fn plain_never_allows_the_interactive_view() {
        assert!(!interactive_ui_allowed(true));
    }

    #[test]
    fn remotes_are_listed_in_git_order_with_their_fingerprints() {
        let (_directory, repo) = repository_with(&[
            ("zeta", "https://example.invalid/z.git"),
            ("origin", "https://example.invalid/o.git"),
            ("alpha", "https://example.invalid/a.git"),
        ]);
        let overview = remote_overview(&repo).unwrap();
        // `git remote` prints these sorted, whatever order they were added in.
        let names: Vec<_> = overview.iter().map(|remote| remote.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "origin", "zeta"]);
        let git_order = repo.remotes().unwrap();
        let listed: Vec<_> = git_order.iter().flatten().collect();
        assert_eq!(names, listed);
        for remote in &overview {
            assert_eq!(
                remote.fingerprint.as_deref().unwrap(),
                fingerprint_of(&repo, &remote.name)
            );
            assert!(!remote.sharing_consented && !remote.auto_push);
            assert_eq!(remote.consent_error, None);
        }
    }

    #[test]
    fn the_location_is_sanitized_and_no_raw_url_is_kept() {
        let (_directory, repo) = repository_with(&[
            (
                "alpha",
                "https://user:tok-3xample@example.invalid/o/a.git?x=1#f",
            ),
            ("beta", "git@example.invalid:o/b.git"),
            (
                "gamma",
                "ssh://deploy:hunter-22@example.invalid:2222/o/c.git",
            ),
            ("delta", "/srv/repos/d.git"),
        ]);
        repo.remote_set_pushurl("gamma", Some("ssh://tok-9yz@push.example.invalid/o/c.git"))
            .unwrap();
        add_unusable_remotes(&repo);
        let overview = remote_overview(&repo).unwrap();
        let location = |name: &str| entry(&overview, name).location.as_deref();
        assert_eq!(location("alpha"), Some("example.invalid/o/a.git"));
        assert_eq!(location("beta"), Some("example.invalid:o/b.git"));
        // The effective push URL is what is described, not the fetch URL.
        assert_eq!(location("gamma"), Some("push.example.invalid/o/c.git"));
        assert_eq!(location("delta"), Some("/srv/repos/d.git"));
        assert_eq!(location("emptyurl"), None);
        assert_eq!(location("blankurl"), None);
        // Nothing of any credential survives, not even in `Debug`.
        let debug = format!("{overview:?}");
        for secret in ["tok-3xample", "hunter-22", "tok-9yz", "user:", "deploy"] {
            assert!(!debug.contains(secret), "{secret} leaked: {debug}");
        }
    }

    #[test]
    fn a_repository_without_remotes_has_an_empty_overview() {
        let (_directory, repo) = repository_with(&[]);
        assert!(remote_overview(&repo).unwrap().is_empty());
    }

    #[test]
    fn an_unusable_remote_is_reported_alone_and_does_not_fail_the_overview() {
        let (_directory, repo) = repository_with(&[("origin", "https://example.invalid/o.git")]);
        add_unusable_remotes(&repo);
        let overview = remote_overview(&repo).unwrap();
        assert_eq!(overview.len(), 3);

        for name in ["emptyurl", "blankurl"] {
            let broken = entry(&overview, name);
            let reason = broken.fingerprint.as_ref().unwrap_err();
            assert!(reason.contains("empty push URL"), "{name}: {reason}");
            assert!(
                !reason.contains(".thoughtignore"),
                "the policy prefix does not belong on a remote problem: {reason}"
            );
            assert!(!broken.sharing_consented && !broken.auto_push);
            assert_eq!(broken.consent_error, None);
        }

        let healthy = entry(&overview, "origin");
        assert_eq!(
            healthy.fingerprint.as_deref().unwrap(),
            fingerprint_of(&repo, "origin")
        );
    }

    #[test]
    fn consent_is_reported_per_destination_fingerprint() {
        let (_directory, repo) = repository_with(&[
            ("alpha", "https://example.invalid/a.git"),
            ("origin", "https://example.invalid/o.git"),
        ]);
        let alpha = privacy::remote_destination(&repo, "alpha").unwrap();
        privacy::acknowledge_sharing_destination(&repo, &alpha).unwrap();

        let overview = remote_overview(&repo).unwrap();
        assert!(entry(&overview, "alpha").sharing_consented);
        assert!(!entry(&overview, "alpha").auto_push);
        assert!(!entry(&overview, "origin").sharing_consented);

        privacy::set_auto_push_destination(&repo, &alpha, true).unwrap();
        let overview = remote_overview(&repo).unwrap();
        assert!(entry(&overview, "alpha").sharing_consented);
        assert!(entry(&overview, "alpha").auto_push);
        assert!(!entry(&overview, "origin").auto_push);
    }

    #[test]
    fn consent_for_one_url_grants_nothing_for_another() {
        let (_directory, repo) = repository_with(&[("origin", "https://example.invalid/o.git")]);
        let before = privacy::remote_destination(&repo, "origin").unwrap();
        privacy::acknowledge_sharing_destination(&repo, &before).unwrap();
        assert!(remote_overview(&repo).unwrap()[0].sharing_consented);

        // The effective push URL is what consent is bound to.
        repo.remote_set_pushurl("origin", Some("https://example.invalid/elsewhere.git"))
            .unwrap();
        let overview = remote_overview(&repo).unwrap();
        let origin = entry(&overview, "origin");
        assert_ne!(origin.fingerprint.as_deref().unwrap(), before.fingerprint);
        assert!(!origin.sharing_consented);
        assert!(!origin.auto_push);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_consent_state_is_unknown_not_declined_and_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, repo) = repository_with(&[
            ("alpha", "https://example.invalid/a.git"),
            ("origin", "https://example.invalid/o.git"),
        ]);
        let alpha = privacy::remote_destination(&repo, "alpha").unwrap();
        privacy::acknowledge_sharing_destination(&repo, &alpha).unwrap();
        let state = repo.path().join("cvc").join("privacy.json");
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o644)).unwrap();

        let overview = remote_overview(&repo).unwrap();
        assert_eq!(overview.len(), 2);
        for remote in &overview {
            assert!(remote.fingerprint.is_ok());
            assert!(!remote.sharing_consented && !remote.auto_push);
            let reason = remote.consent_error.as_deref().unwrap();
            assert!(reason.contains("insecure"), "{reason}");
            assert!(!reason.contains(".thoughtignore"), "{reason}");
        }
    }

    #[test]
    fn resolve_returns_the_destination_the_overview_showed() {
        let (_directory, repo) = repository_with(&[("origin", "https://example.invalid/o.git")]);
        let overview = remote_overview(&repo).unwrap();
        let destination = overview[0].resolve(&repo).unwrap();
        assert_eq!(destination.name, "origin");
        assert_eq!(
            &destination.fingerprint,
            overview[0].fingerprint.as_ref().unwrap()
        );
    }

    #[test]
    fn resolve_refuses_an_unusable_remote_and_a_remote_that_moved() {
        let (_directory, repo) = repository_with(&[("origin", "https://example.invalid/o.git")]);
        add_unusable_remotes(&repo);
        let overview = remote_overview(&repo).unwrap();
        let error = entry(&overview, "emptyurl").resolve(&repo).unwrap_err();
        assert!(error.to_string().contains("unusable"), "{error}");

        repo.remote_set_pushurl("origin", Some("https://example.invalid/moved.git"))
            .unwrap();
        let error = entry(&overview, "origin").resolve(&repo).unwrap_err();
        assert!(
            error.to_string().contains("different destination"),
            "{error}"
        );

        // A remote deleted since the listing is refused too.
        repo.remote_delete("origin").unwrap();
        let error = entry(&overview, "origin").resolve(&repo).unwrap_err();
        assert!(error.to_string().contains("not configured"), "{error}");
        assert!(!error.to_string().contains(".thoughtignore"), "{error}");
    }
}
