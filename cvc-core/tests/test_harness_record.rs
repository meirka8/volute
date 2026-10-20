//! The repository-wide Claude Code harness record, the safety rules for the
//! settings file that propagation writes into a worktree, and the survey of
//! sibling worktrees that `cvc harness install|uninstall` reports from.
use cvc_core::claude_code::harness_record::{clear_record, read_record, record_path, write_record};
use cvc_core::claude_code::settings::{
    self, TrackedSettings, WorktreeHooks, WorktreeState, SETTINGS_RELATIVE_PATH,
};
use cvc_core::repository::RepositoryLayout;
use git2::{Repository, Signature};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    layout: RepositoryLayout,
}

fn commit_empty(repo: &Repository) {
    let signature = Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let tree = {
        let mut index = repo.index().unwrap();
        let id = index.write_tree().unwrap();
        repo.find_tree(id).unwrap()
    };
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
}

impl Fixture {
    /// An initialized repository with one commit, so linked worktrees can be
    /// created from it.
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("repo");
        fs::create_dir(&repo_dir).unwrap();
        let repo = Repository::init(&repo_dir).unwrap();
        commit_empty(&repo);
        drop(repo);
        let layout = RepositoryLayout::discover(&repo_dir).unwrap();
        fs::create_dir_all(layout.cvc_dir()).unwrap();
        let root = layout.worktree_root().unwrap().to_path_buf();
        Self {
            _temp: temp,
            root,
            layout,
        }
    }

    /// A stand-in `cvc`: an existing, executable file.
    fn binary(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// Adds a linked worktree named `name` next to the repository.
    fn linked_worktree(&self, name: &str) -> PathBuf {
        let path = self.root.parent().unwrap().join(format!("linked-{name}"));
        self.layout
            .repository()
            .worktree(name, &path, None)
            .unwrap();
        fs::canonicalize(path).unwrap()
    }

    fn outside(&self) -> PathBuf {
        let path = self.root.parent().unwrap().join("outside");
        fs::create_dir_all(&path).unwrap();
        path
    }
}

fn record_file(fixture: &Fixture) -> PathBuf {
    record_path(&fixture.layout)
}

// ---- the record ----

#[test]
fn the_record_is_one_absolute_path_and_a_newline() {
    let fixture = Fixture::new();
    assert_eq!(read_record(&fixture.layout).unwrap(), None);

    let binary = fixture.binary("cvc");
    write_record(&fixture.layout, &binary).unwrap();
    assert_eq!(read_record(&fixture.layout).unwrap(), Some(binary.clone()));

    // The file is exactly the path and one newline: what the Git hook script
    // reads with `read -r`.
    assert_eq!(
        record_file(&fixture),
        fixture.layout.cvc_dir().join("harness/claude-code.path")
    );
    assert_eq!(
        fs::read_to_string(record_file(&fixture)).unwrap(),
        format!("{}\n", binary.display())
    );

    // A later install replaces the record; the last binary wins.
    let moved = fixture.binary("moved-cvc");
    write_record(&fixture.layout, &moved).unwrap();
    assert_eq!(read_record(&fixture.layout).unwrap(), Some(moved));
    let names: Vec<_> = fs::read_dir(fixture.layout.cvc_dir().join("harness"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["claude-code.path"]);
}

#[test]
fn write_validates_its_input_and_never_initializes_a_repository() {
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    // The hook entries need an absolute path, and one line holds no more.
    for bad in ["relative/cvc", "/tmp/a\nb", "/tmp/a\rb", "/tmp/a\0b", ""] {
        assert!(
            write_record(&fixture.layout, Path::new(bad)).is_err(),
            "{bad:?}"
        );
    }
    assert!(!fixture.layout.cvc_dir().join("harness").exists());

    let cvc_dir = fixture.layout.cvc_dir();
    fs::remove_dir_all(&cvc_dir).unwrap();
    assert!(write_record(&fixture.layout, &binary).is_err());
    assert!(!cvc_dir.exists());
    assert_eq!(read_record(&fixture.layout).unwrap(), None);
    assert!(!clear_record(&fixture.layout).unwrap());
}

#[cfg(unix)]
#[test]
fn record_and_directory_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    write_record(&fixture.layout, &fixture.binary("cvc")).unwrap();
    let file = fs::metadata(record_file(&fixture)).unwrap();
    assert_eq!(file.permissions().mode() & 0o777, 0o600);
    let dir = fs::metadata(record_file(&fixture).parent().unwrap()).unwrap();
    assert_eq!(dir.permissions().mode() & 0o777, 0o700);

    // A rewrite keeps the mode: it never goes through a wider-open file.
    write_record(&fixture.layout, &fixture.binary("other")).unwrap();
    let file = fs::metadata(record_file(&fixture)).unwrap();
    assert_eq!(file.permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn a_record_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    write_record(&fixture.layout, &fixture.binary("cvc")).unwrap();
    let file = record_file(&fixture);
    let dir = file.parent().unwrap().to_path_buf();
    let chmod = |path: &Path, mode: u32| {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap()
    };

    // Group- or world-writable: refused, and the error says why.
    for mode in [0o660, 0o620, 0o666, 0o602] {
        chmod(&file, mode);
        let error = read_record(&fixture.layout).expect_err(&format!("{mode:o}"));
        assert!(
            format!("{error:#}").contains("writable by group or others"),
            "{mode:o}: {error:#}"
        );
    }
    // Readable by others is harmless (it is a path) and stays accepted.
    for mode in [0o600, 0o640, 0o644] {
        chmod(&file, mode);
        assert!(read_record(&fixture.layout).unwrap().is_some(), "{mode:o}");
    }
    // The same rule for the directory that holds it.
    chmod(&file, 0o600);
    for mode in [0o770, 0o722, 0o777] {
        chmod(&dir, mode);
        let error = read_record(&fixture.layout).expect_err(&format!("dir {mode:o}"));
        assert!(
            format!("{error:#}").contains("writable by group or others"),
            "dir {mode:o}: {error:#}"
        );
    }
    // A refused record can still be removed (and rewritten) by its owner.
    chmod(&dir, 0o700);
    chmod(&file, 0o666);
    assert!(clear_record(&fixture.layout).unwrap());
    write_record(&fixture.layout, &fixture.binary("cvc")).unwrap();
    assert!(read_record(&fixture.layout).unwrap().is_some());
}

#[test]
fn clear_removes_the_record_and_reports_whether_there_was_one() {
    let fixture = Fixture::new();
    assert!(!clear_record(&fixture.layout).unwrap());

    write_record(&fixture.layout, &fixture.binary("cvc")).unwrap();
    assert!(clear_record(&fixture.layout).unwrap());
    assert_eq!(read_record(&fixture.layout).unwrap(), None);
    // The now-empty directory goes too.
    assert!(!fixture.layout.cvc_dir().join("harness").exists());
    assert!(!clear_record(&fixture.layout).unwrap());

    // A directory that holds something else stays.
    write_record(&fixture.layout, &fixture.binary("cvc")).unwrap();
    let other = fixture.layout.cvc_dir().join("harness/other.json");
    fs::write(&other, "{}").unwrap();
    assert!(clear_record(&fixture.layout).unwrap());
    assert!(other.exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_record_or_directory_is_refused() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let outside = fixture.outside();

    // Symlinked record file: never read, replaced, or removed through.
    let target = outside.join("target.path");
    fs::write(&target, "/bin/sh\n").unwrap();
    let harness = fixture.layout.cvc_dir().join("harness");
    fs::create_dir(&harness).unwrap();
    symlink(&target, harness.join("claude-code.path")).unwrap();
    assert!(read_record(&fixture.layout).is_err());
    assert!(write_record(&fixture.layout, &binary).is_err());
    assert!(clear_record(&fixture.layout).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "/bin/sh\n");
    assert!(fs::symlink_metadata(harness.join("claude-code.path"))
        .unwrap()
        .file_type()
        .is_symlink());
    fs::remove_dir_all(&harness).unwrap();

    // Symlinked directory: nothing is written into, or removed from, its target.
    fs::write(outside.join("claude-code.path"), "keep").unwrap();
    symlink(&outside, &harness).unwrap();
    assert!(read_record(&fixture.layout).is_err());
    assert!(write_record(&fixture.layout, &binary).is_err());
    assert!(clear_record(&fixture.layout).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("claude-code.path")).unwrap(),
        "keep"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 2);
}

#[test]
fn a_malformed_record_is_an_error_and_can_still_be_cleared() {
    let fixture = Fixture::new();
    let harness = fixture.layout.cvc_dir().join("harness");
    fs::create_dir(&harness).unwrap();
    let path = harness.join("claude-code.path");
    let oversized = format!("/{}\n", "a".repeat(9 * 1024));
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("only a newline", b"\n".to_vec()),
        ("relative", b"relative/cvc\n".to_vec()),
        ("two lines", b"/opt/cvc\n/opt/other\n".to_vec()),
        ("carriage return", b"/opt/cvc\r\n".to_vec()),
        ("embedded newline", b"/opt\ncvc\n".to_vec()),
        ("NUL", b"/opt/cv\0c\n".to_vec()),
        ("not UTF-8", vec![b'/', 0xff, 0xfe, b'\n']),
        ("oversized", oversized.into_bytes()),
    ];
    for (name, content) in cases {
        fs::write(&path, content).unwrap();
        let error = read_record(&fixture.layout).expect_err(name);
        assert!(
            format!("{error:#}").contains("claude-code.path"),
            "{name}: {error:#}"
        );
    }

    // A directory where the file belongs is not a record either.
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(read_record(&fixture.layout).is_err());
    fs::remove_dir(&path).unwrap();

    // The final newline is optional on read, and nothing else is tolerated.
    for content in ["/opt/cvc\n", "/opt/cvc"] {
        fs::write(&path, content).unwrap();
        assert_eq!(
            read_record(&fixture.layout).unwrap(),
            Some(PathBuf::from("/opt/cvc"))
        );
    }

    // A damaged record never blocks an uninstall, and a rewrite repairs it.
    fs::write(&path, "garbage").unwrap();
    assert!(clear_record(&fixture.layout).unwrap());
    fs::create_dir(&harness).unwrap();
    fs::write(&path, "garbage").unwrap();
    write_record(&fixture.layout, &fixture.binary("cvc")).unwrap();
    assert!(read_record(&fixture.layout).unwrap().is_some());
}

#[test]
fn concurrent_writers_never_expose_a_torn_record() {
    let fixture = Fixture::new();
    let root = fixture.root.clone();
    let binaries: Vec<PathBuf> = (0..4)
        .map(|index| fixture.binary(&format!("cvc-{index}")))
        .collect();
    write_record(&fixture.layout, &binaries[0]).unwrap();

    let writers: Vec<_> = binaries
        .iter()
        .cloned()
        .map(|binary| {
            let root = root.clone();
            std::thread::spawn(move || {
                let layout = RepositoryLayout::discover(&root).unwrap();
                for _ in 0..50 {
                    write_record(&layout, &binary).unwrap();
                }
            })
        })
        .collect();
    let reader = {
        let root = root.clone();
        std::thread::spawn(move || {
            let layout = RepositoryLayout::discover(&root).unwrap();
            for _ in 0..400 {
                // Always the old record or a new one, never absent or damaged.
                read_record(&layout).unwrap().unwrap();
            }
        })
    };
    for writer in writers {
        writer.join().unwrap();
    }
    reader.join().unwrap();
    let names: Vec<_> = fs::read_dir(fixture.layout.cvc_dir().join("harness"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["claude-code.path"]);
}

#[test]
fn every_worktree_of_the_repository_shares_one_record() {
    let fixture = Fixture::new();
    let linked = fixture.linked_worktree("wt");
    let linked_layout = RepositoryLayout::discover(&linked).unwrap();
    assert_eq!(linked_layout.cvc_dir(), fixture.layout.cvc_dir());

    let binary = fixture.binary("cvc");
    write_record(&fixture.layout, &binary).unwrap();
    assert_eq!(read_record(&linked_layout).unwrap(), Some(binary));
    assert!(clear_record(&linked_layout).unwrap());
    assert_eq!(read_record(&fixture.layout).unwrap(), None);
}

// ---- the settings file in a worktree a branch controls ----

#[test]
fn a_binary_must_be_absolute_existing_and_executable() {
    let fixture = Fixture::new();
    let good = fixture.binary("cvc");
    assert_eq!(settings::binary_problem(&good), None);
    assert!(settings::hook_command(&good).is_ok());

    assert_eq!(
        settings::binary_problem(Path::new("relative/cvc")),
        Some("relative/cvc is not an absolute path".into())
    );
    let missing = fixture.root.join("missing");
    assert_eq!(
        settings::binary_problem(&missing),
        Some(format!("{} does not exist", missing.display()))
    );
    assert_eq!(
        settings::binary_problem(&fixture.root),
        Some(format!("{} is not a file", fixture.root.display()))
    );
    assert!(settings::hook_command(&missing).is_err());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let plain = fixture.root.join("plain");
        fs::write(&plain, "x").unwrap();
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            settings::binary_problem(&plain),
            Some(format!("{} is not executable", plain.display()))
        );
        let error = settings::install(&fixture.layout, &plain).unwrap_err();
        assert!(
            format!("{error:#}").contains("is not executable"),
            "{error:#}"
        );
        assert!(!fixture.root.join(".claude").exists());
        // Any execute bit will do: it is the owner's choice who may run it.
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o601)).unwrap();
        assert_eq!(settings::binary_problem(&plain), None);
    }
}

#[cfg(unix)]
#[test]
fn a_claude_directory_that_is_a_link_or_a_file_is_never_written_through() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let outside = fixture.outside();
    let claude = fixture.root.join(".claude");
    let dangling = fixture.root.parent().unwrap().join("not-there");

    for name in [
        "absolute symlink",
        "relative symlink",
        "dangling symlink",
        "file",
    ] {
        let expected = match name {
            "absolute symlink" => {
                symlink(&outside, &claude).unwrap();
                "is a symlink; not following it"
            }
            "relative symlink" => {
                symlink(Path::new("..").join("outside"), &claude).unwrap();
                "is a symlink; not following it"
            }
            "dangling symlink" => {
                symlink(&dangling, &claude).unwrap();
                "is a symlink; not following it"
            }
            _ => {
                fs::write(&claude, "i am a file").unwrap();
                "is not a directory"
            }
        };
        let error = settings::install(&fixture.layout, &binary).expect_err(name);
        let text = format!("{error:#}");
        assert!(text.contains(expected), "{name}: {text}");
        assert!(
            text.contains(claude.to_str().unwrap()),
            "{name}: names the path: {text}"
        );
        assert!(!text.contains('\n'), "{name}: one line: {text}");
        assert!(settings::uninstall(&fixture.layout).is_err(), "{name}");
        assert!(!settings::installed(&fixture.layout), "{name}");
        // Nothing appeared anywhere a link could have led.
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "{name}");
        assert!(!dangling.exists(), "{name}: dangling target was created");
        // A link or file is left exactly as the branch committed it.
        assert!(fs::symlink_metadata(&claude).is_ok(), "{name}");
        fs::remove_file(&claude).unwrap();
    }

    // A real directory is used, and created when absent.
    settings::install(&fixture.layout, &binary).unwrap();
    assert!(!fs::symlink_metadata(&claude)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(claude.is_dir());
}

#[cfg(unix)]
#[test]
fn a_settings_file_that_is_a_link_is_never_written_through() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let outside = fixture.outside();
    let target = outside.join("settings.json");
    fs::write(&target, "{\"untouched\":true}\n").unwrap();
    fs::create_dir(fixture.root.join(".claude")).unwrap();
    symlink(&target, fixture.root.join(SETTINGS_RELATIVE_PATH)).unwrap();

    assert!(settings::install(&fixture.layout, &binary).is_err());
    assert!(settings::uninstall(&fixture.layout).is_err());
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "{\"untouched\":true}\n"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn settings_are_written_through_a_random_private_temporary_file() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let path = settings::settings_path(&fixture.layout).unwrap();
    let dir = path.parent().unwrap().to_path_buf();

    // A stale file under the old fixed temporary name is neither used nor
    // clobbered: the temporary name is random now.
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join(".settings.local.json.cvc-tmp"), "stale").unwrap();
    settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join(".settings.local.json.cvc-tmp")).unwrap(),
        "stale"
    );
    let mut names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [".settings.local.json.cvc-tmp", "settings.local.json"]
    );

    // A new file is private; one that existed keeps the mode it had.
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let moved = fixture.binary("moved-cvc");
    settings::install(&fixture.layout, &moved).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

#[test]
fn a_tracked_settings_file_is_refused_and_left_alone() {
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let path = settings::settings_path(&fixture.layout).unwrap();
    fs::create_dir(path.parent().unwrap()).unwrap();
    let committed = "{\"permissions\":{\"allow\":[\"Bash(ls:*)\"]}}\n";
    fs::write(&path, committed).unwrap();
    {
        let mut index = fixture.layout.repository().index().unwrap();
        index.add_path(Path::new(SETTINGS_RELATIVE_PATH)).unwrap();
        index.write().unwrap();
    }

    let error = settings::install(&fixture.layout, &binary).unwrap_err();
    assert!(error.is::<TrackedSettings>(), "{error:#}");
    assert_eq!(
        error.to_string(),
        "`.claude/settings.local.json` is tracked by Git in this checkout; CVC writes a machine-specific path there and will not modify a tracked file (untrack it with `git rm --cached .claude/settings.local.json`)"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), committed);
    // Refusal comes before anything else is written, the exclude included.
    let exclude = fixture.layout.common_git_dir().join("info/exclude");
    assert!(!fs::read_to_string(exclude)
        .unwrap_or_default()
        .contains("settings.local.json"));

    // Untracked, it is just a settings file again and the foreign content is
    // merged around.
    {
        let mut index = fixture.layout.repository().index().unwrap();
        index
            .remove_path(Path::new(SETTINGS_RELATIVE_PATH))
            .unwrap();
        index.write().unwrap();
    }
    let outcome = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(outcome.action, settings::InstallAction::Created);
    let merged: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(merged["permissions"]["allow"][0], "Bash(ls:*)");
}

#[test]
fn create_only_ever_creates_the_settings_file() {
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let path = settings::settings_path(&fixture.layout).unwrap();

    // Nothing there yet: created, like an install.
    let outcome = settings::create(&fixture.layout, &binary).unwrap();
    assert_eq!(outcome.action, settings::InstallAction::Created);
    let created = fs::read_to_string(&path).unwrap();
    assert!(created.contains("ingest claude-code --hook"));

    // Anything there already -- ours included -- is refused, never merged into
    // and never rewritten, whatever it holds.
    let message = "`.claude/settings.local.json` already exists in this worktree; CVC only creates it in a new worktree (run `cvc harness install claude-code` here to add the hooks)";
    let mut names = vec![
        "our own entries",
        "valid foreign settings",
        "malformed",
        "empty",
        "a directory",
    ];
    #[cfg(unix)]
    names.push("a dangling symlink");
    for name in names {
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&path);
        match name {
            "our own entries" => fs::write(&path, &created).unwrap(),
            "valid foreign settings" => fs::write(&path, "{\"permissions\":{}}\n").unwrap(),
            "malformed" => fs::write(&path, "{ not json").unwrap(),
            "empty" => fs::write(&path, "").unwrap(),
            "a directory" => fs::create_dir(&path).unwrap(),
            #[cfg(unix)]
            _ => std::os::unix::fs::symlink(path.with_file_name("nowhere"), &path).unwrap(),
            #[cfg(not(unix))]
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&path).unwrap().file_type();
        let error = settings::create(&fixture.layout, &binary).expect_err(name);
        assert!(error.is::<settings::SettingsExist>(), "{name}: {error:#}");
        assert_eq!(error.to_string(), message, "{name}");
        // Untouched: same kind of thing, and no temporary left behind.
        assert_eq!(
            fs::symlink_metadata(&path).unwrap().file_type(),
            before,
            "{name}"
        );
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".cvc-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{name}: {leftovers:?}");
    }
    // `install` (the manual command) still merges into an existing file.
    let _ = fs::remove_dir_all(&path);
    let _ = fs::remove_file(&path);
    fs::write(&path, "{\"permissions\":{\"allow\":[\"Bash(ls:*)\"]}}\n").unwrap();
    let merged = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(merged.action, settings::InstallAction::Created);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("Bash(ls:*)") && text.contains("ingest claude-code --hook"));
}

#[test]
fn a_tracked_settings_file_is_reported_as_tracked_before_as_existing() {
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let path = settings::settings_path(&fixture.layout).unwrap();
    fs::create_dir(path.parent().unwrap()).unwrap();
    fs::write(&path, "{}\n").unwrap();
    {
        let mut index = fixture.layout.repository().index().unwrap();
        index.add_path(Path::new(SETTINGS_RELATIVE_PATH)).unwrap();
        index.write().unwrap();
    }
    let error = settings::create(&fixture.layout, &binary).unwrap_err();
    assert!(error.is::<TrackedSettings>(), "{error:#}");
    assert!(!error.is::<settings::SettingsExist>());
    assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
}

#[cfg(unix)]
#[test]
fn a_harness_directory_others_can_write_fails_the_record_step() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let harness = fixture.layout.cvc_dir().join("harness");
    fs::create_dir(&harness).unwrap();
    for mode in [0o770, 0o722, 0o777] {
        fs::set_permissions(&harness, fs::Permissions::from_mode(mode)).unwrap();
        let error = write_record(&fixture.layout, &binary).expect_err(&format!("{mode:o}"));
        assert!(
            format!("{error:#}").contains("writable by group or others"),
            "{mode:o}: {error:#}"
        );
        // Nothing was written into it.
        assert_eq!(fs::read_dir(&harness).unwrap().count(), 0, "{mode:o}");
    }
    // Private (even if wider for reading), the same call succeeds.
    for mode in [0o700, 0o750, 0o755] {
        fs::set_permissions(&harness, fs::Permissions::from_mode(mode)).unwrap();
        write_record(&fixture.layout, &binary).unwrap();
        assert_eq!(read_record(&fixture.layout).unwrap(), Some(binary.clone()));
    }
}

#[test]
fn only_the_settings_file_itself_blocks_an_install() {
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    let claude = fixture.root.join(".claude");
    fs::create_dir_all(claude.join("agents")).unwrap();
    fs::write(claude.join("settings.json"), "{}\n").unwrap();
    fs::write(claude.join("agents/reviewer.md"), "# reviewer\n").unwrap();
    {
        let mut index = fixture.layout.repository().index().unwrap();
        index.add_path(Path::new(".claude/settings.json")).unwrap();
        index
            .add_path(Path::new(".claude/agents/reviewer.md"))
            .unwrap();
        index.write().unwrap();
    }
    let outcome = settings::install(&fixture.layout, &binary).unwrap();
    assert_eq!(outcome.action, settings::InstallAction::Created);
    assert_eq!(
        fs::read_to_string(claude.join("settings.json")).unwrap(),
        "{}\n"
    );
}

// ---- the survey of other worktrees ----

#[test]
fn only_a_linked_worktree_is_linked() {
    let fixture = Fixture::new();
    assert!(!fixture.layout.is_linked_worktree());
    let linked = fixture.linked_worktree("wt");
    let linked_layout = RepositoryLayout::discover(&linked).unwrap();
    assert!(linked_layout.is_linked_worktree());
    // From a subdirectory, too: the answer is about the repository layout, not
    // about where in the worktree the caller happens to stand.
    fs::create_dir(linked.join("sub")).unwrap();
    assert!(RepositoryLayout::discover(linked.join("sub"))
        .unwrap()
        .is_linked_worktree());
    fs::create_dir(fixture.root.join("sub")).unwrap();
    assert!(!RepositoryLayout::discover(fixture.root.join("sub"))
        .unwrap()
        .is_linked_worktree());
}

fn root_of(worktree: &Path) -> PathBuf {
    fs::canonicalize(worktree).unwrap()
}

#[test]
fn other_worktrees_lists_siblings_with_their_hook_state() {
    let fixture = Fixture::new();
    let binary = fixture.binary("cvc");
    // Alone in the repository: nothing else to report.
    let alone = settings::other_worktrees(&fixture.layout).unwrap();
    assert!(alone.worktrees.is_empty());
    assert!(alone.main_located);

    let first = fixture.linked_worktree("a");
    let second = fixture.linked_worktree("b");
    let first_layout = RepositoryLayout::discover(&first).unwrap();
    settings::install(&first_layout, &binary).unwrap();

    let hooks = |root: &Path, state: WorktreeState| WorktreeHooks {
        root: root.to_path_buf(),
        state,
    };
    // From the main worktree: both linked worktrees, the active one excluded.
    let from_main = settings::other_worktrees(&fixture.layout).unwrap();
    assert!(from_main.main_located);
    assert_eq!(
        from_main.worktrees,
        vec![
            hooks(&first, WorktreeState::Installed),
            hooks(&second, WorktreeState::NotInstalled),
        ]
    );

    // From a linked worktree: the main worktree comes first, itself excluded.
    let second_layout = RepositoryLayout::discover(&second).unwrap();
    let from_second = settings::other_worktrees(&second_layout).unwrap();
    assert!(from_second.main_located);
    assert_eq!(
        from_second.worktrees,
        vec![
            hooks(&root_of(&fixture.root), WorktreeState::NotInstalled),
            hooks(&first, WorktreeState::Installed),
        ]
    );

    // Installing in the main worktree flips only its entry.
    settings::install(&fixture.layout, &binary).unwrap();
    let from_second = settings::other_worktrees(&second_layout).unwrap();
    assert!(from_second.worktrees[0].installed());
    assert!(from_second.worktrees[1].installed());

    // A worktree directory deleted without `git worktree remove` is skipped.
    fs::remove_dir_all(&second).unwrap();
    let from_main = settings::other_worktrees(&fixture.layout).unwrap();
    assert_eq!(from_main.worktrees.len(), 1);
    assert_eq!(from_main.worktrees[0].root, first);
}

#[cfg(unix)]
#[test]
fn worktrees_that_cannot_be_checked_are_said_so_not_called_clean() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = fixture.outside();
    let linked = fixture.linked_worktree("odd");
    let reason_of = |worktree: &Path| {
        let survey = settings::other_worktrees(&fixture.layout).unwrap();
        survey
            .worktrees
            .into_iter()
            .find(|candidate| candidate.root == worktree)
            .unwrap()
            .state
    };
    assert_eq!(reason_of(&linked), WorktreeState::NotInstalled);

    // A `.claude` symlink is not followed, not even to read it.
    symlink(&outside, linked.join(".claude")).unwrap();
    fs::write(
        outside.join("settings.local.json"),
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"x ingest claude-code --hook"}]}]}}"#,
    )
    .unwrap();
    let WorktreeState::Unreadable(reason) = reason_of(&linked) else {
        panic!("a symlinked .claude must read as unreadable");
    };
    assert!(reason.contains("symlink"), "{reason}");
    fs::remove_file(linked.join(".claude")).unwrap();

    // Malformed, oversized and symlinked settings files likewise.
    fs::create_dir(linked.join(".claude")).unwrap();
    let file = linked.join(SETTINGS_RELATIVE_PATH);
    for (name, plant) in [
        (
            "malformed",
            Box::new(|file: &Path| fs::write(file, "{ not json").unwrap()) as Box<dyn Fn(&Path)>,
        ),
        (
            "oversized",
            Box::new(|file: &Path| {
                fs::write(file, vec![b' '; 4 * 1024 * 1024 + 16]).unwrap();
            }),
        ),
        (
            "symlinked",
            Box::new({
                let target = outside.join("settings.local.json");
                move |file: &Path| symlink(&target, file).unwrap()
            }),
        ),
    ] {
        plant(&file);
        assert!(
            matches!(reason_of(&linked), WorktreeState::Unreadable(_)),
            "{name}"
        );
        fs::remove_file(&file).unwrap();
    }
}

#[test]
fn a_main_worktree_that_cannot_be_located_is_reported() {
    // A repository whose Git directory is not its own `.git`
    // (`git init --separate-git-dir`) has no back-reference to its worktree
    // from a linked one, so the survey cannot find it and says so.
    let temp = TempDir::new().unwrap();
    let workdir = temp.path().join("main");
    let gitdir = temp.path().join("store");
    let linked = temp.path().join("linked");
    fs::create_dir(&workdir).unwrap();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=f@example.invalid",
            ])
            .args(args)
            .current_dir(&workdir)
            .env("HOME", temp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", temp.path().join("gitconfig"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    run(&["init", "--separate-git-dir", gitdir.to_str().unwrap(), "."]);
    run(&["commit", "--allow-empty", "-m", "initial"]);
    run(&["worktree", "add", linked.to_str().unwrap(), "-b", "linked"]);

    let main_layout = RepositoryLayout::discover(&workdir).unwrap();
    // A main worktree whose Git directory lives elsewhere is still the main one.
    assert!(!main_layout.is_linked_worktree());
    assert!(RepositoryLayout::discover(&linked)
        .unwrap()
        .is_linked_worktree());
    fs::create_dir_all(main_layout.cvc_dir()).unwrap();
    let from_main = settings::other_worktrees(&main_layout).unwrap();
    assert!(
        from_main.main_located,
        "the active worktree is the main one"
    );

    let linked_layout = RepositoryLayout::discover(&linked).unwrap();
    let from_linked = settings::other_worktrees(&linked_layout).unwrap();
    assert!(!from_linked.main_located);
    assert!(from_linked.worktrees.is_empty());
}
