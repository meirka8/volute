//! Public issue 116: the Claude Code plugin (`plugins/claude-code`) and
//! `cvc harness install claude-code` run the same ingest command from the same
//! events. These tests keep the plugin's files in lockstep with the constants
//! the harness installer writes from, and run the plugin's two scripts: the
//! launcher its hooks run `cvc` through (`run-cvc.sh`, which finds `cvc` only
//! in absolute PATH directories, so that a project cannot supply the `cvc` the
//! plugin's hooks run) and the SessionStart check, both against the real `cvc`
//! and against stand-ins for a missing, an old and a project-supplied one.
//!
//! Behaviour of the command itself, including which sessions it records and how
//! runs on one session take turns, is in `claude_code_cli.rs`.
use cvc_core::claude_code::settings::{HOOK_EVENTS, HOOK_SIGNATURE, HOOK_TIMEOUT_SECS};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// What the plugin's ingest hooks run: the launcher, through the absolute
/// `/bin/sh` (a bare `sh` is looked up in PATH, where a project in the working
/// directory could supply one), which finds `cvc` in an absolute PATH directory
/// and runs it with these arguments (the checkout's entries run the same
/// arguments through the absolute binary path they pin). Exit status 2 is the
/// one Claude Code takes as "block", and older dash and busybox shells exit
/// with it when they cannot open the script (a plugin cache replaced mid-
/// session, a broken install), so it is mapped to 1; `cvc` itself never exits
/// 2 in hook mode. Claude Code substitutes `${CLAUDE_PLUGIN_ROOT}` before the
/// shell runs it and also exports it.
const INGEST_COMMAND: &str = r#"/bin/sh "${CLAUDE_PLUGIN_ROOT}/scripts/run-cvc.sh" ingest claude-code --hook; s=$?; [ "$s" -ne 2 ] || s=1; exit "$s""#;

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
fn the_launcher_the_ingest_hooks_run_is_in_the_plugin() {
    assert!(
        plugin_file("scripts/run-cvc.sh").is_file(),
        "the script the ingest hooks run is in the plugin"
    );
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
const SESSION_START_COMMAND: &str = r#"/bin/sh "${CLAUDE_PLUGIN_ROOT}/scripts/check-cvc.sh""#;

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
/// report, so the script overrides it for the probe. It probes the `cvc` that
/// the launcher locates, so that one rule picks the `cvc`.
/// `claude_code_cli.rs` pins what hook mode does with exactly these.
#[test]
fn the_check_script_probes_with_the_payload_hook_mode_is_tested_with() {
    let script = std::fs::read_to_string(plugin_file("scripts/check-cvc.sh")).unwrap();
    assert!(
        script.contains(
            r#"printf '%s' '{"cwd":"/nonexistent/cvc-plugin-probe"}' | CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe "$cvc" ingest claude-code --hook >/dev/null 2>&1"#
        ),
        "{script}"
    );
    assert!(
        script
            .contains(r#"cvc=$(CVC_PLUGIN_LOCATE_ONLY=1 /bin/sh "$here/run-cvc.sh" 2>/dev/null)"#),
        "{script}"
    );
    // Never the working directory, which is the project's.
    assert!(
        script.contains("case $0 in\n  */*) here=${0%/*} ;;\n  *) exit 0 ;;\nesac"),
        "{script}"
    );
}

/// The consent message tells a user how to stop the plugin's hooks with
/// `/plugin disable cvc@volute`: the plugin's name from its manifest and the
/// marketplace's name, as Claude Code writes a plugin's id. The marketplace
/// installs the plugin from this repository's release tag, which is the
/// crate's version, so the tag cannot drift from the release.
#[test]
fn the_plugin_is_cvc_at_volute_and_is_installed_from_the_release_tag() {
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
    let plugins = marketplace["plugins"]
        .as_array()
        .expect("the marketplace lists plugins");
    assert_eq!(plugins.len(), 1, "one plugin");
    let entry = &plugins[0];
    assert_eq!(entry["name"], "cvc");
    let source = &entry["source"];
    assert_eq!(source["source"], "git-subdir", "{source}");
    assert_eq!(source["path"], "plugins/claude-code", "{source}");
    assert_eq!(
        source["ref"],
        format!("v{}", env!("CARGO_PKG_VERSION")),
        "{source}"
    );
}

/// What `/plugin` shows of the hooks must not promise more than hook mode does:
/// it records a session only in a repository that is CVC-initialized with
/// capture acknowledged, and where the session was started.
#[test]
fn the_hooks_description_says_what_they_record_and_what_they_need() {
    let document = read_json("hooks/hooks.json");
    assert_eq!(
        document["description"],
        "CVC: records Claude Code sessions started in a repository where CVC is initialized and capture is acknowledged; elsewhere the hooks record nothing. Requires cvc 0.7.1 or later in an absolute directory on PATH."
    );
}

#[cfg(unix)]
mod scripts {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;
    use std::process::{Command, Output, Stdio};
    use std::time::SystemTime;
    use tempfile::TempDir;

    const BASIC: &str =
        include_str!("../../cvc-core/tests/fixtures/claude-code/v2.1.260/basic.jsonl");
    const SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";

    /// The link the SessionStart command's messages carry.
    const INSTALL_LINK: &str = "https://github.com/meirka8/volute#install-a-release";

    /// What the launcher says, on stderr, when it finds no `cvc`.
    const NOT_FOUND: &str = "cvc: command not found in an absolute PATH directory (the cvc plugin does not use relative PATH entries)";

    fn not_on_the_path() -> String {
        format!("CVC: the cvc command is not on the PATH Claude Code runs hooks with, so the cvc plugin cannot capture this session. Install CVC ({INSTALL_LINK}) or add its directory (by default ~/.cvc/bin) to that PATH, then start a new session.")
    }

    fn too_old() -> String {
        format!("CVC: the cvc on PATH is older than the cvc plugin needs (0.7.1 or later), so its hooks report errors in projects without CVC. Upgrade CVC ({INSTALL_LINK}), then start a new session.")
    }

    fn only_in_a_relative_entry() -> String {
        "CVC: the only cvc on the PATH Claude Code runs hooks with is in a relative PATH entry, which the cvc plugin does not use, so it cannot capture this session. Put cvc in an absolute directory on that PATH (by default ~/.cvc/bin), then start a new session.".to_owned()
    }

    fn could_not_be_run() -> String {
        "CVC: the cvc found on the PATH Claude Code runs hooks with could not be run (for example an npm-installed cvc whose node is not on that PATH), so the cvc plugin cannot capture this session. Check that cvc --version works with that PATH, then start a new session.".to_owned()
    }

    /// The plugin as this repository ships it, which is what Claude Code
    /// installs and points `CLAUDE_PLUGIN_ROOT` at.
    fn plugin_root() -> PathBuf {
        fs::canonicalize(plugin_file("")).unwrap()
    }

    /// An existing absolute directory with nothing in it, to be (all of) a PATH.
    /// The hooks' commands name `/bin/sh` and the scripts use only shell
    /// builtins, so a PATH needs to hold nothing but the `cvc` a case wants.
    fn empty_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    fn executable(file: &Path, contents: &str) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, contents).unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A program a project could supply (a `cvc`, a `sh`): executable, and
    /// leaving `marker` if it ever runs. It creates the marker with a shell
    /// redirection, not a command, so that it does not depend on the PATH of
    /// the run it is in: a run with a PATH that holds no `touch` still leaves
    /// the marker.
    fn project_supplied(file: &Path, marker: &Path) {
        executable(
            file,
            &format!("#!/bin/sh\n: > '{}'\nexit 0\n", marker.display()),
        );
    }

    /// Runs a hook command from `hooks.json` as Claude Code does: through
    /// `/bin/sh -c`, in `cwd` (where the session started, too), with
    /// `CLAUDE_PLUGIN_ROOT` set to `root` and a PATH of just `path`, `input`
    /// on stdin. Nothing else of this process's environment is inherited.
    fn run_hook_command_from(
        command: &str,
        root: &Path,
        path: impl AsRef<OsStr>,
        cwd: &Path,
        home: &Path,
        input: &[u8],
    ) -> Output {
        let mut child = Command::new("/bin/sh")
            .args(["-c", command])
            .current_dir(cwd)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", home.join("templates"))
            .env("CLAUDE_PLUGIN_ROOT", root)
            // The directory the session was started in, which Claude Code
            // exports to every hook.
            .env("CLAUDE_PROJECT_DIR", cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }

    fn run_hook_command(
        command: &str,
        path: impl AsRef<OsStr>,
        cwd: &Path,
        home: &Path,
        input: &[u8],
    ) -> Output {
        run_hook_command_from(command, &plugin_root(), path, cwd, home, input)
    }

    /// The SessionStart command.
    fn run_check(path: impl AsRef<OsStr>, cwd: &Path, home: &Path) -> Output {
        let command = session_start_groups()[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_owned();
        run_hook_command(&command, path, cwd, home, b"")
    }

    /// The command of the plugin's `Stop` hook.
    fn ingest_command() -> String {
        let (_, groups) = hook_events()
            .into_iter()
            .find(|(event, _)| event == "Stop")
            .expect("hooks.json has a Stop hook");
        commands_of(&groups[0])[0].to_owned()
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

    fn assert_nothing_shown(output: &Output) {
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
    }

    /// The markers of these tests are only worth something if a program that
    /// runs leaves one, in the environments the hooks run in.
    #[test]
    fn a_project_supplied_program_leaves_its_marker_when_it_runs() {
        let temp = TempDir::new().unwrap();
        let marker = temp.path().join("marker");
        project_supplied(&temp.path().join("sh"), &marker);
        let output = Command::new("/bin/sh")
            .args(["-c", "./sh"])
            .current_dir(temp.path())
            .env_clear()
            .env("PATH", "")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert!(marker.exists(), "a program that runs leaves its marker");
    }

    #[test]
    fn it_says_so_when_cvc_is_not_on_the_path() {
        let path = empty_dir();
        let elsewhere = TempDir::new().unwrap();
        let output = run_check(path.path(), elsewhere.path(), elsewhere.path());
        assert_shows(&output, &not_on_the_path());
    }

    #[test]
    fn it_says_so_when_the_cvc_on_the_path_predates_the_silent_no_op() {
        let path = empty_dir();
        // What cvc 0.7.0 does with the probe: no session in the payload.
        executable(
            &path.path().join("cvc"),
            "#!/bin/sh\necho 'hook payload has no session_id' >&2\nexit 1\n",
        );
        let elsewhere = TempDir::new().unwrap();
        let output = run_check(path.path(), elsewhere.path(), elsewhere.path());
        assert_shows(&output, &too_old());
    }

    /// A `cvc` that is found in an absolute entry but cannot be run is neither
    /// old nor absent, and is not in a relative entry: an npm launcher whose
    /// node is not on the PATH, an interpreter that is gone, a program that
    /// fails the way a shell does when it cannot start one.
    #[test]
    fn it_says_so_when_the_cvc_it_found_could_not_be_run() {
        for (label, contents) in [
            (
                "an interpreter that does not exist",
                "#!/nonexistent/interp\n",
            ),
            ("exit status 127", "#!/bin/sh\nexit 127\n"),
            ("exit status 126", "#!/bin/sh\nexit 126\n"),
            ("exit status 2", "#!/bin/sh\nexit 2\n"),
            ("a signal", "#!/bin/sh\nkill -9 $$\n"),
        ] {
            let path = empty_dir();
            executable(&path.path().join("cvc"), contents);
            let elsewhere = TempDir::new().unwrap();
            let output = run_check(path.path(), elsewhere.path(), elsewhere.path());
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("could not be run"), "{label}: {stdout}");
            assert_shows(&output, &could_not_be_run());
        }
    }

    /// The plugin's hooks never use a `cvc` that is only reachable through a
    /// relative PATH entry (`.`, an empty entry, a directory name), and the
    /// check says that, rather than that cvc is missing or old. Nothing in the
    /// working directory runs.
    #[test]
    fn it_says_so_when_the_only_cvc_is_in_a_relative_path_entry() {
        let absolute = empty_dir();
        let absolute = absolute.path().display();
        for (label, entries, cvc) in [
            ("a dot", format!(".:{absolute}"), "cvc"),
            ("an empty entry", format!(":{absolute}"), "cvc"),
            ("a trailing empty entry", format!("{absolute}:"), "cvc"),
            (
                "a directory name",
                format!("relbin:{absolute}"),
                "relbin/cvc",
            ),
            (
                "a dot-dot",
                format!("../elsewhere:{absolute}"),
                "../elsewhere/cvc",
            ),
        ] {
            let temp = TempDir::new().unwrap();
            let cwd = temp.path().join("cwd");
            let marker = temp.path().join("marker");
            fs::create_dir(&cwd).unwrap();
            project_supplied(&cwd.join(cvc), &marker);
            let output = run_check(&entries, &cwd, temp.path());
            assert_shows(&output, &only_in_a_relative_entry());
            assert!(!marker.exists(), "{label}: the project's cvc ran");
        }
    }

    /// Where a relative entry comes before the real thing, the real one is
    /// what the check finds and runs, and the project's is never run.
    #[test]
    fn it_runs_the_cvc_in_an_absolute_entry_and_not_one_a_project_supplies_before_it() {
        let temp = TempDir::new().unwrap();
        let (home, repo) = initialized_acknowledged_repository(temp.path());
        let real = empty_dir();
        symlink(env!("CARGO_BIN_EXE_cvc"), real.path().join("cvc")).unwrap();
        let marker = temp.path().join("marker");
        project_supplied(&repo.join("cvc"), &marker);
        project_supplied(&repo.join("relbin").join("cvc"), &marker);
        let path = format!(".::relbin:{}", real.path().display());
        let before = snapshot(temp.path());

        let output = run_check(&path, &repo, &home);
        assert_nothing_shown(&output);
        assert!(!marker.exists(), "the project's cvc ran");
        assert_eq!(snapshot(temp.path()), before, "nothing was written");
    }

    /// The hooks' commands name `/bin/sh`: a `sh` that a project supplies, found
    /// through a `.` at the front of the PATH, does not run, whichever hook it
    /// would have been in.
    #[test]
    fn a_sh_the_project_supplies_never_runs_for_the_check() {
        let temp = TempDir::new().unwrap();
        let (home, repo) = initialized_acknowledged_repository(temp.path());
        let real = empty_dir();
        symlink(env!("CARGO_BIN_EXE_cvc"), real.path().join("cvc")).unwrap();
        let marker = temp.path().join("marker");
        project_supplied(&repo.join("sh"), &marker);
        let path = format!(".:{}", real.path().display());

        let output = run_check(&path, &repo, &home);
        assert_nothing_shown(&output);
        assert!(!marker.exists(), "the project's sh ran");
    }

    /// Run without a directory in its name (`sh check-cvc.sh`), the script
    /// cannot say where the launcher is, and does nothing: it does not look in
    /// the working directory, which is the project's, where a `run-cvc.sh` of
    /// its own could be.
    #[test]
    fn the_check_does_nothing_when_it_does_not_know_where_it_is() {
        // From the plugin's own scripts directory, the way it is spelled.
        let scripts = plugin_file("scripts");
        let real = empty_dir();
        symlink(env!("CARGO_BIN_EXE_cvc"), real.path().join("cvc")).unwrap();
        let output = Command::new("/bin/sh")
            .arg("check-cvc.sh")
            .current_dir(&scripts)
            .env_clear()
            .env("PATH", real.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_nothing_shown(&output);

        // From a project that supplies a `run-cvc.sh` beside a copy of it.
        let temp = TempDir::new().unwrap();
        let marker = temp.path().join("marker");
        fs::copy(
            scripts.join("check-cvc.sh"),
            temp.path().join("check-cvc.sh"),
        )
        .unwrap();
        project_supplied(&temp.path().join("run-cvc.sh"), &marker);
        let output = Command::new("/bin/sh")
            .arg("check-cvc.sh")
            .current_dir(temp.path())
            .env_clear()
            .env("PATH", "")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_nothing_shown(&output);
        assert!(!marker.exists(), "the project's run-cvc.sh ran");
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

    fn interactions(repo: &Path) -> i64 {
        rusqlite::Connection::open(repo.join(".git/cvc/index.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap()
    }

    /// The check against the `cvc` this build produces, run where hook mode
    /// would ingest if it were sent a real session, in a session whose project
    /// directory is that repository (`run_hook_command` exports it, as Claude
    /// Code does): it must say nothing, and leave no trace. This is what ties
    /// the probe to the binary, and shows the script's override of the project
    /// directory working: without it, hook mode would report the probe's
    /// missing session in this repository.
    #[test]
    fn it_is_silent_and_leaves_no_trace_with_this_builds_cvc_inside_a_capturing_repository() {
        let temp = TempDir::new().unwrap();
        let (home, repo) = initialized_acknowledged_repository(temp.path());
        let path = empty_dir();
        symlink(env!("CARGO_BIN_EXE_cvc"), path.path().join("cvc")).unwrap();
        let before = snapshot(temp.path());

        let output = run_check(path.path(), &repo, &home);
        assert_nothing_shown(&output);
        assert_eq!(
            snapshot(temp.path()),
            before,
            "nothing was created or written"
        );
        assert_eq!(interactions(&repo), 0);
    }

    /// A session to ingest: an initialized repository with capture
    /// acknowledged, and the payload Claude Code sends the `Stop` hook for a
    /// recorded transcript of it.
    struct Session {
        temp: TempDir,
        home: PathBuf,
        repo: PathBuf,
        payload: String,
    }

    fn session() -> Session {
        let temp = TempDir::new().unwrap();
        let (home, repo) = initialized_acknowledged_repository(temp.path());
        let transcripts = temp.path().join("transcripts");
        fs::create_dir(&transcripts).unwrap();
        let transcript = transcripts.join(format!("{SESSION}.jsonl"));
        let root = fs::canonicalize(&repo).unwrap();
        fs::write(
            &transcript,
            BASIC.replace("__WORKTREE__", root.to_str().unwrap()),
        )
        .unwrap();
        let payload = format!(
            "{{\"session_id\":\"{SESSION}\",\"transcript_path\":\"{}\",\"cwd\":\"{}\",\"hook_event_name\":\"Stop\"}}",
            transcript.display(),
            repo.display()
        );
        Session {
            temp,
            home,
            repo,
            payload,
        }
    }

    impl Session {
        /// The plugin's ingest hook, run in the repository with `path` as the
        /// PATH and the session's payload on stdin.
        fn ingest(&self, path: impl AsRef<OsStr>) -> Output {
            run_hook_command(
                &ingest_command(),
                path,
                &self.repo,
                &self.home,
                self.payload.as_bytes(),
            )
        }

        fn marker(&self) -> PathBuf {
            self.temp.path().join("marker")
        }

        /// A directory holding a link to the `cvc` this build produces.
        fn real_cvc_in(&self, directory: &Path) -> PathBuf {
            fs::create_dir_all(directory).unwrap();
            symlink(env!("CARGO_BIN_EXE_cvc"), directory.join("cvc")).unwrap();
            directory.to_path_buf()
        }
    }

    /// The plugin's hooks run in every project a user opens, so a `cvc` that
    /// the project supplies must never be what runs, whether the PATH reaches
    /// it by `.`, by an empty entry or by a directory name. The real one
    /// further along an absolute entry ingests the session, with its payload
    /// on stdin and its arguments as given.
    #[test]
    fn the_ingest_hook_skips_relative_path_entries_and_runs_the_cvc_in_an_absolute_one() {
        let session = session();
        let marker = session.marker();
        project_supplied(&session.repo.join("cvc"), &marker);
        project_supplied(&session.repo.join("relbin").join("cvc"), &marker);
        let real = session.real_cvc_in(&session.temp.path().join("real"));
        let path = format!(".::relbin:{}", real.display());

        let output = session.ingest(&path);
        assert_nothing_shown(&output);
        assert_eq!(interactions(&session.repo), 7, "the real cvc ingested");
        assert!(!marker.exists(), "the project's cvc ran");
    }

    /// The hook's command starts with `/bin/sh`, not `sh`: with `.` first on the
    /// PATH, a `sh` the project supplies would otherwise run in place of the
    /// launcher. It does not, and the session is captured all the same.
    #[test]
    fn a_sh_the_project_supplies_never_runs_for_the_ingest_hook() {
        let session = session();
        let marker = session.marker();
        project_supplied(&session.repo.join("sh"), &marker);
        project_supplied(&session.repo.join("cvc"), &marker);
        let real = session.real_cvc_in(&session.temp.path().join("real"));
        let path = format!(".:{}", real.display());

        let output = session.ingest(&path);
        assert_nothing_shown(&output);
        assert_eq!(interactions(&session.repo), 7, "the real cvc ingested");
        assert!(!marker.exists(), "something the project supplied ran");
    }

    #[test]
    fn the_ingest_hook_with_only_relative_cvc_exits_127_and_says_why() {
        let session = session();
        let marker = session.marker();
        project_supplied(&session.repo.join("cvc"), &marker);
        project_supplied(&session.repo.join("relbin").join("cvc"), &marker);
        let absolute = empty_dir();
        let path = format!(".::relbin:{}", absolute.path().display());

        let output = session.ingest(&path);
        assert_eq!(output.status.code(), Some(127));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("{NOT_FOUND}\n")
        );
        assert!(!marker.exists(), "the project's cvc ran");
        assert_eq!(interactions(&session.repo), 0);
    }

    /// Entries are split on `:` alone: spaces stay in a directory's name.
    #[test]
    fn an_absolute_path_entry_with_a_space_in_it_is_used() {
        let session = session();
        let real = session.real_cvc_in(&session.temp.path().join("a bin dir"));

        let output = session.ingest(real.display().to_string());
        assert_nothing_shown(&output);
        assert_eq!(interactions(&session.repo), 7);
    }

    /// Entries are not subject to filename expansion: a pattern in PATH does
    /// not stand for the directories it matches.
    #[test]
    fn a_pattern_in_an_absolute_path_entry_is_not_expanded() {
        let session = session();
        let parent = session.temp.path().join("g");
        session.real_cvc_in(&parent.join("real"));

        let output = session.ingest(format!("{}/*", parent.display()));
        assert_eq!(output.status.code(), Some(127));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("{NOT_FOUND}\n")
        );
        assert_eq!(interactions(&session.repo), 0);
    }

    /// The exit status is `cvc`'s own, here the visible failure of a payload
    /// with no session, and what it prints to stderr is untouched.
    #[test]
    fn the_ingest_hook_passes_cvcs_exit_status_and_messages_through() {
        let session = session();
        let real = session.real_cvc_in(&session.temp.path().join("real"));

        let payload = format!("{{\"cwd\":\"{}\"}}", session.repo.display());
        let output = run_hook_command(
            &ingest_command(),
            real.display().to_string(),
            &session.repo,
            &session.home,
            payload.as_bytes(),
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("no session_id"), "{stderr}");
        assert_eq!(interactions(&session.repo), 0);
    }

    /// Claude Code takes exit status 2 as "block", and on a `Stop` hook that
    /// keeps Claude from stopping. A shell that cannot open the script (a
    /// plugin cache replaced mid-session, a broken install) may exit with 2, so
    /// the command maps it to 1, and passes every other status through.
    #[test]
    fn the_ingest_command_never_exits_2() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        let run = |launcher: Option<&str>| {
            let root = TempDir::new().unwrap();
            fs::create_dir(root.path().join("scripts")).unwrap();
            if let Some(contents) = launcher {
                fs::write(root.path().join("scripts/run-cvc.sh"), contents).unwrap();
            }
            run_hook_command_from(
                &ingest_command(),
                root.path(),
                "",
                temp.path(),
                &home,
                b"{}",
            )
        };
        assert_eq!(run(Some("exit 2\n")).status.code(), Some(1));
        assert_eq!(run(Some("exit 1\n")).status.code(), Some(1));
        assert_eq!(run(Some("exit 0\n")).status.code(), Some(0));
        assert_eq!(run(Some("exit 3\n")).status.code(), Some(3));
        assert_eq!(run(Some("exit 127\n")).status.code(), Some(127));
        // A launcher that is not there: the shell's own status (127 under bash,
        // 2 under older dash and busybox), and never 2.
        let missing = run(None);
        let code = missing.status.code();
        assert!(code.is_some_and(|code| code != 0 && code != 2), "{code:?}");
    }

    /// The launcher run on its own, with stand-ins that report what they got:
    /// the arguments as given (spaces, patterns and empty ones included), all
    /// of stdin, and the status the program exits with.
    #[test]
    fn the_launcher_passes_arguments_stdin_and_status_through() {
        let temp = TempDir::new().unwrap();
        let bin = temp.path().join("bin");
        executable(
            &bin.join("cvc"),
            "#!/bin/sh\nprintf 'ran cvc\\n'\nfor argument in \"$@\"; do printf 'argument [%s]\\n' \"$argument\"; done\nwhile IFS= read -r line; do printf 'input [%s]\\n' \"$line\"; done\nexit 42\n",
        );
        let launcher = plugin_file("scripts/run-cvc.sh");
        let mut child = Command::new("/bin/sh")
            .arg(&launcher)
            .args(["one two", "*", "", "--hook"])
            .current_dir(temp.path())
            .env_clear()
            .env("PATH", format!(".:{}", bin.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"first\nsecond line\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(42));
        assert!(output.stderr.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "ran cvc\nargument [one two]\nargument [*]\nargument []\nargument [--hook]\ninput [first]\ninput [second line]\n"
        );
    }

    /// Directories the launcher's selection cases are built from.
    fn candidates(temp: &Path) {
        let named = |name: &str| format!("#!/bin/sh\nprintf '%s\\n' '{name}'\n");
        executable(&temp.join("both/cvc"), &named("both: cvc"));
        executable(&temp.join("both/cvc.exe"), &named("both: cvc.exe"));
        executable(&temp.join("exe-only/cvc.exe"), &named("exe-only: cvc.exe"));
        executable(&temp.join("later/cvc"), &named("later: cvc"));
        // Not usable: a directory called cvc, and a file that is not executable.
        fs::create_dir_all(temp.join("a-directory/cvc")).unwrap();
        fs::create_dir_all(temp.join("not-executable")).unwrap();
        fs::write(temp.join("not-executable/cvc"), "#!/bin/sh\n").unwrap();
    }

    fn launcher_with(temp: &Path, entries: &[&str], locate: Option<&str>) -> Output {
        let path: Vec<String> = entries
            .iter()
            .map(|entry| temp.join(entry).display().to_string())
            .collect();
        let mut command = Command::new("/bin/sh");
        command
            .arg(plugin_file("scripts/run-cvc.sh"))
            .env_clear()
            .env("PATH", path.join(":"))
            .stdin(Stdio::null());
        if let Some(value) = locate {
            command.env("CVC_PLUGIN_LOCATE_ONLY", value);
        }
        command.output().unwrap()
    }

    /// In an absolute directory `cvc` comes before `cvc.exe`, which is used
    /// where there is no `cvc`; the first directory with either wins; and
    /// neither counts unless it is an executable regular file.
    #[test]
    fn the_launcher_prefers_cvc_to_cvc_exe_and_the_first_directory_that_has_either() {
        let temp = TempDir::new().unwrap();
        candidates(temp.path());
        let run = |entries: &[&str]| {
            let output = launcher_with(temp.path(), entries, None);
            assert_eq!(output.status.code(), Some(0));
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        assert_eq!(run(&["both", "later"]), "both: cvc");
        assert_eq!(run(&["exe-only", "later"]), "exe-only: cvc.exe");
        assert_eq!(
            run(&["a-directory", "not-executable", "later"]),
            "later: cvc"
        );
    }

    /// With `CVC_PLUGIN_LOCATE_ONLY=1` the launcher prints the path it would
    /// run, and a newline, and exits 0, by the same rule as when it runs it, and
    /// without running it.
    #[test]
    fn the_launcher_in_locate_mode_prints_the_path_and_runs_nothing() {
        let temp = TempDir::new().unwrap();
        candidates(temp.path());
        let marker = temp.path().join("marker");
        project_supplied(&temp.path().join("marked/cvc"), &marker);

        let locate = |entries: &[&str]| {
            let output = launcher_with(temp.path(), entries, Some("1"));
            assert_eq!(output.status.code(), Some(0));
            assert!(output.stderr.is_empty());
            String::from_utf8(output.stdout).unwrap()
        };
        let path_of = |relative: &str| format!("{}\n", temp.path().join(relative).display());
        assert_eq!(locate(&["both", "later"]), path_of("both/cvc"));
        assert_eq!(locate(&["exe-only", "later"]), path_of("exe-only/cvc.exe"));
        assert_eq!(
            locate(&["a-directory", "not-executable", "later"]),
            path_of("later/cvc")
        );
        assert_eq!(locate(&["marked"]), path_of("marked/cvc"));
        assert!(
            !marker.exists(),
            "it located the program and did not run it"
        );

        // Arguments are ignored; only the value 1 asks for it; and without it
        // the same program is run.
        let output = Command::new("/bin/sh")
            .arg(plugin_file("scripts/run-cvc.sh"))
            .args(["ingest", "claude-code", "--hook"])
            .env_clear()
            .env("PATH", temp.path().join("marked"))
            .env("CVC_PLUGIN_LOCATE_ONLY", "1")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert!(!marker.exists());
        for value in ["0", "", "yes"] {
            let output = launcher_with(temp.path(), &["marked"], Some(value));
            assert_eq!(output.status.code(), Some(0), "{value:?}");
            assert!(output.stdout.is_empty(), "{value:?}");
            assert!(marker.exists(), "{value:?}: the program ran");
            fs::remove_file(&marker).unwrap();
        }
    }

    /// Nothing found is the same in locate mode: exit 127, the message, and
    /// nothing on stdout.
    #[test]
    fn the_launcher_in_locate_mode_still_says_when_there_is_no_cvc() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join("empty")).unwrap();
        let marker = temp.path().join("marker");
        project_supplied(&temp.path().join("cvc"), &marker);
        let output = Command::new("/bin/sh")
            .arg(plugin_file("scripts/run-cvc.sh"))
            .current_dir(temp.path())
            .env_clear()
            .env(
                "PATH",
                format!(".::{}", temp.path().join("empty").display()),
            )
            .env("CVC_PLUGIN_LOCATE_ONLY", "1")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(127));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("{NOT_FOUND}\n")
        );
        assert!(!marker.exists());
    }
}
