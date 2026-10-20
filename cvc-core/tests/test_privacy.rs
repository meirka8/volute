use chrono::Utc;
use cvc_core::db::CvcStore;
use cvc_core::models::FutureSharePolicy;
use cvc_core::models::{
    Author, ContextItem, Conversation, Interaction, InteractionId, ToolExecution, ToolStatus,
};
use cvc_core::privacy::{
    acknowledge_sharing, privacy_status, scrub, scrub_json, validate_sanitized_json, CliRunCapture,
    McpCapture, PreparedPolicy,
};
use cvc_core::sync;
use git2::Repository;
use tempfile::tempdir;

/// "No remote baseline": pins a tracking ref that does not exist, so the
/// projection starts from an empty tree and verifies that nothing appeared.
fn no_baseline(repo: &Repository) -> sync::ImportedBaseline {
    sync::ImportedBaseline::observe(repo, "refs/remotes/unpublished/cvc/main").unwrap()
}

fn project_fixture_to_ref(repo: &Repository, store: &CvcStore, ref_name: &str, destination: &str) {
    let projection =
        sync::push_projection_to_ref(repo, store, &no_baseline(repo), destination).unwrap();
    let sync::ProjectionResult::Candidate { oid, candidate, .. } = projection else {
        panic!("fixture expected projection candidate");
    };
    repo.reference(ref_name, oid, true, "fixture: remote accepted candidate")
        .unwrap();
    drop(candidate);
}

fn interaction(id: InteractionId, conversation_id: &str) -> Interaction {
    Interaction {
        id,
        conversation_id: conversation_id.into(),
        parent_id: None,
        timestamp: Utc::now(),
        author: Author::Human,
        user_prompt: "sk-abcdefghijklmnopqrstuvwxyz123456".into(),
        model_name: None,
        model_cot: None,
        model_response: None,
        source_request_id: None,
    }
}

#[test]
fn scrub_covers_sensitive_forms() {
    assert!(!scrub("AKIAABCDEFGHIJKLMNOP secret=supersecretvalue")
        .unwrap()
        .contains("supersecretvalue"));
    assert_eq!(
        scrub("0123456789abcdef0123456789abcdef01234567").unwrap(),
        "0123456789abcdef0123456789abcdef01234567"
    );
}

#[test]
fn credential_subtrees_keep_shape_but_not_raw_leaves() {
    let scrubbed = scrub_json(
        r#"{"password":{"value":"ordinary-sensitive-value","items":["nested-secret"]}}"#,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&scrubbed).unwrap();
    assert!(value["password"].is_object());
    assert!(!scrubbed.contains("ordinary-sensitive-value"));
    assert!(!scrubbed.contains("nested-secret"));
}

#[test]
fn sanitized_nested_credential_json_is_idempotent() {
    let once = scrub_json(r#"{"password":{"value":"raw-value","more":["raw-two"]}}"#).unwrap();
    assert_eq!(scrub_json(&once).unwrap(), once);
    assert!(validate_sanitized_json(&once).is_ok());
    assert!(validate_sanitized_json(r#"{"password":{"value":"raw-value"}}"#).is_err());
}

#[test]
fn policy_absent_valid_invalid_and_links_fail_closed() {
    let dir = tempdir().unwrap();
    assert!(!PreparedPolicy::load(dir.path())
        .unwrap()
        .ignores_path("src/a"));
    std::fs::write(
        dir.path().join(".thoughtignore"),
        "path:secrets\nliteral:DO_NOT_STORE",
    )
    .unwrap();
    assert!(PreparedPolicy::load(dir.path())
        .unwrap()
        .ignores_path("secrets/key"));
    std::fs::write(dir.path().join(".thoughtignore"), "path:../escape").unwrap();
    assert!(PreparedPolicy::load(dir.path()).is_err());
    std::fs::remove_file(dir.path().join(".thoughtignore")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("missing", dir.path().join(".thoughtignore")).unwrap();
        assert!(
            PreparedPolicy::load(dir.path()).is_err(),
            "dangling symlink is not absent"
        );
        std::fs::remove_file(dir.path().join(".thoughtignore")).unwrap();
        std::fs::write(dir.path().join("target"), "path:secret").unwrap();
        std::os::unix::fs::symlink("target", dir.path().join(".thoughtignore")).unwrap();
        assert!(PreparedPolicy::load(dir.path()).is_err());
    }
}

#[test]
fn aggregate_uses_the_supplied_policy_snapshot() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".thoughtignore"), "path:private").unwrap();
    let policy = PreparedPolicy::load(dir.path()).unwrap();
    // Replacing the file after load must not change the capture's policy.
    std::fs::write(dir.path().join(".thoughtignore"), "path:other").unwrap();
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let id = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "c".into(),
                title: "c".into(),
                created_at: Utc::now(),
            },
            interaction(id.clone(), "c"),
            vec![ContextItem {
                id: None,
                interaction_id: id.clone(),
                file_path: "private/key".into(),
                git_blob_sha: None,
                dirty_patch: Some("secret=supersecretvalue".into()),
                start_line: None,
                end_line: None,
            }],
            Vec::new(),
            policy,
            "0".repeat(64),
        ))
        .unwrap();
    assert!(store.get_context_items(&id).unwrap().is_empty());
    assert!(!store
        .get_interaction(&id)
        .unwrap()
        .unwrap()
        .user_prompt
        .contains("abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn malformed_policy_aborts_before_aggregate_write() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".thoughtignore"), "invalid: policy").unwrap();
    assert!(PreparedPolicy::load(dir.path()).is_err());
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let id = InteractionId::new();
    store
        .capture_cli_run(CliRunCapture::new(
            Conversation {
                id: "c".into(),
                title: "c".into(),
                created_at: Utc::now(),
            },
            interaction(id.clone(), "c"),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    assert!(store.get_interaction(&id).unwrap().is_some());
}

#[test]
fn sharing_consent_is_remote_fingerprinted_and_defaults_off() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.remote("origin", "https://example.test/one.git")
        .unwrap();
    let fresh = privacy_status(&repo, "origin").unwrap();
    assert!(!fresh.capture_acknowledged && !fresh.sharing_consented && !fresh.auto_push);
    acknowledge_sharing(&repo, "origin").unwrap();
    assert!(privacy_status(&repo, "origin").unwrap().sharing_consented);
    repo.remote_set_url("origin", "https://example.test/two.git")
        .unwrap();
    assert!(!privacy_status(&repo, "origin").unwrap().sharing_consented);
}

#[test]
fn sharing_is_atomic_and_projection_never_includes_private_peers() {
    let dir = tempdir().unwrap();
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let first = InteractionId::new();
    let second = InteractionId::new();
    for (id, conversation) in [(&first, "share-me"), (&second, "private-peer")] {
        store
            .capture_mcp(McpCapture::new(
                Conversation {
                    id: conversation.into(),
                    title: conversation.into(),
                    created_at: Utc::now(),
                },
                interaction(id.clone(), conversation),
                Vec::new(),
                Vec::new(),
                PreparedPolicy::built_ins_only(),
                "0".repeat(64),
            ))
            .unwrap();
    }
    assert!(store
        .projection_interaction_ids("remote")
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .share_conversation_for_remote("share-me", "remote", FutureSharePolicy::Private)
            .unwrap(),
        1
    );
    assert_eq!(
        store.projection_interaction_ids("remote").unwrap(),
        vec![first.clone()]
    );
    // Explicit future policy is persisted; an unrelated conversation is untouched.
    let future = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "share-me".into(),
                title: "share-me".into(),
                created_at: Utc::now(),
            },
            interaction(future.clone(), "share-me"),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    assert_eq!(
        store.projection_interaction_ids("remote").unwrap(),
        vec![first]
    );
}

#[test]
fn sharing_one_destination_never_authorizes_another() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.remote("origin", "https://example.test/origin.git")
        .unwrap();
    repo.remote("backup", "https://example.test/backup.git")
        .unwrap();
    let origin = privacy_status(&repo, "origin").unwrap().remote_fingerprint;
    let backup = privacy_status(&repo, "backup").unwrap().remote_fingerprint;
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let id = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "multi-remote".into(),
                title: "multi-remote".into(),
                created_at: Utc::now(),
            },
            interaction(id.clone(), "multi-remote"),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    store
        .share_conversation_for_remote("multi-remote", &backup, FutureSharePolicy::Private)
        .unwrap();
    assert!(store
        .projection_interaction_ids(&origin)
        .unwrap()
        .is_empty());
    assert_eq!(store.projection_interaction_ids(&backup).unwrap(), vec![id]);
}

#[test]
fn pulled_publication_does_not_grant_sharing_to_another_remote() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.remote("source", "https://example.test/source.git")
        .unwrap();
    repo.remote("other", "https://example.test/other.git")
        .unwrap();

    let source = CvcStore::open(dir.path().join("source.db")).unwrap();
    let id = InteractionId::new();
    source
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "remote-conversation".into(),
                title: "source".into(),
                created_at: Utc::now(),
            },
            interaction(id.clone(), "remote-conversation"),
            Vec::new(),
            Vec::new(),
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    let source_fingerprint = privacy_status(&repo, "source").unwrap().remote_fingerprint;
    source
        .share_conversation_for_remote(
            "remote-conversation",
            &source_fingerprint,
            FutureSharePolicy::Private,
        )
        .unwrap();
    project_fixture_to_ref(&repo, &source, "refs/cvc/source", &source_fingerprint);

    let imported = CvcStore::open(dir.path().join("imported.db")).unwrap();
    sync::pull_from_ref_for_remote(
        &repo,
        &imported,
        "refs/cvc/source",
        Some(&source_fingerprint),
    )
    .unwrap();
    let other_fingerprint = privacy_status(&repo, "other").unwrap().remote_fingerprint;
    assert!(imported
        .projection_interaction_ids(&other_fingerprint)
        .unwrap()
        .is_empty());
    assert!(imported
        .projection_interaction_ids(&source_fingerprint)
        .unwrap()
        .is_empty());

    // A later explicit local share is the only transition that makes it eligible.
    imported
        .share_conversation_for_remote(
            &format!("remote:{source_fingerprint}:remote-conversation"),
            &other_fingerprint,
            FutureSharePolicy::Private,
        )
        .unwrap();
    assert_eq!(
        imported
            .projection_interaction_ids(&other_fingerprint)
            .unwrap(),
        vec![id]
    );
}

#[cfg(unix)]
#[test]
fn privacy_store_files_are_private_and_reject_symlinks() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let cvc_dir = dir.path().join("cvc");
    let db = cvc_dir.join("index.db");
    let _store = CvcStore::open(&db).unwrap();
    assert_eq!(
        std::fs::metadata(&cvc_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for suffix in ["-wal", "-shm"] {
        let sidecar = std::path::PathBuf::from(format!("{}{}", db.display(), suffix));
        if sidecar.exists() {
            assert_eq!(
                std::fs::metadata(sidecar).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    let target = dir.path().join("target.db");
    std::fs::write(&target, b"not sqlite").unwrap();
    let link = dir.path().join("linked.db");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(CvcStore::open(&link).is_err());
}

#[test]
fn aws_pair_is_absent_from_all_captured_fields_db_wal_and_projection() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let db_path = dir.path().join("index.db");
    let store = CvcStore::open(&db_path).unwrap();
    let key = "AKIAABCDEFGHIJKLMNOP";
    let secret = "aws_secret_access_key=abcdefghijklmnopqrstuvwx1234567890";
    let id = InteractionId::new();
    let mut captured = interaction(id.clone(), "aws-matrix");
    captured.user_prompt = format!("prompt {key} {secret}");
    captured.model_name = Some(format!("model {key}"));
    captured.model_cot = Some(format!("cot {secret}"));
    captured.model_response = Some(format!("response {key}"));
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "aws-matrix".into(),
                title: format!("title {key}"),
                created_at: Utc::now(),
            },
            captured,
            vec![ContextItem {
                id: None,
                interaction_id: id.clone(),
                file_path: format!("src/{key}.rs"),
                git_blob_sha: None,
                dirty_patch: Some(format!("patch {secret}")),
                start_line: None,
                end_line: None,
            }],
            vec![ToolExecution {
                id: None,
                interaction_id: id.clone(),
                tool_protocol: format!("mcp-{key}"),
                tool_name: format!("tool-{secret}"),
                arguments: format!(
                    r#"{{"nested":{{"aws_access_key":"{key}","secret":"{secret}"}}}}"#
                ),
                status: ToolStatus::Success,
            }],
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    store
        .share_conversation_for_remote("aws-matrix", "remote", FutureSharePolicy::Private)
        .unwrap();
    let saved = store.get_interaction(&id).unwrap().unwrap();
    assert!(!format!("{:?}", saved).contains(key));
    assert!(!format!("{:?}", store.get_context_items(&id).unwrap()).contains(secret));
    assert!(!format!("{:?}", store.get_tool_executions(&id).unwrap()).contains(key));
    let bytes = std::fs::read(&db_path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(key));
    let wal = std::path::PathBuf::from(format!("{}-wal", db_path.display()));
    if wal.exists() {
        assert!(!String::from_utf8_lossy(&std::fs::read(wal).unwrap()).contains(secret));
    }
    let projection =
        sync::push_projection_to_ref(&repo, &store, &no_baseline(&repo), "remote").unwrap();
    let sync::ProjectionResult::Candidate { oid, candidate, .. } = projection else {
        panic!("expected projection")
    };
    let tree = repo.find_commit(oid).unwrap().tree().unwrap();
    let nodes = tree
        .get_name("nodes")
        .unwrap()
        .to_object(&repo)
        .unwrap()
        .peel_to_tree()
        .unwrap();
    let shard = nodes
        .get_name(&id.as_str()[..2])
        .unwrap()
        .to_object(&repo)
        .unwrap()
        .peel_to_tree()
        .unwrap();
    let blob = repo
        .find_blob(shard.get_name(&format!("{id}.json")).unwrap().id())
        .unwrap();
    let wire = String::from_utf8_lossy(blob.content());
    assert!(!wire.contains(key) && !wire.contains(secret));
    drop(candidate);
}

#[test]
fn share_snapshot_race_rejects_new_turn_without_sharing_it() {
    let dir = tempdir().unwrap();
    let store = CvcStore::open(dir.path().join("index.db")).unwrap();
    let first = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "race".into(),
                title: "race".into(),
                created_at: Utc::now(),
            },
            interaction(first.clone(), "race"),
            vec![],
            vec![],
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    let snapshot = store.share_snapshot("race").unwrap();
    let second = InteractionId::new();
    store
        .capture_mcp(McpCapture::new(
            Conversation {
                id: "race".into(),
                title: "race".into(),
                created_at: Utc::now(),
            },
            interaction(second.clone(), "race"),
            vec![],
            vec![],
            PreparedPolicy::built_ins_only(),
            "0".repeat(64),
        ))
        .unwrap();
    assert!(store
        .share_exact_snapshot("race", "remote", &snapshot, FutureSharePolicy::Private)
        .is_err());
    assert!(store
        .projection_interaction_ids("remote")
        .unwrap()
        .is_empty());
}

fn shown(raw: &str) -> Option<String> {
    cvc_core::privacy::display_location(raw)
}

#[test]
fn display_location_keeps_host_port_and_path_of_scheme_urls_and_nothing_else() {
    for (raw, expected) in [
        ("https://user:tok@host/a/b.git", "host/a/b.git"),
        ("https://tok@host/p?x=1#f", "host/p"),
        ("https://us%40er:p%3Ass@host/a", "host/a"),
        ("https://host/a/b.git?token=abc#frag", "host/a/b.git"),
        ("ssh://git@host:22/p", "host:22/p"),
        ("ssh://user:pw@host/p", "host/p"),
        ("git://host/o/r.git", "host/o/r.git"),
        (
            "https://HOST.Example/Org/Repo.git",
            "host.example/Org/Repo.git",
        ),
        ("https://host:8443", "host:8443"),
        ("https://host", "host"),
        ("https://host/", "host"),
        ("ssh://host", "host"),
        // The scheme's own default port is not shown.
        ("https://host:443/p", "host/p"),
        ("https://host/a%20b.git", "host/a%20b.git"),
        (
            "https://[2001:db8::1]:8443/r.git",
            "[2001:db8::1]:8443/r.git",
        ),
        ("https://u:p@[::1]/r.git", "[::1]/r.git"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

#[test]
fn display_location_drops_the_user_of_scp_like_remotes() {
    for (raw, expected) in [
        ("git@github.com:o/r.git", "github.com:o/r.git"),
        ("github.com:o/r.git", "github.com:o/r.git"),
        ("user@host.example:~/repo", "host.example:~/repo"),
        ("git@[2001:db8::1]:o/r.git", "[2001:db8::1]:o/r.git"),
        ("[::1]:path", "[::1]:path"),
        ("a@b@host:p", "host:p"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

#[test]
fn display_location_shows_local_paths_as_they_are() {
    for (raw, expected) in [
        ("/srv/r.git", "/srv/r.git"),
        ("  /srv/r.git  ", "/srv/r.git"),
        ("../sibling.git", "../sibling.git"),
        ("relative/dir", "relative/dir"),
        // A slash before the colon makes it a path, not host:path.
        ("/srv/a:b/r.git", "/srv/a:b/r.git"),
        ("C:\\repos\\x.git", "C:\\repos\\x.git"),
        ("C:/repos/x.git", "C:/repos/x.git"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
    #[cfg(unix)]
    for (raw, expected) in [
        ("file:///srv/r.git", "/srv/r.git"),
        ("file:///srv/r%20x.git", "/srv/r x.git"),
        ("file://localhost/srv/r.git", "/srv/r.git"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

#[test]
fn display_location_refuses_what_it_cannot_show_safely() {
    let secret_in_path = format!(
        "https://host/{}",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz"
    );
    let too_long = format!("https://host/{}", "a".repeat(3000));
    for raw in [
        "",
        "   ",
        "http://",
        "https://",
        "https://[::1",
        "https://host:notaport/p",
        "://x",
        "ht tp://x",
        "mailto:someone@example.com",
        // Git would read this as host `user` and path `secret@host:path`.
        "user:secret@host:path",
        // A query or fragment in a form that is not a URL has nothing to strip.
        "git@host:o/r.git?x=1",
        "/srv/r.git#frag",
        // Not a URL, not host:path, and holds an `@`: shown as nothing.
        "token@github.com/o/r",
        "git@github.com",
        "host:",
        "ho st:path",
        // Control and direction-changing characters.
        "host:\u{1b}[31mred",
        "host:line\nbreak",
        "/srv/\u{202e}evil.git",
        "file://server/share/r.git",
        // The built-in scrubber would redact something in the form.
        secret_in_path.as_str(),
        too_long.as_str(),
    ] {
        assert_eq!(shown(raw), None, "{raw:?}");
    }
}

#[test]
fn display_location_never_shows_a_credential() {
    let secrets = [
        "s3cr3t-value",
        "p%40ss",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
        "hunter2",
    ];
    for secret in secrets {
        for raw in [
            format!("https://user:{secret}@host/r.git"),
            format!("https://{secret}@host/r.git"),
            format!("ssh://{secret}@host/r.git"),
            format!("ssh://git:{secret}@host:2222/r.git"),
            format!("{secret}@host:o/r.git"),
            format!("git@host:o/r.git?token={secret}"),
            format!("user:{secret}@host:o/r.git"),
            format!("https://host/r.git?token={secret}"),
            format!("https://host/r.git#{secret}"),
            format!("https://user:{secret}@host:8443/r.git?x={secret}#{secret}"),
            format!("https://user:{secret}@[::1]/r.git"),
        ] {
            if let Some(location) = shown(&raw) {
                assert!(
                    !location.contains(secret) && !location.contains('@'),
                    "{raw} -> {location}"
                );
            }
        }
    }
}

#[test]
fn display_location_never_shows_a_host_other_than_the_one_git_contacts() {
    // The `url` crate ends the authority at `?`, `#` and `\`; libgit2 ends it
    // at the first `/` and takes the host after the last `@`. Each of these
    // parses as host `github.com` and is contacted as `evil.example`.
    for raw in [
        "https://github.com?@evil.example/o/r.git",
        "https://github.com#@evil.example/o/r.git",
        "https://github.com\\@evil.example/o/r.git",
        "ssh://github.com?@evil.example/o/r.git",
        "ssh://github.com#@evil.example/o/r.git",
        "ssh://github.com\\@evil.example/o/r.git",
        "git://github.com?@evil.example/o/r.git",
        "https://user@github.com?@evil.example/o/r.git",
        "https://github.com:443?@evil.example:8443/o/r.git",
        "https://github.com?x=1@evil.example/o/r.git",
        "https://github.com#frag@evil.example",
        "ssh+git://[::1]?@evil.example/r",
        // The host that is shown must also be the one that is written: a
        // normalized form (IDNA, percent-encoding, numeric) is not.
        "https://%65xample.com/r",
        "https://\u{430}pple.com/r",
        "https://0x7f.1/r",
        "https://2130706433/r",
        "ssh://h%C3%B6st/r",
        "ssh://github.com%40evil.example/r",
        "https://[2001:0db8::1]/r",
        // Nothing before the `://` may be anything but a scheme.
        "ht tp://github.com/r",
        "/srv/a://github.com/r",
        "git@host:o/r://github.com/r",
        // A `\` in a URL is a separator to one parser and a character to the
        // other, so the path shown might not be the path used either.
        "https://github.com/o\\r.git",
        "file:///srv\\r.git",
        // `file` has no host to contact, and only `localhost` means that.
        "file://github.com/srv/r.git",
        "file://user@/srv/r.git",
    ] {
        assert_eq!(shown(raw), None, "{raw}");
    }
    // The ordinary forms next to them are still shown.
    for (raw, expected) in [
        ("https://github.com/o/r.git", "github.com/o/r.git"),
        ("https://user@github.com/o/r.git", "github.com/o/r.git"),
        (
            "https://user:tok@GitHub.COM:8443/o/r.git",
            "github.com:8443/o/r.git",
        ),
        ("https://a%40b:c@github.com/o/r.git", "github.com/o/r.git"),
        ("ssh://git@github.com:22/o/r.git", "github.com:22/o/r.git"),
        ("ssh://git@[::1]:2222/o/r.git", "[::1]:2222/o/r.git"),
        ("https://xn--80ak6aa92e.com/r", "xn--80ak6aa92e.com/r"),
        ("https://github.com/o/r.git?x=1#frag", "github.com/o/r.git"),
        ("https://user@github.com/?q=a@evil.example", "github.com"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

/// The host libgit2 contacts for `scheme://...` (util/net.c, `git_net_url_parse`
/// and `url_parse_authority`): the authority ends at the first `/` and nowhere
/// else, the host is what follows the last `@`, and a numeric port is not part
/// of it. Lower-cased, since DNS names are not case sensitive.
fn libgit2_host(raw: &str) -> Option<String> {
    let authority = raw.split_once("://")?.1.split('/').next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = match host_port.find(']') {
        Some(end) => &host_port[..=end],
        None => host_port
            .rsplit_once(':')
            .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
            .map_or(host_port, |(host, _)| host),
    };
    Some(host.to_ascii_lowercase())
}

/// The host of a shown location, `host[:port][/path]`.
fn shown_host(location: &str) -> String {
    let end = if location.starts_with('[') {
        location.find(']').map(|index| index + 1)
    } else {
        location.find([':', '/'])
    };
    location[..end.unwrap_or(location.len())].to_ascii_lowercase()
}

#[test]
fn whatever_display_location_shows_the_host_it_shows_is_the_host_libgit2_contacts() {
    // Every combination is a user-information part glued to a host, an `@`, a
    // second host, and what can follow it. Each shown location costs a full
    // scrub, so the sets are small and chosen to differ in what matters: a host
    // name and an IPv6 literal, a special scheme and two that are not.
    let mut shown_count = 0;
    let mut refused_count = 0;
    for scheme in ["https", "ssh", "git"] {
        for first in ["github.com", "[::1]"] {
            for glue in [
                "", "?", "#", "\\", "?x=", "#y:", "\\u:", ":pw", ":", "%40", "%2F",
            ] {
                for second in ["evil.example", "[::1]"] {
                    for port in ["", ":8443"] {
                        for tail in ["", "/?q@evil.example/r"] {
                            let raw = format!("{scheme}://{first}{glue}@{second}{port}{tail}");
                            match shown(&raw) {
                                Some(location) => {
                                    shown_count += 1;
                                    assert_eq!(
                                        Some(shown_host(&location)),
                                        libgit2_host(&raw),
                                        "{raw} is shown as {location}"
                                    );
                                }
                                None => refused_count += 1,
                            }
                        }
                    }
                }
            }
        }
    }
    // Both outcomes were exercised, so the check above is not vacuous.
    assert!(shown_count > 50, "{shown_count} shown");
    assert!(refused_count > 50, "{refused_count} refused");
}

#[test]
fn display_location_shows_nothing_for_mistyped_forms_that_carry_a_credential() {
    for secret in [
        "hunter2",
        "pw",
        "p%40ss",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
    ] {
        for raw in [
            // A single slash after the scheme is not a URL; git reads it as
            // host `https` and a path.
            format!("https:/user:{secret}@github.com/o/r.git"),
            format!("git:/user:{secret}@host/r"),
            format!("ssh:/git:{secret}@host/repo"),
            format!("https:\\\\user:pa/{secret}@host\\repo"),
            // A `/` in the user information of an scp-like remote.
            format!("user:pa/{secret}@host:repo.git"),
            format!("oauth2:{secret}/def@gitlab.com:group/repo.git"),
            // A drive-letter lookalike.
            format!("C:/oauth2:{secret}@github.com/repo"),
            format!("C:\\oauth2:{secret}@github.com\\repo"),
            // A path that reads as user information.
            format!("https://github.com/x-access-token:{secret}@github.com/o/r.git"),
            format!("host:path/with{secret}@host"),
            format!("file:///tmp/x-access-token:{secret}@host/repo"),
            // A transport helper's address is a command line.
            format!("ext::sshpass -p {secret} ssh -l git host %S repo"),
            format!("ext::git-remote-foo --auth={secret} %S"),
            format!("gcrypt::rsync://user:{secret}@host/path"),
            format!("persistent-https::https://user:{secret}@host/r"),
            format!("foo::token:{secret}/host/repo"),
        ] {
            assert_eq!(shown(&raw), None, "{raw}");
        }
    }
}

#[test]
fn display_location_refuses_transport_helper_addresses_only() {
    for raw in [
        "ext::sshpass -p hunter2 ssh -l git host %S repo",
        "fd::7,8",
        "foo::bar",
        "a+b.c-d::x",
        "9ext::x",
        "EXT::x",
        "::x",
        "host::path",
    ] {
        assert_eq!(shown(raw), None, "{raw}");
    }
    // `::` elsewhere, and in an IPv6 host, is not a helper.
    for (raw, expected) in [
        ("host:a::b", "host:a::b"),
        ("[::1]:path", "[::1]:path"),
        ("git@[2001:db8::1]:o/r.git", "[2001:db8::1]:o/r.git"),
        ("https://host/a::b", "host/a::b"),
        ("/srv/a::b", "/srv/a::b"),
    ] {
        assert_eq!(shown(raw).as_deref(), Some(expected), "{raw}");
    }
}

/// Every character of the Unicode general categories Cf, Zl and Zp that the
/// previous hand-written list did not cover, and the ones it did.
fn format_and_separator_characters() -> Vec<char> {
    [
        0x00ad..=0x00ad,
        0x0600..=0x0605,
        0x061c..=0x061c,
        0x06dd..=0x06dd,
        0x070f..=0x070f,
        0x0890..=0x0891,
        0x08e2..=0x08e2,
        0x180e..=0x180e,
        0x200b..=0x200f,
        0x2028..=0x2029,
        0x202a..=0x202e,
        0x2060..=0x2064,
        0x2066..=0x206f,
        0xfeff..=0xfeff,
        0xfff9..=0xfffb,
        0x110bd..=0x110bd,
        0x110cd..=0x110cd,
        0x13430..=0x1343f,
        0x1bca0..=0x1bca3,
        0x1d173..=0x1d17a,
        0xe0001..=0xe0001,
        0xe0020..=0xe007f,
    ]
    .into_iter()
    .flatten()
    .filter_map(char::from_u32)
    .collect()
}

#[test]
fn display_location_refuses_every_control_format_and_separator_character() {
    // `char::is_control` is exactly the category Cc.
    let controls = (0..=0x10ffffu32)
        .filter_map(char::from_u32)
        .filter(|c| c.is_control());
    let others = format_and_separator_characters();
    let mut count = 0;
    for c in controls.chain(others) {
        count += 1;
        for raw in [
            // A local path, an scp-like remote, a URL in its host and in its path.
            format!("/srv/a{c}b"),
            format!("host:a{c}b"),
            format!("https://host/a{c}b"),
            format!("https://ho{c}st/b"),
            // Parts of the string that are not shown are not looked at less.
            format!("https://user{c}@host/b"),
            format!("https://host/b?q={c}z"),
            format!("https://host/b#{c}z"),
            format!("ssh://host/b\u{200d}{c}"),
        ] {
            assert_eq!(shown(&raw), None, "U+{:04X} in {raw:?}", c as u32);
        }
    }
    assert!(count > 200, "{count} characters were tried");
    // The characters the issue names, once more by name.
    for (name, c) in [
        ("Arabic letter mark", '\u{061c}'),
        ("line separator", '\u{2028}'),
        ("paragraph separator", '\u{2029}'),
        ("soft hyphen", '\u{00ad}'),
        ("Mongolian vowel separator", '\u{180e}'),
        ("interlinear annotation anchor", '\u{fff9}'),
        ("interlinear annotation terminator", '\u{fffb}'),
        ("tag latin capital letter a", '\u{e0041}'),
        ("right-to-left override", '\u{202e}'),
        ("zero width space", '\u{200b}'),
        ("escape", '\u{1b}'),
        ("next line", '\u{85}'),
    ] {
        assert_eq!(shown(&format!("host:repo{c}evil")), None, "{name}");
        assert_eq!(shown(&format!("/srv/{c}x")), None, "{name}");
    }
}

#[test]
fn display_location_keeps_text_of_every_script_that_is_not_a_control() {
    for raw in [
        "/srv/r\u{e9}pos/caf\u{e9}.git",
        "/srv/\u{65e5}\u{672c}\u{8a9e}/\u{2713}.git",
        "/srv/\u{5e9}\u{5dc}\u{5d5}\u{5dd}/repo",
        "/srv/\u{645}\u{631}\u{62d}\u{628}\u{627}/repo",
        "/srv/emoji-\u{1f642}.git",
        "host:\u{65e5}\u{672c}/repo",
        // Variation selectors and combining marks are Mn, not Cf.
        "/srv/a\u{fe0f}b",
        "/srv/e\u{301}",
    ] {
        assert_eq!(shown(raw).as_deref(), Some(raw), "{raw}");
    }
}

#[test]
fn the_identifier_rule_is_1_to_256_ascii_letters_digits_and_four_marks() {
    use cvc_core::privacy::is_safe_identifier;

    let longest = "a".repeat(256);
    for id in [
        "a",
        "A1b2C3",
        "client-session_42",
        "harness.session:7",
        "-",
        "_",
        ".",
        ":",
        "3f2b8c1e-5d4a-4b7e-9c0d-1a2b3c4d5e6f",
        longest.as_str(),
    ] {
        assert!(is_safe_identifier(id), "{id:?}");
    }

    let too_long = "a".repeat(257);
    for id in [
        "",
        too_long.as_str(),
        " ",
        "a b",
        "a\tb",
        "a\nb",
        "a/b",
        "a\\b",
        "a@b",
        "a'b",
        "\u{e9}",
        "a\u{200b}",
        "a\u{202e}b",
        "a\u{1b}[2J",
        "emoji \u{1f680}",
    ] {
        assert!(!is_safe_identifier(id), "{id:?}");
    }

    // Byte by byte: of the ASCII characters, exactly the letters, the digits and
    // `.`, `_`, `-` and `:` are allowed.
    for byte in 0u8..=127 {
        let id = format!("a{}b", byte as char);
        let allowed = byte.is_ascii_alphanumeric() || b"._-:".contains(&byte);
        assert_eq!(is_safe_identifier(&id), allowed, "byte {byte:#04x}");
    }
    // No character outside ASCII is, whatever its category.
    for c in [
        '\u{e9}',
        '\u{5e9}',
        '\u{2713}',
        '\u{ff21}',
        '\u{1f680}',
        '\u{a0}',
    ] {
        assert!(!is_safe_identifier(&format!("a{c}b")), "{c:?}");
    }
}

#[test]
fn a_remote_destination_never_prints_its_url() {
    let destination = cvc_core::privacy::RemoteDestination {
        name: "origin".into(),
        effective_url: "https://user:tok-secret@host.example/o/r.git".into(),
        fingerprint: "f".repeat(64),
        ref_name: "refs/cvc/main".into(),
    };
    let debug = format!("{destination:?}");
    assert!(
        debug.contains("origin") && debug.contains(&"f".repeat(64)),
        "{debug}"
    );
    assert!(!debug.contains("tok-secret"), "{debug}");
    assert!(!debug.contains("host.example"), "{debug}");
    // The URL is still there for the code that has to use it.
    assert!(destination.effective_url.contains("tok-secret"));
}
