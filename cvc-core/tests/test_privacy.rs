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
