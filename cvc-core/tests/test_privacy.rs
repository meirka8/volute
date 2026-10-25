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
    sync::pull_from_ref(&repo, &imported, "refs/cvc/source", &source_fingerprint).unwrap();
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
        legacy_fingerprint: Some("e".repeat(64)),
        ref_name: "refs/cvc/main".into(),
    };
    let debug = format!("{destination:?}");
    assert!(
        debug.contains("origin") && debug.contains(&"f".repeat(64)),
        "{debug}"
    );
    assert!(!debug.contains("tok-secret"), "{debug}");
    assert!(!debug.contains("host.example"), "{debug}");
    // The legacy fingerprint is a hash over the credential-bearing URL: that a
    // destination has one is said, never what it is.
    assert!(!debug.contains(&"e".repeat(64)), "{debug}");
    assert!(debug.contains("legacy_fingerprint: Some"), "{debug}");
    // The URL is still there for the code that has to use it.
    assert!(destination.effective_url.contains("tok-secret"));
}

// --- Destination fingerprints and user information ------------------------

/// The fingerprint `https://github.com/example/project.git` has, computed by
/// every version so far as hex SHA-256 of
/// `remote-url:https://github.com/example/project.git\nref:refs/cvc/main`.
/// A URL without user information keeps it, byte for byte.
const ORDINARY_FINGERPRINT: &str =
    "4a8e172ddb7c66ae2248ca8796bc8e4ca0d0af15f14d1c7c4184bfa438538f37";
/// The fingerprint `https://user:token@github.com/example/project.git` had
/// while user information was part of the hashed URL: what records made then
/// are kept under.
const LEGACY_FINGERPRINT: &str = "4ba2b26313bb6382ba4392ad125c285d0ac991385bbd371c523c3acb8fde7ccf";

/// The fingerprint formula as it was before user information was taken out:
/// over the text of the URL as it is.
fn hash_of(url: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        format!("remote-url:{url}\nref:refs/cvc/main").as_bytes(),
    ))
}

fn destination_for(url: &str) -> (tempfile::TempDir, cvc_core::privacy::RemoteDestination) {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.remote("origin", url).unwrap();
    let destination = cvc_core::privacy::remote_destination(&repo, "origin").unwrap();
    (dir, destination)
}

#[test]
fn a_remote_without_user_information_keeps_its_fingerprint() {
    let (_dir, destination) = destination_for("https://github.com/example/project.git");
    assert_eq!(destination.fingerprint, ORDINARY_FINGERPRINT);
    assert_eq!(
        hash_of("https://github.com/example/project.git"),
        ORDINARY_FINGERPRINT
    );
    assert_eq!(destination.legacy_fingerprint, None);
    // Every other form without user information is hashed as it always was,
    // and has no legacy fingerprint, so no record is ever moved for it. An
    // scp-like remote's user, and a transport-helper address, are left as they
    // are.
    for url in [
        "https://github.com/example/project.git?x=1#f",
        "ssh://github.com:2222/example/project.git",
        "git://github.com/example/project.git",
        "git@github.com:example/project.git",
        "user@host.example:~/repo",
        "/srv/repos/project.git",
        "../sibling.git",
        "file:///srv/repos/project.git",
        "C:/repos/project.git",
        "persistent-https::https://user:token@github.com/example/project.git",
        "ext::ssh -l git host %S repo",
    ] {
        let (_dir, destination) = destination_for(url);
        assert_eq!(destination.fingerprint, hash_of(url), "{url}");
        assert_eq!(destination.legacy_fingerprint, None, "{url}");
    }
}

#[test]
fn user_information_is_not_part_of_the_fingerprint() {
    let url = "https://user:token@github.com/example/project.git";
    let (_dir, destination) = destination_for(url);
    assert_eq!(destination.fingerprint, ORDINARY_FINGERPRINT);
    assert_eq!(
        destination.legacy_fingerprint.as_deref(),
        Some(LEGACY_FINGERPRINT)
    );
    assert_eq!(hash_of(url), LEGACY_FINGERPRINT);
    // Fetching and pushing still use the whole URL.
    assert_eq!(destination.effective_url, url);

    // Whatever user information the URL carries, it is the same destination.
    for url in [
        "https://other:s3cret@github.com/example/project.git",
        "https://ghp_0123456789abcdefghijklmnopqrstuvwxyz@github.com/example/project.git",
        "https://x-access-token:abc@github.com/example/project.git",
        "https://@github.com/example/project.git",
        "https://a@b@github.com/example/project.git",
    ] {
        let (_dir, destination) = destination_for(url);
        assert_eq!(destination.fingerprint, ORDINARY_FINGERPRINT, "{url}");
        assert_eq!(destination.legacy_fingerprint, Some(hash_of(url)), "{url}");
    }
    // Anything else in it still makes another destination.
    for url in [
        "https://user:token@github.com/example/other.git",
        "https://user:token@gitlab.com/example/project.git",
        "https://user:token@github.com:8443/example/project.git",
        "http://user:token@github.com/example/project.git",
        "https://user:token@GitHub.com/example/project.git",
        "https://user:token@github.com/example/project.git?x=1",
    ] {
        let (_dir, destination) = destination_for(url);
        assert_ne!(destination.fingerprint, ORDINARY_FINGERPRINT, "{url}");
    }
    // An ssh URL's user goes the same way.
    let (_dir, with_user) = destination_for("ssh://git@github.com/example/project.git");
    let (_dir, without) = destination_for("ssh://github.com/example/project.git");
    assert_eq!(with_user.fingerprint, without.fingerprint);
    assert!(with_user.legacy_fingerprint.is_some());
}

#[test]
fn url_without_userinfo_removes_exactly_the_user_information() {
    use cvc_core::privacy::url_without_userinfo;
    for (url, expected) in [
        ("https://user:token@host/o/r.git", "https://host/o/r.git"),
        ("https://token@host/o/r.git", "https://host/o/r.git"),
        ("https://@host/o/r.git", "https://host/o/r.git"),
        ("ssh://git@host:2222/o/r.git", "ssh://host:2222/o/r.git"),
        ("git+ssh://git@host/r", "git+ssh://host/r"),
        ("https://u:p@[::1]:8443/r.git", "https://[::1]:8443/r.git"),
        // libgit2 takes the host from after the last `@`.
        ("https://a@b:c@host/r", "https://host/r"),
        ("https://us%40er:p%3Ass@host/r", "https://host/r"),
        (
            "https://github.com?@evil.example/o/r.git",
            "https://evil.example/o/r.git",
        ),
        ("https://u:p\\x@host/r", "https://host/r"),
        // Everything after the authority, and the case of what is kept, stay.
        (
            "https://u:p@host/r.git?x=1#frag",
            "https://host/r.git?x=1#frag",
        ),
        (
            "HTTPS://User:Pw@Host.Example/O/R.git",
            "HTTPS://Host.Example/O/R.git",
        ),
        ("https://u@host/a@b/c", "https://host/a@b/c"),
        ("https://u@host", "https://host"),
        ("https://u@host:8443", "https://host:8443"),
    ] {
        assert_eq!(url_without_userinfo(url).unwrap(), expected, "{url}");
    }
    for url in [
        "https://host/o/r.git",
        "https://host/a@b",
        "https://host",
        "https://",
        "git@github.com:o/r.git",
        "user:secret@host:path",
        "token@github.com/o/r",
        "/srv/r.git",
        "/srv/a://user:pw@host/r",
        "ht tp://user:pw@host/r",
        "C:\\repos\\x.git",
        "file:///srv/r.git",
        "persistent-https::https://user:pw@host/r",
        "ext::sshpass -p hunter2 ssh -l git host %S repo",
    ] {
        assert_eq!(url_without_userinfo(url).unwrap(), url, "{url}");
    }
}

#[test]
fn a_url_without_its_user_information_names_the_host_libgit2_contacts() {
    use cvc_core::privacy::url_without_userinfo;
    // Each combination glues user information to a host, a port and what may
    // follow. Taking the user information out must leave the host libgit2
    // contacts for the whole URL, no `@` in the authority, nothing of the
    // secret, and the rest exactly as it was.
    let mut stripped = 0;
    for scheme in ["https", "ssh", "git", "git+ssh"] {
        for userinfo in [
            "",
            "@",
            "u@",
            "u:s3cret@",
            "a@b:s3cret@",
            "u:s%40cret@",
            "github.com?@",
            "x\\:s3cret@",
            "tok#s3cret@",
        ] {
            for host in ["github.com", "[::1]", "evil.example"] {
                for port in ["", ":8443"] {
                    for tail in ["", "/", "/o/r.git", "/o/r.git?q=@x#f@y", "/a@b"] {
                        let raw = format!("{scheme}://{userinfo}{host}{port}{tail}");
                        let location = url_without_userinfo(&raw).unwrap();
                        assert_eq!(libgit2_host(&location), libgit2_host(&raw), "{raw}");
                        let (front, rest) = location.split_once("://").unwrap();
                        assert_eq!(front, scheme, "{raw}");
                        let authority = rest.split('/').next().unwrap();
                        assert_eq!(authority, format!("{host}{port}"), "{raw}");
                        assert!(location.ends_with(tail), "{raw} -> {location}");
                        assert!(!location.contains("s3cret"), "{raw} -> {location}");
                        assert!(!location.contains("s%40cret"), "{raw} -> {location}");
                        if userinfo.is_empty() {
                            assert_eq!(location, raw);
                        } else {
                            stripped += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(stripped > 900, "{stripped} stripped");
}

#[test]
fn a_url_whose_host_cannot_be_located_is_refused_without_repeating_it() {
    use cvc_core::privacy::{url_without_userinfo, PrivacyError};
    for url in [
        "://user:s3cret@host/r",
        "://host/r",
        "https://user:s3cret@/r",
        "https://user:s3cret@",
        "https://user@:443/r",
        "ssh://git:s3cret@:22/r",
    ] {
        let error = url_without_userinfo(url).unwrap_err();
        assert!(matches!(error, PrivacyError::Remote(_)), "{url}: {error:?}");
        assert_eq!(
            error.to_string(),
            "remote push URL has no host that can be located"
        );
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.config()
            .unwrap()
            .set_str("remote.origin.url", url)
            .unwrap();
        let resolved = cvc_core::privacy::remote_destination(&repo, "origin").unwrap_err();
        for text in [
            error.to_string(),
            resolved.to_string(),
            format!("{resolved:?}"),
        ] {
            for part in ["s3cret", "user", "git:", "://", "443", "/r"] {
                assert!(!text.contains(part), "{url}: {text}");
            }
        }
    }
}

#[test]
fn no_privacy_error_about_a_remote_repeats_its_url() {
    // Every refusal on the way to a destination has a fixed text.
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("remote.blank.url", "  ").unwrap();
    config
        .set_str("remote.nohost.url", "https://deploy:s3cret@/o/r.git")
        .unwrap();
    for (name, expected) in [
        ("typo", "remote is not configured"),
        ("blank", "remote has an empty push URL"),
        ("nohost", "remote push URL has no host that can be located"),
    ] {
        let error = cvc_core::privacy::remote_destination(&repo, name).unwrap_err();
        assert!(
            matches!(error, cvc_core::privacy::PrivacyError::Remote(_)),
            "{error:?}"
        );
        assert_eq!(error.to_string(), expected);
        assert!(!error.to_string().contains(".thoughtignore"));
    }
    let error = cvc_core::privacy::destination_operation_lock(&repo, "https://u:s3cret@h/r")
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "invalid destination fingerprint");
}

// --- Moving records from the legacy fingerprint ---------------------------

/// Every column of the local database that holds a destination fingerprint.
const DESTINATION_COLUMNS: [(&str, &str); 10] = [
    ("conversation_shares", "remote_fingerprint"),
    ("interaction_shares", "remote_fingerprint"),
    ("publications", "remote_fingerprint"),
    ("tombstones", "remote_fingerprint"),
    ("derivation_authorizations", "remote_fingerprint"),
    ("range_authorizations", "remote_fingerprint"),
    ("derivation_observations", "source_fingerprint"),
    ("derivation_observations", "source_key"),
    ("range_observations", "source_key"),
    ("range_observations", "source_remote"),
];

/// How many rows hold `fingerprint`, per column of [`DESTINATION_COLUMNS`].
fn rows_under(db: &std::path::Path, fingerprint: &str) -> Vec<i64> {
    let connection = rusqlite::Connection::open(db).unwrap();
    DESTINATION_COLUMNS
        .iter()
        .map(|(table, column)| {
            connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                    [fingerprint],
                    |row| row.get(0),
                )
                .unwrap()
        })
        .collect()
}

#[test]
fn every_column_that_names_a_destination_is_one_records_are_moved_from() {
    // A new table or column that records a destination must be added to the
    // move, or records kept in it stay under the legacy fingerprint.
    let dir = tempdir().unwrap();
    let path = dir.path().join("index.db");
    let _store = CvcStore::open(&path).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut found = Vec::new();
    for table in tables {
        let columns: Vec<String> = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for column in columns {
            if column.contains("fingerprint") || column == "source_remote" {
                found.push((table.clone(), column));
            }
        }
    }
    for (table, column) in &found {
        assert!(
            DESTINATION_COLUMNS
                .iter()
                .any(|(t, c)| t == table && c == column),
            "{table}.{column} names a destination but is not moved"
        );
    }
    assert_eq!(found.len(), 8, "{found:?}");
}

/// A repository whose `origin` pushes to a URL with user information, holding
/// what an earlier version recorded for it under the legacy fingerprint:
/// sharing consent with auto-push on, a conversation shared with its future
/// turns, one turn published and one of unknown state, a remote tombstone,
/// derivation and range authorizations, and what a pull observed.
struct Legacy {
    repo: Repository,
    db: std::path::PathBuf,
    store: CvcStore,
    destination: cvc_core::privacy::RemoteDestination,
    legacy: String,
    turns: Vec<InteractionId>,
    /// Last, so the directory goes after what is open in it.
    _dir: tempfile::TempDir,
}

fn legacy_records() -> Legacy {
    use cvc_core::models::{PublicationState, Tombstone, TombstoneReasonCode};
    use cvc_core::privacy::{
        acknowledge_sharing_destination, remote_destination, set_auto_push_destination,
        RemoteDestination,
    };
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.remote("origin", "https://github.com/example/project.git")
        .unwrap();
    repo.remote_set_pushurl(
        "origin",
        Some("https://user:token@github.com/example/project.git"),
    )
    .unwrap();
    let destination = remote_destination(&repo, "origin").unwrap();
    assert_eq!(destination.fingerprint, ORDINARY_FINGERPRINT);
    let legacy = destination.legacy_fingerprint.clone().unwrap();
    assert_eq!(legacy, LEGACY_FINGERPRINT);

    // What the earlier version recorded, under the fingerprint it computed.
    let earlier = RemoteDestination {
        fingerprint: legacy.clone(),
        legacy_fingerprint: None,
        ..destination.clone()
    };
    acknowledge_sharing_destination(&repo, &earlier).unwrap();
    set_auto_push_destination(&repo, &earlier, true).unwrap();
    let db = repo.path().join("cvc").join("index.db");
    let store = CvcStore::open(&db).unwrap();
    let turns: Vec<InteractionId> = (0..2)
        .map(|_| {
            let id = InteractionId::new();
            store
                .capture_mcp(McpCapture::new(
                    Conversation {
                        id: "shared-before".into(),
                        title: "shared before".into(),
                        created_at: Utc::now(),
                    },
                    interaction(id.clone(), "shared-before"),
                    Vec::new(),
                    Vec::new(),
                    PreparedPolicy::built_ins_only(),
                    "0".repeat(64),
                ))
                .unwrap();
            id
        })
        .collect();
    store
        .share_conversation_for_remote("shared-before", &legacy, FutureSharePolicy::Shared)
        .unwrap();
    store
        .mark_publication(&turns[..1], &legacy, PublicationState::Published)
        .unwrap();
    store
        .mark_publication(&turns[1..], &legacy, PublicationState::Unknown)
        .unwrap();
    store
        .apply_tombstones(
            &[Tombstone::new(
                InteractionId::new(),
                TombstoneReasonCode::UserRequested,
                None,
            )],
            "fixture",
            Some(&legacy),
        )
        .unwrap();
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch(&format!(
            "INSERT INTO derivation_authorizations(event_id, remote_fingerprint, source_event_id) VALUES('event-1', '{legacy}', 'event-1');
             INSERT INTO range_authorizations(range_id, remote_fingerprint) VALUES('range-1', '{legacy}');
             INSERT INTO derivation_observations(event_id, source_fingerprint, source_key, origin, trusted_local) VALUES('event-1', '{legacy}', '{legacy}', 'remote_sync', 0);
             INSERT INTO range_observations(range_id, source_key, origin, trusted_local, source_ref, source_remote) VALUES('range-1', '{legacy}', 'remote_sync', 0, NULL, '{legacy}');"
        ))
        .unwrap();
    Legacy {
        repo,
        db,
        store,
        destination,
        legacy,
        turns,
        _dir: dir,
    }
}

fn privacy_json(repo: &Repository) -> String {
    std::fs::read_to_string(repo.path().join("cvc").join("privacy.json")).unwrap()
}

#[test]
fn records_kept_under_the_legacy_fingerprint_move_to_the_fingerprint_once() {
    use cvc_core::privacy::privacy_status_for_fingerprint;
    let legacy = legacy_records();
    let fingerprint = legacy.destination.fingerprint.clone();
    let kept = rows_under(&legacy.db, &legacy.legacy);
    assert!(kept.iter().all(|&count| count > 0), "{kept:?}");
    assert!(rows_under(&legacy.db, &fingerprint)
        .iter()
        .all(|&count| count == 0));
    let before = privacy_status_for_fingerprint(&legacy.repo, &fingerprint).unwrap();
    assert!(!before.sharing_consented && !before.auto_push);

    sync::adopt_legacy_records(&legacy.repo, &legacy.store, &legacy.destination).unwrap();

    // Consent, with its auto-push setting, reads as it did.
    let after = privacy_status_for_fingerprint(&legacy.repo, &fingerprint).unwrap();
    assert!(after.sharing_consented && after.auto_push);
    let gone = privacy_status_for_fingerprint(&legacy.repo, &legacy.legacy).unwrap();
    assert!(!gone.sharing_consented && !gone.auto_push);
    let state = privacy_json(&legacy.repo);
    assert!(!state.contains(&legacy.legacy), "{state}");
    // Every row moved, and nothing is left under the legacy fingerprint.
    assert_eq!(rows_under(&legacy.db, &fingerprint), kept);
    assert!(rows_under(&legacy.db, &legacy.legacy)
        .iter()
        .all(|&count| count == 0));
    // So shares and publications read as they did.
    let summary = legacy
        .store
        .list_conversation_summaries(Some(&fingerprint), 10)
        .unwrap()
        .into_iter()
        .find(|summary| summary.id == "shared-before")
        .unwrap();
    assert!(summary.shared);
    assert_eq!(summary.share_future, Some(true));
    assert_eq!((summary.published, summary.unconfirmed), (1, 1));
    assert_eq!(
        legacy
            .store
            .uncertain_publication_ids(&fingerprint)
            .unwrap(),
        vec![legacy.turns[1].clone()]
    );
    assert_eq!(
        legacy
            .store
            .tombstones_for_projection(&fingerprint)
            .unwrap()
            .len(),
        1
    );

    // Once only: a second run changes nothing.
    let rows = rows_under(&legacy.db, &fingerprint);
    sync::adopt_legacy_records(&legacy.repo, &legacy.store, &legacy.destination).unwrap();
    assert_eq!(rows_under(&legacy.db, &fingerprint), rows);
    assert_eq!(privacy_json(&legacy.repo), state);
}

#[test]
fn records_already_kept_under_the_fingerprint_win_and_nothing_is_merged() {
    use cvc_core::models::PublicationState;
    use cvc_core::privacy::{acknowledge_sharing_destination, privacy_status_for_fingerprint};
    let legacy = legacy_records();
    let fingerprint = legacy.destination.fingerprint.clone();
    // Both fingerprints hold records: consent without auto-push and one
    // publication are kept under the fingerprint already.
    acknowledge_sharing_destination(&legacy.repo, &legacy.destination).unwrap();
    legacy
        .store
        .mark_publication(&legacy.turns[..1], &fingerprint, PublicationState::Pending)
        .unwrap();
    let kept = rows_under(&legacy.db, &legacy.legacy);
    let current = rows_under(&legacy.db, &fingerprint);
    let state = privacy_json(&legacy.repo);

    sync::adopt_legacy_records(&legacy.repo, &legacy.store, &legacy.destination).unwrap();

    assert_eq!(rows_under(&legacy.db, &legacy.legacy), kept);
    assert_eq!(rows_under(&legacy.db, &fingerprint), current);
    assert_eq!(privacy_json(&legacy.repo), state);
    let status = privacy_status_for_fingerprint(&legacy.repo, &fingerprint).unwrap();
    assert!(status.sharing_consented && !status.auto_push);
}

#[test]
fn a_store_left_behind_by_an_interrupted_move_is_moved_by_the_next_run() {
    use cvc_core::privacy::privacy_status_for_fingerprint;
    // The database moved, privacy.json did not.
    let legacy = legacy_records();
    let fingerprint = legacy.destination.fingerprint.clone();
    let kept = rows_under(&legacy.db, &legacy.legacy);
    let connection = rusqlite::Connection::open(&legacy.db).unwrap();
    for (table, column) in DESTINATION_COLUMNS {
        connection
            .execute(
                &format!("UPDATE {table} SET {column} = ?2 WHERE {column} = ?1"),
                [&legacy.legacy, &fingerprint],
            )
            .unwrap();
    }
    sync::adopt_legacy_records(&legacy.repo, &legacy.store, &legacy.destination).unwrap();
    let status = privacy_status_for_fingerprint(&legacy.repo, &fingerprint).unwrap();
    assert!(status.sharing_consented && status.auto_push);
    assert_eq!(rows_under(&legacy.db, &fingerprint), kept);

    // privacy.json moved, the database did not.
    let legacy = legacy_records();
    let fingerprint = legacy.destination.fingerprint.clone();
    let kept = rows_under(&legacy.db, &legacy.legacy);
    let path = legacy.repo.path().join("cvc").join("privacy.json");
    let moved = privacy_json(&legacy.repo).replace(&legacy.legacy, &fingerprint);
    std::fs::write(&path, moved).unwrap();
    sync::adopt_legacy_records(&legacy.repo, &legacy.store, &legacy.destination).unwrap();
    assert_eq!(rows_under(&legacy.db, &fingerprint), kept);
    assert!(rows_under(&legacy.db, &legacy.legacy)
        .iter()
        .all(|&count| count == 0));
    let status = privacy_status_for_fingerprint(&legacy.repo, &fingerprint).unwrap();
    assert!(status.sharing_consented && status.auto_push);
}

#[test]
fn a_destination_without_user_information_takes_no_migration_path() {
    let legacy = legacy_records();
    // The same repository's fetch URL has no user information.
    legacy.repo.remote_set_pushurl("origin", None).unwrap();
    let plain = cvc_core::privacy::remote_destination(&legacy.repo, "origin").unwrap();
    assert_eq!(plain.fingerprint, ORDINARY_FINGERPRINT);
    assert_eq!(plain.legacy_fingerprint, None);
    let kept = rows_under(&legacy.db, &legacy.legacy);
    let state = privacy_json(&legacy.repo);
    let entries = |repo: &Repository| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(repo.path().join("cvc"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let files = entries(&legacy.repo);

    sync::adopt_legacy_records(&legacy.repo, &legacy.store, &plain).unwrap();

    // Nothing was read into a move, locked or written.
    assert_eq!(rows_under(&legacy.db, &legacy.legacy), kept);
    assert!(rows_under(&legacy.db, ORDINARY_FINGERPRINT)
        .iter()
        .all(|&count| count == 0));
    assert_eq!(privacy_json(&legacy.repo), state);
    assert_eq!(entries(&legacy.repo), files);
    assert!(
        !files.iter().any(|name| name.starts_with("publish-")),
        "{files:?}"
    );
}

#[test]
fn a_destination_whose_fingerprints_are_not_its_urls_is_refused() {
    let legacy = legacy_records();
    let kept = rows_under(&legacy.db, &legacy.legacy);
    let state = privacy_json(&legacy.repo);
    let bogus = "0".repeat(64);
    for destination in [
        // A legacy fingerprint that is not the one of the URL.
        cvc_core::privacy::RemoteDestination {
            legacy_fingerprint: Some(bogus.clone()),
            ..legacy.destination.clone()
        },
        // A fingerprint that is not the one of the URL.
        cvc_core::privacy::RemoteDestination {
            fingerprint: bogus.clone(),
            ..legacy.destination.clone()
        },
        // A URL without user information has no legacy fingerprint.
        cvc_core::privacy::RemoteDestination {
            effective_url: "https://github.com/example/project.git".into(),
            ..legacy.destination.clone()
        },
    ] {
        let error = sync::adopt_legacy_records(&legacy.repo, &legacy.store, &destination)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "destination fingerprints do not match its push URL");
    }
    assert_eq!(rows_under(&legacy.db, &legacy.legacy), kept);
    assert_eq!(privacy_json(&legacy.repo), state);
}
