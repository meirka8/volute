//! Public issue 116: the Claude Code plugin (`plugins/claude-code`) and
//! `cvc harness install claude-code` run the same ingest command from the same
//! events. These tests keep the plugin's files in lockstep with the constants
//! the harness installer writes from, and check the one hook that is not
//! ingestion: the SessionStart check script, which is also run here against the
//! real `cvc` and against stand-ins for a missing and an old one.
//!
//! Behaviour of the command itself, including which sessions it records and how
//! runs on one session take turns, is in `claude_code_cli.rs`.
use cvc_core::claude_code::settings::{HOOK_EVENTS, HOOK_SIGNATURE, HOOK_TIMEOUT_SECS};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// What both the plugin and the checkout's entries run.
const INGEST_COMMAND: &str = "cvc ingest claude-code --hook";

fn plugin_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("plugins")
        .join("claude-code")
        .join(relative)
}

fn read_json(relative: &str) -> Value {
    let path = plugin_file(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not JSON: {error}", path.display()))
}

/// The hook events of `hooks/hooks.json`, each with its matcher groups.
fn hook_events() -> Vec<(String, Vec<Value>)> {
    let document = read_json("hooks/hooks.json");
    document["hooks"]
        .as_object()
        .expect("hooks.json has a `hooks` object")
        .iter()
        .map(|(event, groups)| {
            let groups = groups
                .as_array()
                .unwrap_or_else(|| panic!("{event} is not a list of groups"))
                .clone();
            (event.clone(), groups)
        })
        .collect()
}

fn commands_of(group: &Value) -> Vec<&str> {
    group["hooks"]
        .as_array()
        .expect("a group has a `hooks` list")
        .iter()
        .map(|hook| hook["command"].as_str().expect("a hook has a command"))
        .collect()
}

#[test]
fn the_plugin_ingests_on_the_events_and_with_the_command_harness_install_writes() {
    let events = hook_events();

    let ingesting: BTreeSet<&str> = events
        .iter()
        .filter(|(_, groups)| {
            groups
                .iter()
                .flat_map(commands_of)
                .any(|command| command.contains(HOOK_SIGNATURE))
        })
        .map(|(event, _)| event.as_str())
        .collect();
    assert_eq!(
        ingesting,
        HOOK_EVENTS.into_iter().collect::<BTreeSet<_>>(),
        "the plugin ingests on the events `cvc harness install claude-code` does"
    );

    for event in HOOK_EVENTS {
        let (_, groups) = events
            .iter()
            .find(|(name, _)| name == event)
            .unwrap_or_else(|| panic!("hooks.json has no {event} hook"));
        assert_eq!(groups.len(), 1, "{event}: one group");
        let group = &groups[0];
        // No matcher: every tool, every source, as the checkout's entries.
        assert!(
            group.get("matcher").is_none(),
            "{event}: the ingest group has no matcher"
        );
        let hooks = group["hooks"].as_array().expect("a `hooks` list");
        assert_eq!(hooks.len(), 1, "{event}: one hook");
        let hook = &hooks[0];
        assert_eq!(hook["type"], "command", "{event}");
        assert_eq!(hook["command"], INGEST_COMMAND, "{event}");
        assert!(
            hook["command"].as_str().unwrap().contains(HOOK_SIGNATURE),
            "{event}: the command carries the signature `harness install` recognizes entries by"
        );
        assert_eq!(
            hook["timeout"].as_u64(),
            Some(HOOK_TIMEOUT_SECS),
            "{event}: the timeout `harness install` sets"
        );
    }
}

#[test]
fn the_plugin_has_no_hooks_beyond_ingestion_and_the_session_start_check() {
    let names: BTreeSet<String> = hook_events().into_iter().map(|(event, _)| event).collect();
    let expected: BTreeSet<String> = HOOK_EVENTS
        .into_iter()
        .chain(["SessionStart"])
        .map(str::to_owned)
        .collect();
    assert_eq!(names, expected);
}

/// The SessionStart hook's command, as `hooks.json` must spell it. Claude Code
/// substitutes `${CLAUDE_PLUGIN_ROOT}` before the shell runs it and also
/// exports it, so the shell finds the same directory either way.
const SESSION_START_COMMAND: &str = r#"sh "${CLAUDE_PLUGIN_ROOT}/scripts/check-cvc.sh""#;

fn session_start_groups() -> Vec<Value> {
    hook_events()
        .into_iter()
        .find(|(event, _)| event == "SessionStart")
        .expect("hooks.json has a SessionStart hook")
        .1
}

#[test]
fn the_session_start_hook_runs_the_check_script_when_a_session_starts_or_resumes() {
    let groups = session_start_groups();
    assert_eq!(groups.len(), 1, "one group");
    let group = &groups[0];
    // A matcher, so the check runs for new and resumed sessions and not at
    // every restart of one that was already checked.
    assert_eq!(group["matcher"], "startup|resume");
    let hooks = group["hooks"].as_array().expect("a `hooks` list");
    assert_eq!(hooks.len(), 1, "one hook");
    assert_eq!(hooks[0]["type"], "command");
    assert_eq!(hooks[0]["command"], SESSION_START_COMMAND);
    // Room for an npm-launcher `cvc` to download its binary on the first run.
    assert_eq!(hooks[0]["timeout"].as_u64(), Some(30));
    assert!(
        !SESSION_START_COMMAND.contains("ingest"),
        "SessionStart does not ingest"
    );
    assert!(
        plugin_file("scripts/check-cvc.sh").is_file(),
        "the script the hook runs is in the plugin"
    );
}

/// The check tells a cvc that has the silent no-op from one that lacks it by
/// sending hook mode a payload with a working directory that does not exist,
/// and a project directory that does not exist either. The session's own
/// `CLAUDE_PROJECT_DIR` is exported to a SessionStart hook too and may be an
/// initialized repository, where the payload's missing session is an error to
/// report, so the script overrides it for the probe.
/// `claude_code_cli.rs` pins what hook mode does with exactly these.
#[test]
fn the_check_script_probes_with_the_payload_hook_mode_is_tested_with() {
    let script = std::fs::read_to_string(plugin_file("scripts/check-cvc.sh")).unwrap();
    assert!(
        script.contains(
            r#"elif ! printf '%s' '{"cwd":"/nonexistent/cvc-plugin-probe"}' | CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe cvc ingest claude-code --hook >/dev/null 2>&1; then"#
        ),
        "{script}"
    );
}

/// The consent message tells a user how to stop the plugin's hooks with
/// `/plugin disable cvc@volute`: the plugin's name from its manifest and the
/// marketplace's name, as Claude Code writes a plugin's id.
#[test]
fn the_plugin_is_cvc_at_volute() {
    let manifest = read_json(".claude-plugin/plugin.json");
    let marketplace: Value = {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(".claude-plugin")
            .join("marketplace.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        serde_json::from_str(&text).unwrap()
    };
    assert_eq!(manifest["name"], "cvc");
    assert_eq!(marketplace["name"], "volute");
    let listed: Vec<&Value> = marketplace["plugins"]
        .as_array()
        .expect("the marketplace lists plugins")
        .iter()
        .map(|plugin| &plugin["name"])
        .collect();
    assert_eq!(listed, [&Value::from("cvc")]);
}

/// What `/plugin` shows of the hooks must not promise more than hook mode does:
/// it records a session only in a repository that is CVC-initialized with
/// capture acknowledged, and where the session was started.
#[test]
fn the_hooks_description_says_what_they_record_and_what_they_need() {
    let document = read_json("hooks/hooks.json");
    assert_eq!(
        document["description"],
        "CVC: records Claude Code sessions started in a repository where CVC is initialized and capture is acknowledged; elsewhere the hooks record nothing. Requires cvc 0.7.1 or later on PATH."
    );
}

#[cfg(unix)]
mod check_script {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;
    use std::process::{Command, Output, Stdio};
    use std::time::SystemTime;
    use tempfile::TempDir;

    /// The link the SessionStart command's messages carry.
    const INSTALL_LINK: &str = "https://github.com/meirka8/volute#install-a-release";

    fn not_on_the_path() -> String {
        format!("CVC: the cvc command is not on the PATH Claude Code runs hooks with, so the cvc plugin cannot capture this session. Install CVC ({INSTALL_LINK}) or add its directory (by default ~/.cvc/bin) to that PATH, then start a new session.")
    }

    fn too_old() -> String {
        format!("CVC: the cvc on PATH is older than the cvc plugin needs (0.7.1 or later), so its hooks report errors in projects without CVC. Upgrade CVC ({INSTALL_LINK}), then start a new session.")
    }

    /// The plugin as this repository ships it, which is what Claude Code
    /// installs and points `CLAUDE_PLUGIN_ROOT` at.
    fn plugin_root() -> PathBuf {
        fs::canonicalize(plugin_file("")).unwrap()
    }

    /// A directory to be the whole PATH: it holds `sh`, which the hook's
    /// command starts with, and whatever the case puts in it.
    fn path_with_sh() -> TempDir {
        let path = TempDir::new().unwrap();
        symlink("/bin/sh", path.path().join("sh")).unwrap();
        path
    }

    /// Runs the SessionStart command from `hooks.json` as Claude Code does:
    /// through a shell, in `cwd` (where the session started, too), with
    /// `CLAUDE_PLUGIN_ROOT` set and a PATH of just `path`. Nothing else of this
    /// process's environment is inherited.
    fn run_check(path: &Path, cwd: &Path, home: &Path) -> Output {
        let command = session_start_groups()[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_owned();
        Command::new("/bin/sh")
            .args(["-c", &command])
            .current_dir(cwd)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("CLAUDE_PLUGIN_ROOT", plugin_root())
            // The directory the session was started in, which Claude Code
            // exports to every hook.
            .env("CLAUDE_PROJECT_DIR", cwd)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// Exit 0, nothing on stderr, and on stdout the one JSON object that
    /// Claude Code shows the user, carrying `expected` and nothing else.
    fn assert_shows(output: &Output, expected: &str) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(stderr.is_empty(), "{stderr}");
        assert_eq!(stdout.lines().count(), 1, "{stdout}");
        let shown: Value =
            serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{error}: {stdout}"));
        assert_eq!(shown, serde_json::json!({ "systemMessage": expected }));
    }

    #[test]
    fn it_says_so_when_cvc_is_not_on_the_path() {
        let path = path_with_sh();
        let elsewhere = TempDir::new().unwrap();
        let output = run_check(path.path(), elsewhere.path(), elsewhere.path());
        assert_shows(&output, &not_on_the_path());
    }

    #[test]
    fn it_says_so_when_the_cvc_on_the_path_predates_the_silent_no_op() {
        let path = path_with_sh();
        // What cvc 0.7.0 does with the probe: no session in the payload.
        let stub = path.path().join("cvc");
        fs::write(
            &stub,
            "#!/bin/sh\necho 'hook payload has no session_id' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let elsewhere = TempDir::new().unwrap();
        let output = run_check(path.path(), elsewhere.path(), elsewhere.path());
        assert_shows(&output, &too_old());
    }

    /// A command with the isolation the CLI tests use: no inherited `GIT_*`
    /// or Claude Code variables, and a home and Git configuration of its own.
    fn isolated(program: impl AsRef<OsStr>, home: &Path, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", home.join("templates"))
            .stdin(Stdio::null());
        command
    }

    /// `home` and an initialized repository with capture acknowledged, the
    /// state in which hook mode would ingest whatever it is sent.
    fn initialized_acknowledged_repository(root: &Path) -> (PathBuf, PathBuf) {
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(home.join("templates")).unwrap();
        fs::create_dir(&repo).unwrap();
        let run = |program: &str, args: &[&str]| {
            let output = isolated(program, &home, &repo).args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{program} {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run("git", &["init"]);
        run("git", &["config", "user.email", "test@example.invalid"]);
        run("git", &["config", "user.name", "CVC test"]);
        fs::write(repo.join("tracked"), "initial\n").unwrap();
        run("git", &["add", "tracked"]);
        run("git", &["commit", "-m", "initial"]);
        run(env!("CARGO_BIN_EXE_cvc"), &["init"]);
        let repository = git2::Repository::open(&repo).unwrap();
        cvc_core::privacy::acknowledge_capture(&repository).unwrap();
        (home, repo)
    }

    /// Every path under `root`, with its size and modification time: equal
    /// before and after means nothing was created, removed or written.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, Option<SystemTime>)> {
        fn walk(directory: &Path, found: &mut BTreeMap<PathBuf, (u64, Option<SystemTime>)>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                found.insert(path.clone(), (metadata.len(), metadata.modified().ok()));
                if metadata.is_dir() {
                    walk(&path, found);
                }
            }
        }
        let mut found = BTreeMap::new();
        walk(root, &mut found);
        found
    }

    /// The check against the `cvc` this build produces, run where hook mode
    /// would ingest if it were sent a real session, in a session whose project
    /// directory is that repository (`run_check` exports it, as Claude Code
    /// does): it must say nothing, and leave no trace. This is what ties the
    /// probe to the binary, and shows the script's override of the project
    /// directory working: without it, hook mode would report the probe's missing
    /// session in this repository.
    #[test]
    fn it_is_silent_and_leaves_no_trace_with_this_builds_cvc_inside_a_capturing_repository() {
        let temp = TempDir::new().unwrap();
        let (home, repo) = initialized_acknowledged_repository(temp.path());
        let path = path_with_sh();
        symlink(env!("CARGO_BIN_EXE_cvc"), path.path().join("cvc")).unwrap();
        let before = snapshot(temp.path());

        let output = run_check(path.path(), &repo, &home);
        assert_eq!(output.status.code(), Some(0));
        assert!(
            output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            snapshot(temp.path()),
            before,
            "nothing was created or written"
        );
        let connection = rusqlite::Connection::open(repo.join(".git/cvc/index.db")).unwrap();
        let interactions: i64 = connection
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(interactions, 0);
    }
}
