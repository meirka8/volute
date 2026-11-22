//! Every committed fixture under `cvc-core/tests/fixtures/`, and the
//! Reviewer's shared test data (`cvc-reviewer/src/test/*.ts`), is checked for
//! what the sanitising checklist in `fixtures/README.md` forbids:
//!
//! - a string the built-in scrubber would change, line by line and in the
//!   whole text (vendor-token shapes, private keys, also split across lines,
//!   and credential-keyed JSON values); a scrub that fails, or whose JSON no
//!   longer parses, is a finding too;
//! - a personal path, other than an invented placeholder name: `/home/<name>`,
//!   `/Users/<name>`, `/var/home/<name>`, `/export/home/<name>`,
//!   `/mnt/c/Users/<name>`, `C:\Users\<name>` or `C:/Users/<name>` (`Users`
//!   in any case), also as a `file://` URL, percent-encoded or JSON-escaped;
//! - an e-mail address outside `example.invalid` and `example.com`, which
//!   includes an SSH-style `git@host:` address;
//! - a file that is not UTF-8 text, or is a link;
//! - in a harness's directory, a subdirectory that is not named
//!   `v<agent version>` or a file other than `README.md`.
//!
//! It is a safety net, not a guarantee: the scrubber finds only what it has a
//! detector for, and a personal name in prose is not looked for. A fixture
//! that fails here is fixed by sanitising the fixture. A rule is changed only
//! when it is wrong for every fixture, never to let one through.

use cvc_core::harness::HarnessId;
use cvc_core::privacy::PreparedPolicy;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

/// Directories under a harness's directory that are not agent versions, each
/// documented in `fixtures/README.md`.
const NOT_VERSIONS: &[(&str, &str)] = &[
    // The characterisation goldens of `test_claude_code_goldens.rs`.
    ("claude-code", "goldens"),
];

/// User names a fixture may put in a home path because they are invented.
/// `dev` is the Reviewer's search fixtures' documented placeholder
/// (`/home/dev/work/shop`).
const PLACEHOLDER_USERS: &[&str] = &["someone", "user", "example", "dev", "__HOME__", "__USER__"];

/// Lines the scrubber rule does not read, each exact and with why: code whose
/// variable names match a detector, not data. Keep it short.
const SCRUB_EXCEPTIONS: &[(&str, &str)] = &[
    // The generic `token = <value>` detector matches the variable that holds
    // a computed boundary token.
    (
        "cvc-reviewer/src/test/stimulusWriter.ts",
        "const token = boundaryToken(id, shape, candidate);",
    ),
];

/// Whether line `line` of `path` is one of the [`SCRUB_EXCEPTIONS`].
fn scrub_exception(path: &Path, line: &str) -> bool {
    SCRUB_EXCEPTIONS
        .iter()
        .any(|(file, exact)| path == Path::new(file) && line.trim() == *exact)
}

/// Mail domains that are reserved for examples.
const EXAMPLE_MAIL_DOMAINS: &[&str] = &["example.invalid", "example.com"];

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every file below `dir`, sorted, with paths relative to the fixtures root.
fn files_below(dir: &Path, into: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.unwrap())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let kind = entry.file_type().unwrap();
        let path = entry.path();
        assert!(
            !kind.is_symlink(),
            "{}: a fixture is never a link",
            path.display()
        );
        if kind.is_dir() {
            files_below(&path, into);
        } else {
            into.push(path);
        }
    }
}

/// The Reviewer's shared test data (`cvc-reviewer/src/test/*.ts`), which is
/// committed to the same public repository and is read by the same rules.
fn reviewer_test_data() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cvc-reviewer/src/test");
    let Ok(entries) = fs::read_dir(&dir) else {
        // A crate packaged without the rest of the repository.
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "ts"))
        .collect();
    paths.sort();
    paths
}

/// Every file the content rules read, with a path to report it by: the
/// fixtures (relative to the fixtures root) and the Reviewer's test data.
fn fixtures() -> Vec<(PathBuf, String)> {
    let root = fixtures_root();
    let mut paths = Vec::new();
    files_below(&root, &mut paths);
    assert!(!paths.is_empty(), "no fixtures under {}", root.display());
    let reviewer = reviewer_test_data();
    paths
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(&root).unwrap().to_path_buf();
            (path, relative)
        })
        .chain(reviewer.into_iter().map(|path| {
            let shown = Path::new("cvc-reviewer/src/test").join(path.file_name().unwrap());
            (path, shown)
        }))
        .map(|(path, shown)| {
            let bytes = fs::read(&path).unwrap();
            let text = String::from_utf8(bytes).unwrap_or_else(|_| {
                panic!(
                    "{}: not UTF-8 text; a fixture is text (a database is a SQL dump)",
                    shown.display()
                )
            });
            (shown, text)
        })
        .collect()
}

/// `path:line: what` for each finding, so a failure names every problem.
fn report(findings: &[String]) {
    assert!(
        findings.is_empty(),
        "{} fixture hygiene problem(s); see cvc-core/tests/fixtures/README.md:\n{}",
        findings.len(),
        findings.join("\n")
    );
}

/// What the scrubber would do to `line`, or `None` when it keeps it.
fn scrub_finding(policy: &PreparedPolicy, line: &str) -> Option<String> {
    match policy.scrub(line) {
        Ok(scrubbed) if scrubbed == line => {}
        Ok(_) => return Some("the built-in scrubber would change this line".into()),
        Err(error) => return Some(format!("the built-in scrubber refuses this line: {error}")),
    }
    // A JSON line is also scrubbed as JSON, as tool arguments are: values
    // under a credential-shaped key are replaced too.
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    match policy.scrub_json(line) {
        // Fails closed: output that is no longer JSON is not "unchanged".
        Ok(scrubbed) => match serde_json::from_str::<serde_json::Value>(&scrubbed) {
            Ok(scrubbed) => (scrubbed != value)
                .then(|| "the built-in JSON scrubber would change a value in this line".into()),
            Err(_) => Some("the built-in JSON scrubber's output for this line is not JSON".into()),
        },
        Err(error) => Some(format!(
            "the built-in JSON scrubber refuses this line: {error}"
        )),
    }
}

/// What the scrubber would do to a file's whole text, or `None` when it keeps
/// it; a refusal (a text too large to scrub) is a finding too.
fn whole_text_finding(policy: &PreparedPolicy, text: &str) -> Option<String> {
    match policy.scrub(text) {
        Ok(scrubbed) if scrubbed == text => None,
        Ok(_) => {
            Some("the built-in scrubber would change the file's text (across its lines)".into())
        }
        Err(error) => Some(format!(
            "the built-in scrubber refuses the file's text: {error}"
        )),
    }
}

#[test]
fn no_fixture_holds_anything_the_built_in_scrubber_would_change() {
    let policy = PreparedPolicy::built_ins_only();
    let mut findings = Vec::new();
    for (path, text) in fixtures() {
        for (number, line) in text.lines().enumerate() {
            if scrub_exception(&path, line) {
                continue;
            }
            if let Some(what) = scrub_finding(&policy, line) {
                findings.push(format!("{}:{}: {what}", path.display(), number + 1));
            }
        }
        // The whole text too, for what spans its lines (a PEM key, say),
        // with the excepted lines left empty.
        let read: String = text
            .split_inclusive('\n')
            .map(|line| {
                if scrub_exception(&path, line) {
                    "\n"
                } else {
                    line
                }
            })
            .collect();
        if let Some(what) = whole_text_finding(&policy, &read) {
            findings.push(format!("{}: {what}", path.display()));
        }
        // And a whole JSON document as JSON.
        if serde_json::from_str::<serde_json::Value>(&text).is_ok() && text.lines().count() > 1 {
            let document: serde_json::Value = serde_json::from_str(&text).unwrap();
            let compact = document.to_string();
            if let Some(what) = scrub_finding(&policy, &compact) {
                findings.push(format!("{}: {what} (as one document)", path.display()));
            }
        }
    }
    report(&findings);
}

fn home_paths() -> Regex {
    // The name after a home directory:
    // - at the root of a POSIX path (`/home`, `/Users`, `/var/home`,
    //   `/export/home`), alone or after `file://`, but not inside another path
    //   such as `/srv/home`;
    // - under a WSL mount of a Windows drive (`/mnt/c/Users`);
    // - on a Windows drive, with either slash (`C:\Users`, `C:/Users`).
    // `Users` in any letter case. Matched on normalised text (see
    // `normalised`), so escaped and percent-encoded forms count too.
    Regex::new(
        r#"(?x)
        (?:
            (?: ^ | [^A-Za-z0-9._~/-] | file:// )
            (?: (?: /var | /export )? /home | /(?i:users) | /mnt/[A-Za-z]/(?i:users) ) /
          | [A-Za-z]: [\\/]+ (?i:users) [\\/]+
        )
        ([^/\\\s"'`<>:;,()\[\]{}]+)"#,
    )
    .unwrap()
}

/// `text` with percent-encoding decoded (as in a `file://` URL) and the JSON
/// escapes of slashes undone (`\/` and `\\`), so that a path cannot pass the
/// path rule by being spelled differently.
fn normalised(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                decoded.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded)
        .replace(r"\/", "/")
        .replace(r"\\", r"\")
}

fn personal_paths(text: &str) -> Vec<String> {
    home_paths()
        .captures_iter(&normalised(text))
        .map(|found| found[1].to_owned())
        .filter(|name| !PLACEHOLDER_USERS.contains(&name.as_str()))
        .collect()
}

#[test]
fn no_fixture_holds_a_personal_path() {
    let mut findings = Vec::new();
    for (path, text) in fixtures() {
        for (number, line) in text.lines().enumerate() {
            for name in personal_paths(line) {
                findings.push(format!(
                    "{}:{}: a home path of `{name}`; use __WORKTREE__, __HOME__ or a placeholder",
                    path.display(),
                    number + 1
                ));
            }
        }
    }
    report(&findings);
}

fn mail_addresses(text: &str) -> Vec<String> {
    let address =
        Regex::new(r"[A-Za-z0-9._%+-]+@([A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,})").unwrap();
    address
        .captures_iter(text)
        .filter(|found| {
            let domain = found[1].to_ascii_lowercase();
            !EXAMPLE_MAIL_DOMAINS
                .iter()
                .any(|allowed| domain == *allowed || domain.ends_with(&format!(".{allowed}")))
        })
        .map(|found| found[0].to_owned())
        .collect()
}

#[test]
fn no_fixture_holds_a_real_looking_mail_address() {
    let mut findings = Vec::new();
    for (path, text) in fixtures() {
        for (number, line) in text.lines().enumerate() {
            for address in mail_addresses(line) {
                findings.push(format!(
                    "{}:{}: the mail address `{address}`; use one at example.invalid",
                    path.display(),
                    number + 1
                ));
            }
        }
    }
    report(&findings);
}

fn is_version_directory(name: &str) -> bool {
    Regex::new(r"^v[0-9]+(\.[0-9]+)*(\.x)?$")
        .unwrap()
        .is_match(name)
}

#[test]
fn every_harness_fixture_directory_is_named_for_an_agent_version() {
    let root = fixtures_root();
    let mut findings = Vec::new();
    for harness in HarnessId::ALL {
        let dir = root.join(harness.name());
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type().unwrap().is_dir() {
                // A README beside the versions says where they came from.
                if name != "README.md" {
                    findings.push(format!(
                        "{}/{name}: a file directly under a harness; put it in v<agent version>/",
                        harness.name()
                    ));
                }
                continue;
            }
            if is_version_directory(&name) || NOT_VERSIONS.contains(&(harness.name(), &name)) {
                continue;
            }
            findings.push(format!(
                "{}/{name}/: not named v<agent version>",
                harness.name()
            ));
        }
    }
    report(&findings);
}

// ---- the rules themselves --------------------------------------------------------------

/// A string shaped like a vendor's token, joined at run time so that no
/// committed file holds one in a piece.
fn token_shaped() -> String {
    ["gh", "p_", &"A1b2C3d4E5".repeat(4)[..36]].concat()
}

#[test]
fn the_rules_catch_what_they_are_for() {
    let policy = PreparedPolicy::built_ins_only();
    // The scrubber rule.
    let line = format!(
        r#"{{"type":"user","text":"export TOKEN={}"}}"#,
        token_shaped()
    );
    assert!(scrub_finding(&policy, &line).is_some(), "{line}");
    let line = format!(r#"{{"api_key":"{}"}}"#, "plain-words-only");
    assert!(
        scrub_finding(&policy, &line).is_some(),
        "a credential-keyed value: {line}"
    );
    let private_key = [
        "-----BEGIN ",
        "PRIVATE KEY-----",
        "MIIBVQIBADANBgkqhkiG9w0BAQEFAASCAT8wggE7AgEAAkEA",
        "-----END ",
        "PRIVATE KEY-----",
    ]
    .concat();
    assert!(scrub_finding(&policy, &private_key).is_some());
    // Split across lines, no one line holds it; the whole text does.
    let split_key = [
        "-----BEGIN ",
        "PRIVATE KEY-----\n",
        "MIIBVQIBADANBgkqhkiG9w0BAQEFAASCAT8wggE7AgEAAkEA\n",
        "-----END ",
        "PRIVATE KEY-----\n",
    ]
    .concat();
    assert!(whole_text_finding(&policy, &split_key).is_some());
    assert_eq!(whole_text_finding(&policy, "line one\nline two\n"), None);
    assert_eq!(
        scrub_finding(&policy, r#"{"cwd":"__WORKTREE__","text":"Add a greeting"}"#),
        None
    );

    // The path rule.
    assert_eq!(personal_paths("/home/alice/src/x.rs"), vec!["alice"]);
    assert_eq!(personal_paths("cd /Users/bob/work"), vec!["bob"]);
    assert_eq!(personal_paths(r#""C:\\Users\\carol\\x""#), vec!["carol"]);
    assert_eq!(personal_paths(r"C:\Users\dave\x"), vec!["dave"]);
    assert!(personal_paths("/home/someone/x /Users/user/y /home/__HOME__/z").is_empty());
    assert!(personal_paths("__WORKTREE__/src/lib.rs and /srv/home/x").is_empty());
    // As a file URL, percent-encoded, or with JSON-escaped slashes.
    assert_eq!(personal_paths("file:///home/erin/notes.md"), vec!["erin"]);
    assert_eq!(personal_paths("see file:///Users/frank/x"), vec!["frank"]);
    assert_eq!(personal_paths("file:///home/gr%61ce/x"), vec!["grace"]);
    assert_eq!(
        personal_paths("file:%2F%2F%2Fhome%2Fheidi%2Fx"),
        vec!["heidi"]
    );
    assert_eq!(personal_paths(r#""\/home\/ivan\/x""#), vec!["ivan"]);
    // WSL, forward slashes and a lower-case `users` on Windows.
    assert_eq!(personal_paths("/mnt/c/Users/judy/x"), vec!["judy"]);
    assert_eq!(personal_paths("C:/Users/karl/x"), vec!["karl"]);
    assert_eq!(personal_paths(r"c:\users\liam\x"), vec!["liam"]);
    assert_eq!(personal_paths("file:///C:/Users/mona/x"), vec!["mona"]);
    assert_eq!(personal_paths("cd /users/nick"), vec!["nick"]);
    // Other home roots.
    assert_eq!(personal_paths("/var/home/olga/x"), vec!["olga"]);
    assert_eq!(personal_paths("/export/home/pete/x"), vec!["pete"]);

    // The mail rule.
    assert_eq!(
        mail_addresses("from alice@example.org and dev@corp.example.invalid"),
        vec!["alice@example.org"]
    );
    assert!(mail_addresses("test@example.invalid, ci@example.com, a@b").is_empty());
    assert!(mail_addresses("git@github.com:owner/repo.git").len() == 1);

    // The directory rule.
    for name in ["v2.1.260", "v1", "v0.140.x"] {
        assert!(is_version_directory(name), "{name}");
    }
    for name in ["2.1.260", "legacy-0.140", "v2.1.260-beta", "goldens", "v"] {
        assert!(!is_version_directory(name), "{name}");
    }
}
