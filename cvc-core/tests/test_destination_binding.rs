//! Transport is bound to the consented destination: a chained git URL rewrite
//! that would apply again at transport time, or a remote that no longer
//! resolves to the destination an operation was bound to, is refused before
//! any connection is made.
use cvc_core::privacy::{remote_destination, RemoteDestination};
use cvc_core::sync::{self, SyncError};
use git2::{Oid, Repository};
use std::path::Path;
use tempfile::TempDir;

/// What `origin` is configured with. It is never contacted: one rule rewrites it.
const CONFIGURED_URL: &str = "https://example.invalid/team/project";
const CONFIGURED_BASE: &str = "https://example.invalid/team/";
const TRACKING_REF: &str = "refs/remotes/origin/cvc/main";
const CANDIDATE_REF: &str = "refs/cvc/tmp/candidate";

/// A working repository at `<root>/work` whose `origin` is [`CONFIGURED_URL`],
/// with one rule, `url.<root>/a/.insteadOf = https://example.invalid/team/`,
/// so that the consented URL is `<root>/a/project`: a bare repository with a
/// branch but no CVC ref yet, as a project that never received CVC data is.
/// (Not one with no refs at all: git2 0.20's `Remote::list` then builds its
/// slice from a null pointer, which debug builds abort on.)
///
/// `<root>/b/project` is a second bare repository, on a root that does not
/// start with `<root>/a/`, which already holds `refs/cvc/main`: that is where
/// the rules the tests add would send transport, and a fetch that reached it
/// would create [`TRACKING_REF`]. The working repository holds a candidate
/// ref, so a push that reached either one would land there.
struct Fixture {
    root: TempDir,
    repo: Repository,
    elsewhere_tip: Oid,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let repo = Repository::init(root.path().join("work")).unwrap();
        repo.remote("origin", CONFIGURED_URL).unwrap();
        let candidate = empty_commit(&repo);
        repo.reference(CANDIDATE_REF, candidate, false, "fixture")
            .unwrap();
        let consented = Repository::init_bare(root.path().join("a").join("project")).unwrap();
        let branch = empty_commit(&consented);
        consented
            .reference("refs/heads/main", branch, false, "fixture")
            .unwrap();
        let elsewhere = Repository::init_bare(root.path().join("b").join("project")).unwrap();
        let elsewhere_tip = empty_commit(&elsewhere);
        elsewhere
            .reference("refs/cvc/main", elsewhere_tip, false, "fixture")
            .unwrap();
        let fixture = Self {
            root,
            repo,
            elsewhere_tip,
        };
        fixture.set(
            &format!("url.{}.insteadOf", fixture.base()),
            CONFIGURED_BASE,
        );
        fixture
    }

    /// `<root>/a/`: what the one rule rewrites [`CONFIGURED_BASE`] to.
    fn base(&self) -> String {
        format!("{}/a/", self.root.path().display())
    }

    /// `<root>/b/`: where a second rule sends what starts with [`Self::base`].
    fn elsewhere(&self) -> String {
        format!("{}/b/", self.root.path().display())
    }

    fn set(&self, key: &str, value: &str) {
        self.repo.config().unwrap().set_str(key, value).unwrap();
    }

    fn destination(&self) -> RemoteDestination {
        remote_destination(&self.repo, "origin").unwrap()
    }

    fn consented_cvc_ref(&self) -> Option<Oid> {
        cvc_ref(&self.root.path().join("a").join("project"))
    }

    /// Neither repository received anything: the consented one still has no
    /// CVC ref, and the other still has the one it started with.
    fn assert_nothing_pushed(&self) {
        assert_eq!(self.consented_cvc_ref(), None, "nothing reached <root>/a");
        assert_eq!(
            cvc_ref(&self.root.path().join("b").join("project")),
            Some(self.elsewhere_tip),
            "nothing reached <root>/b"
        );
    }

    fn assert_nothing_fetched(&self) {
        assert!(
            self.repo.find_reference(TRACKING_REF).is_err(),
            "nothing was fetched"
        );
    }
}

fn empty_commit(repo: &Repository) -> Oid {
    let tree = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    repo.commit(None, &signature, &signature, "fixture", &tree, &[])
        .unwrap()
}

fn cvc_ref(bare: &Path) -> Option<Oid> {
    Repository::open_bare(bare)
        .unwrap()
        .refname_to_id("refs/cvc/main")
        .ok()
}

#[test]
fn one_rewrite_rule_is_the_consented_destination_and_transport_proceeds() {
    let fixture = Fixture::new();
    let destination = fixture.destination();
    // One rule: the consented URL is the rewritten one, applied once.
    assert_eq!(
        destination.effective_url,
        format!("{}project", fixture.base())
    );

    // No CVC ref there yet: the advertisement proves it absent, which is a
    // successful fetch of nothing, not a refusal.
    sync::fetch_destination(&fixture.repo, &destination).unwrap();
    fixture.assert_nothing_fetched();

    // A push reaches the same repository, and a fetch then brings it back.
    sync::push_temp_ref(&fixture.repo, &destination, CANDIDATE_REF).unwrap();
    let candidate = fixture.repo.refname_to_id(CANDIDATE_REF).unwrap();
    assert_eq!(fixture.consented_cvc_ref(), Some(candidate));
    sync::fetch_destination(&fixture.repo, &destination).unwrap();
    assert_eq!(
        fixture.repo.refname_to_id(TRACKING_REF).ok(),
        Some(candidate)
    );
}

#[test]
fn a_chained_rewrite_that_would_apply_again_is_refused_before_connecting() {
    let fixture = Fixture::new();
    // The second rule matches the already rewritten URL. git applies the rules
    // once, so the consented URL does not change, but libgit2 applies them
    // again to an anonymous remote and would send transport to <root>/b.
    fixture.set(
        &format!("url.{}.insteadOf", fixture.elsewhere()),
        &fixture.base(),
    );
    let destination = fixture.destination();
    assert_eq!(
        destination.effective_url,
        format!("{}project", fixture.base())
    );

    let fetched = sync::fetch_destination(&fixture.repo, &destination);
    assert!(
        fetched
            .as_ref()
            .is_err_and(SyncError::refused_before_transport),
        "{fetched:?}"
    );
    match fetched {
        Err(SyncError::DestinationRewritten { consented, actual }) => {
            assert_eq!(consented, format!("{}project", fixture.base()));
            assert_eq!(actual, format!("{}project", fixture.elsewhere()));
        }
        other => panic!("expected DestinationRewritten, got {other:?}"),
    }
    fixture.assert_nothing_fetched();

    let pushed = sync::push_temp_ref(&fixture.repo, &destination, CANDIDATE_REF);
    assert!(
        matches!(pushed, Err(SyncError::DestinationRewritten { .. })),
        "{pushed:?}"
    );
    fixture.assert_nothing_pushed();
}

#[test]
fn a_push_rewrite_rule_that_matches_the_consented_url_is_refused_too() {
    let fixture = Fixture::new();
    let destination = fixture.destination();
    fixture.set(
        &format!("url.{}.pushInsteadOf", fixture.elsewhere()),
        &fixture.base(),
    );
    // DestinationRewritten, not DestinationChanged: on lookup libgit2 applies
    // pushInsteadOf only to the remote's configured URL, which this rule does
    // not match, so `origin` still resolves to the same destination; only the
    // anonymous transport remote, built from the rewritten URL, gets the rule's
    // URL as its push URL.
    assert_eq!(fixture.destination().fingerprint, destination.fingerprint);
    sync::verify_destination_current(&fixture.repo, &destination).unwrap();

    match sync::push_temp_ref(&fixture.repo, &destination, CANDIDATE_REF) {
        Err(SyncError::DestinationRewritten { consented, actual }) => {
            assert_eq!(consented, format!("{}project", fixture.base()));
            assert_eq!(actual, format!("{}project", fixture.elsewhere()));
        }
        other => panic!("expected DestinationRewritten, got {other:?}"),
    }
    fixture.assert_nothing_pushed();

    // The transport remote must resolve to the consented URL in both
    // directions, so a fetch is refused as well, before connecting.
    let fetched = sync::fetch_destination(&fixture.repo, &destination);
    assert!(
        matches!(fetched, Err(SyncError::DestinationRewritten { .. })),
        "{fetched:?}"
    );
    fixture.assert_nothing_fetched();
}

#[test]
fn a_refusal_shows_locations_without_their_credentials() {
    let fixture = Fixture::new();
    fixture.set(
        "url.https://user:tok-secret-value@host.example/.insteadOf",
        &fixture.base(),
    );
    let destination = fixture.destination();
    let error = sync::fetch_destination(&fixture.repo, &destination).unwrap_err();
    match &error {
        SyncError::DestinationRewritten { actual, .. } => {
            assert_eq!(actual, "host.example/project");
        }
        other => panic!("expected DestinationRewritten, got {other:?}"),
    }
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains("tok-secret-value"), "{shown}");
        assert!(!shown.contains("user:"), "{shown}");
    }
    fixture.assert_nothing_fetched();
}

/// `Remote::url()` is `None` for a URL that is not UTF-8, so the check
/// compares bytes: a chained rule whose replacement is not UTF-8 is refused as
/// well, and the location it would reach is not shown at all.
#[cfg(unix)]
#[test]
fn a_chained_rewrite_to_a_url_that_is_not_utf8_is_refused() {
    let fixture = Fixture::new();
    // The config API writes only UTF-8 keys; a config file holds bytes.
    let config = fixture.repo.path().join("config");
    let mut raw = std::fs::read(&config).unwrap();
    raw.extend_from_slice(b"[url \"");
    raw.extend_from_slice(format!("{}/b", fixture.root.path().display()).as_bytes());
    raw.push(0xff);
    raw.extend_from_slice(b"/\"]\n\tinsteadOf = ");
    raw.extend_from_slice(fixture.base().as_bytes());
    raw.push(b'\n');
    std::fs::write(&config, raw).unwrap();
    let destination = fixture.destination();
    assert_eq!(
        destination.effective_url,
        format!("{}project", fixture.base())
    );

    match sync::fetch_destination(&fixture.repo, &destination) {
        Err(SyncError::DestinationRewritten { consented, actual }) => {
            assert_eq!(consented, format!("{}project", fixture.base()));
            assert_eq!(actual, "(location not shown)");
        }
        other => panic!("expected DestinationRewritten, got {other:?}"),
    }
    fixture.assert_nothing_fetched();
    let pushed = sync::push_temp_ref(&fixture.repo, &destination, CANDIDATE_REF);
    assert!(
        matches!(pushed, Err(SyncError::DestinationRewritten { .. })),
        "{pushed:?}"
    );
    fixture.assert_nothing_pushed();
}

#[test]
fn a_remote_that_changed_since_resolution_is_refused_and_a_removed_one_is_not() {
    let fixture = Fixture::new();
    let destination = fixture.destination();

    // The remote's URL changes while the operation holds the destination, for
    // instance while a consent prompt is open. Transport would still go to the
    // bound URL, which is reachable, so only the refusal keeps it from going.
    fixture
        .repo
        .remote_set_url("origin", "https://example.invalid/other/project")
        .unwrap();
    let result = sync::verify_destination_current(&fixture.repo, &destination);
    assert!(
        matches!(result, Err(SyncError::DestinationChanged { ref remote }) if remote == "origin"),
        "{result:?}"
    );
    let fetched = sync::fetch_destination(&fixture.repo, &destination);
    assert!(
        matches!(fetched, Err(SyncError::DestinationChanged { .. })),
        "{fetched:?}"
    );
    assert!(fetched.is_err_and(|error| error.refused_before_transport()));
    fixture.assert_nothing_fetched();
    let pushed = sync::push_temp_ref(&fixture.repo, &destination, CANDIDATE_REF);
    assert!(
        matches!(pushed, Err(SyncError::DestinationChanged { .. })),
        "{pushed:?}"
    );
    fixture.assert_nothing_pushed();

    // A remote that is still configured but no longer resolves to any
    // destination cannot be shown to be the bound one either.
    fixture.set("remote.origin.url", "");
    let result = sync::verify_destination_current(&fixture.repo, &destination);
    assert!(
        matches!(result, Err(SyncError::DestinationChanged { .. })),
        "{result:?}"
    );

    // Removing the remote is not a change of destination: the bound URL stands.
    fixture.repo.remote_delete("origin").unwrap();
    sync::verify_destination_current(&fixture.repo, &destination).unwrap();
    sync::fetch_destination(&fixture.repo, &destination).unwrap();
    fixture.assert_nothing_fetched();
}
