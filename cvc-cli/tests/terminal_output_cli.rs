#![cfg(unix)]

//! Text that did not come from a person typing at the terminal cannot act on
//! it. A title is whatever the harness or the first prompt line said (a Claude
//! Code `ai-title`, a subagent description), secret-scrubbed and nothing more,
//! so a prompt-injected agent can put escape sequences in one. Printed raw, they
//! redraw the "Sharing conversation for remote 'origin':" line above the
//! `I SHARE` prompt. The text of an error, which can be a remote server's, is
//! the same kind of text, and so are the prompts and responses `cvc log` shows,
//! the warnings of the Git hooks, the names of remotes and what `cvc ingest`
//! reports of a transcript.

mod common;

use common::{Fixture, PtySession};
use rusqlite::{params, Connection};
use std::io::Write;
use std::process::{Output, Stdio};

/// Cursor up, erase the line, set the window title, ring the bell, reverse the
/// text that follows, return to the start of the line, and then forge the line
/// above the prompt.
const HOSTILE_TITLE: &str = "Fix the build\u{1b}[1A\u{1b}[2K\u{1b}]0;pwned\u{7}\u{202e}\rSharing conversation for remote 'origin':";

/// What the title above reads as once its control and format characters are
/// replaced.
const NEUTRALIZED_TITLE: &str =
    "Fix the build?[1A?[2K?]0;pwned???Sharing conversation for remote 'origin':";

/// Whether `text` holds nothing that acts on a terminal. A line feed is how a
/// line ends, and a pseudo-terminal turns it into a carriage return and a line
/// feed on the way out, so that pair counts as one line feed. The set is spelled
/// out here and not taken from the code under test.
fn is_inert(text: &str) -> bool {
    text.replace("\r\n", "\n").chars().all(|c| {
        c == '\n'
            || !(c.is_control()
                || matches!(
                    c,
                    '\u{ad}'
                        | '\u{61c}'
                        | '\u{180e}'
                        | '\u{200b}'..='\u{200f}'
                        | '\u{2028}'..='\u{202e}'
                        | '\u{2060}'..='\u{2069}'
                        | '\u{feff}'
                        | '\u{fff9}'..='\u{fffb}'
                        | '\u{e0001}'..='\u{e007f}'
                ))
    })
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// One captured conversation whose title is the hostile one.
fn conversation_with_hostile_title(fixture: &Fixture) -> String {
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    let id = fixture.only_run_conversation_id();
    Connection::open(fixture.index_db_path())
        .unwrap()
        .execute(
            "UPDATE conversations SET title = ?1 WHERE id = ?2",
            params![HOSTILE_TITLE, id],
        )
        .unwrap();
    id
}

#[test]
fn the_plain_listing_prints_a_hostile_title_as_inert_text_on_one_line() {
    let fixture = Fixture::new();
    let id = conversation_with_hostile_title(&fixture);

    let output = fixture.cvc(&["conversations", "--plain"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let listing = stdout(&output);
    assert!(is_inert(&listing), "{listing:?}");
    let entry: Vec<_> = listing
        .lines()
        .skip_while(|line| !line.contains(&id))
        .take(2)
        .collect();
    assert_eq!(entry.len(), 2, "{listing:?}");
    assert_eq!(
        entry[1],
        format!("      {NEUTRALIZED_TITLE}"),
        "{listing:?}"
    );
    // The forged line did not become a line of its own.
    assert_eq!(
        listing.matches("Sharing conversation for remote").count(),
        1,
        "{listing:?}"
    );
}

#[test]
fn the_share_picker_prints_a_hostile_title_as_inert_text() {
    let fixture = Fixture::new();
    conversation_with_hostile_title(&fixture);

    let mut session = PtySession::spawn(fixture.cvc_command(&["share", "--remote", "origin"]), b"");
    let shown = session.wait_for_text("Share which conversation?");
    session.write(b"\n");
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
    assert!(is_inert(&output), "{output:?}");
    assert!(shown.contains(NEUTRALIZED_TITLE), "{shown:?}");
}

#[test]
fn the_summary_before_the_share_challenge_cannot_be_rewritten_by_a_title() {
    let fixture = Fixture::new();
    let id = conversation_with_hostile_title(&fixture);

    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", &id, "--remote", "origin"]),
        b"",
    );
    let challenge = session.wait_for_challenge(1);
    assert!(challenge.starts_with("I SHARE "), "{challenge}");
    session.write(b"no\n");
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output}");
    assert!(is_inert(&output), "{output:?}");
    let summary = output.replace("\r\n", "\n");
    let lines: Vec<_> = summary.lines().collect();
    let header = lines
        .iter()
        .position(|line| *line == "Sharing conversation for remote 'origin':")
        .unwrap_or_else(|| panic!("no summary header: {output:?}"));
    assert!(lines[header + 1].contains(&id), "{output:?}");
    assert_eq!(
        lines[header + 2],
        format!("      {NEUTRALIZED_TITLE}"),
        "{output:?}"
    );
    // The forgery is not a line of its own either.
    assert_eq!(
        summary.matches("Sharing conversation for remote").count(),
        2,
        "the real one and the neutralized title: {output:?}"
    );
    // Nothing was shared.
    let listing = fixture.cvc_ok(&["conversations", "--plain"]);
    assert!(listing.contains("[private]"), "{listing}");
}

#[test]
fn a_phrase_with_a_control_character_in_it_is_not_asked() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);

    let session = PtySession::spawn(
        fixture.cvc_command(&["share", "c\u{1b}[2J-1", "--remote", "origin"]),
        b"",
    );
    let (status, output) = session.wait_exit();
    assert!(!status.success(), "{output:?}");
    assert!(is_inert(&output), "{output:?}");
    assert!(output.contains("cannot be shown"), "{output:?}");
    assert!(output.contains("nothing was recorded"), "{output:?}");
    assert!(!output.contains("Type exactly"), "{output:?}");
}

#[test]
fn an_error_that_quotes_hostile_text_prints_it_as_inert_text() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);

    // The parser of the interaction id quotes the first character it refused.
    let output = fixture.cvc(&["delete-local", "\u{1b}[2J\u{1b}]0;pwned\u{7}\u{202e}x"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(is_inert(&message), "{message:?}");
    assert!(message.contains("invalid interaction id"), "{message:?}");
    assert!(message.contains("found `?` at 1"), "{message:?}");
}

#[test]
fn an_advisory_hook_warning_prints_hostile_text_as_inert_text_and_still_succeeds() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);

    // clap quotes the argument it did not recognize. It strips escape sequences
    // and controls from its messages itself; a bidirectional override it does not.
    let output = fixture.cvc(&["hook", "bogus\u{202e}x"]);
    assert!(
        output.status.success(),
        "a hook never fails the git operation"
    );
    let message = stderr(&output);
    assert!(message.starts_with("CVC Hook Warning: advisory hook skipped: "));
    assert!(is_inert(&message), "{message:?}");
    assert!(message.contains("'bogus?x'"), "{message:?}");
}

/// An erase, an OSC 52 clipboard write ended by BEL, a right-to-left override
/// and a line separator: a piece of text a prompt, a response, a transcript or
/// a remote can carry.
const HOSTILE_PIECE: &str = "\u{1b}[2J\u{1b}]52;c;SGVsbG8=\u{7}\u{202e}\u{2028}";

/// [`HOSTILE_PIECE`] once its control and format characters are replaced.
const NEUTRALIZED_PIECE: &str = "?[2J?]52;c;SGVsbG8=???";

#[test]
fn the_log_prints_the_first_line_of_a_hostile_prompt_and_response_as_inert_text() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    let prompt = format!("Fix the build{HOSTILE_PIECE}now\nthe prompt's second line");
    let response = format!("Done{HOSTILE_PIECE}\r\u{1b}[1Aok\nthe response's second line");
    let updated = Connection::open(fixture.index_db_path())
        .unwrap()
        .execute(
            "UPDATE interactions SET user_prompt = ?1, model_response = ?2",
            params![prompt, response],
        )
        .unwrap();
    assert_eq!(updated, 1);

    let log = fixture.cvc_ok(&["log"]);
    assert!(is_inert(&log), "{log:?}");
    let lines: Vec<_> = log.lines().collect();
    let prompt_line = format!("Prom: Fix the build{NEUTRALIZED_PIECE}now...");
    let response_line = format!("Resp: Done{NEUTRALIZED_PIECE}??[1Aok...");
    assert!(lines.contains(&prompt_line.as_str()), "{log:?}");
    assert!(lines.contains(&response_line.as_str()), "{log:?}");
    // Only the first line of each, as before.
    assert!(!log.contains("second line"), "{log:?}");
}

/// Points `origin` at a repository that is not there, with [`HOSTILE_PIECE`]
/// in its path. libgit2 quotes that path in the error it fails with, which is
/// how the text of a transport error reaches what a hook prints. Returns the
/// path as a warning shows it.
fn origin_at_a_path_that_quotes_hostile_text(fixture: &Fixture) -> String {
    let path = format!("{}/gone{HOSTILE_PIECE}.git", fixture.temp.path().display());
    fixture.git(&["remote", "set-url", "origin", &format!("file://{path}")]);
    path.replace(HOSTILE_PIECE, NEUTRALIZED_PIECE)
}

#[test]
fn a_hook_warning_prints_the_text_of_a_transport_error_as_inert_text_and_succeeds() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let shown = origin_at_a_path_that_quotes_hostile_text(&fixture);

    // `git merge` and `git pull` run the post-merge hook, which pulls.
    let output = fixture.cvc(&["hook", "post-merge", "0"]);
    assert!(
        output.status.success(),
        "a hook never fails the git operation"
    );
    let message = stderr(&output);
    assert!(is_inert(&message), "{message:?}");
    assert!(is_inert(&stdout(&output)), "{output:?}");
    let warning = message
        .lines()
        .find(|line| line.starts_with("CVC: post-merge pull skipped ("))
        .unwrap_or_else(|| panic!("no pull warning: {message:?}"));
    assert!(warning.contains(&format!("'{shown}'")), "{message:?}");

    // `git push` runs the pre-push hook, which publishes where auto-push is on.
    fixture.consent_to_sharing();
    fixture.enable_auto_push();
    let output = fixture.cvc(&["hook", "pre-push", "origin", "file:///unused"]);
    assert!(
        output.status.success(),
        "a hook never fails the git operation"
    );
    let message = stderr(&output);
    assert!(is_inert(&message), "{message:?}");
    let warning = message
        .lines()
        .find(|line| line.starts_with("CVC: sync skipped ("))
        .unwrap_or_else(|| panic!("no sync warning: {message:?}"));
    assert!(warning.contains(&format!("'{shown}'")), "{message:?}");
}

#[test]
fn an_error_that_quotes_a_url_does_not_print_its_user_information() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let path = format!("{}/gone.git", fixture.temp.path().display());
    fixture.git(&[
        "remote",
        "set-url",
        "origin",
        &format!("file://deploy:tok-3xample@localhost{path}"),
    ]);
    let shown = format!("'file://localhost{path}'");

    let output = fixture.cvc(&["pull"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains(&shown), "{message:?}");
    assert!(!message.contains("tok-3xample"), "{message:?}");
    assert!(!message.contains("deploy"), "{message:?}");

    let output = fixture.cvc(&["hook", "post-merge", "0"]);
    assert!(output.status.success());
    let message = stderr(&output);
    assert!(
        message.starts_with("CVC: post-merge pull skipped ("),
        "{message:?}"
    );
    assert!(message.contains(&shown), "{message:?}");
    assert!(!message.contains("tok-3xample"), "{message:?}");
    assert!(!message.contains("deploy"), "{message:?}");
}

/// A remote's name that Git and libgit2 take: an erase and an OSC 52 clipboard
/// write in their C1 forms (CSI and OSC as one character each, the OSC ended by
/// ST), a right-to-left override and a line separator. libgit2 resolves a remote
/// only by a name that passes Git's ref-name rules, which refuse ASCII controls
/// and none of these, so a name `cvc` can resolve never holds ESC or BEL.
const HOSTILE_REMOTE: &str = "up\u{9b}2J\u{9d}52;c;SGVsbG8=\u{9c}st\u{202e}re\u{2028}am";

/// [`HOSTILE_REMOTE`] as every message shows it.
const NEUTRALIZED_REMOTE: &str = "up?2J?52;c;SGVsbG8=?st?re?am";

#[test]
fn messages_that_name_a_remote_print_a_hostile_name_as_inert_text() {
    let fixture = Fixture::new();
    fixture.cvc_ok(&["init"]);
    let bare = fixture.remote.to_str().unwrap().to_owned();
    fixture.git(&["config", &format!("remote.{HOSTILE_REMOTE}.url"), &bare]);
    let fingerprint = cvc_core::privacy::remote_destination(&fixture.open_repo(), HOSTILE_REMOTE)
        .unwrap()
        .fingerprint;

    let status = fixture.cvc_ok(&["privacy", "status", "--remote", HOSTILE_REMOTE]);
    assert_eq!(
        status,
        format!(
            "capture_acknowledged: false\nsharing_consented: false\nauto_push: false\nremote: {NEUTRALIZED_REMOTE}\ndestination_fingerprint: {fingerprint}\n"
        )
    );

    // Inside `git push`, Git passes the remote's name to the pre-push hook.
    let output = fixture.cvc(&["hook", "pre-push", HOSTILE_REMOTE, &bare]);
    assert!(output.status.success());
    assert_eq!(
        stderr(&output),
        format!("CVC: auto publication disabled for '{NEUTRALIZED_REMOTE}'.\n")
    );

    let reconciled = fixture.cvc_ok(&["privacy", "reconcile", "--remote", HOSTILE_REMOTE]);
    assert_eq!(
        reconciled,
        format!("Reconciled publication state for '{NEUTRALIZED_REMOTE}'.\n")
    );

    // Without `origin`, `cvc pull` and the post-merge hook pull from the
    // repository's first remote, and name it.
    fixture.git(&["remote", "remove", "origin"]);
    let pulled = format!("Pulled 0 interaction(s) from '{NEUTRALIZED_REMOTE}'.\n");
    assert_eq!(fixture.cvc_ok(&["pull"]), pulled);
    let output = fixture.cvc(&["hook", "post-merge", "0"]);
    assert!(output.status.success());
    assert_eq!(stdout(&output), pulled);
    assert!(is_inert(&stderr(&output)), "{output:?}");

    // Git's configuration takes ESC and BEL in a name too, but libgit2 does not
    // resolve such a remote: it is reported as not configured, without its name.
    let refused = format!("ev{HOSTILE_PIECE}il");
    fixture.git(&["config", &format!("remote.{refused}.url"), &bare]);
    let output = fixture.cvc(&["privacy", "status", "--remote", &refused]);
    assert!(!output.status.success());
    assert_eq!(stderr(&output), "remote is not configured\n");
    let output = fixture.cvc(&["hook", "pre-push", &refused, &bare]);
    assert!(output.status.success());
    assert_eq!(
        stderr(&output),
        "CVC: sync skipped (remote is not configured)\n"
    );
}

const BASIC: &str = include_str!("../../cvc-core/tests/fixtures/claude-code/v2.1.260/basic.jsonl");
const SESSION: &str = "7c1e9f5a-2b2f-4f4e-9c3b-0f2e7a1d5b60";

/// The fixture transcript of [`SESSION`], recorded in the fixture's
/// repository, with `rewrite` applied to its text; capture is acknowledged.
fn transcript(fixture: &Fixture, rewrite: impl Fn(String) -> String) -> String {
    fixture.cvc_ok(&["init"]);
    cvc_core::privacy::acknowledge_capture(&fixture.open_repo()).unwrap();
    let worktree = std::fs::canonicalize(&fixture.repo).unwrap();
    let path = fixture.temp.path().join(format!("{SESSION}.jsonl"));
    let content = BASIC.replace("__WORKTREE__", worktree.to_str().unwrap());
    std::fs::write(&path, rewrite(content)).unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn ingest_reports_a_hostile_block_type_as_inert_text() {
    let fixture = Fixture::new();
    // The `type` of the transcript's one reasoning block, [`HOSTILE_PIECE`] in
    // the JSON escapes a transcript would carry it in.
    let path = transcript(&fixture, |content| {
        content.replace(
            r#""type":"thinking""#,
            r#""type":"mys\u001b[2J\u001b]52;c;SGVsbG8=\u0007\u202e\u2028tery""#,
        )
    });

    let printed = fixture.cvc_ok(&["ingest", "claude-code", "--transcript", &path]);
    assert!(is_inert(&printed), "{printed:?}");
    assert!(
        printed.starts_with(&format!("CVC: Claude Code session {SESSION}: ")),
        "{printed:?}"
    );
    let placeholders = format!(
        "  1 content block(s) of a type this version does not record were kept as placeholders: mys{NEUTRALIZED_PIECE}tery"
    );
    assert!(
        printed.lines().any(|line| line == placeholders),
        "{printed:?}"
    );
}

#[test]
fn ingest_refuses_a_hostile_session_id_as_inert_text_and_hook_mode_keeps_stdout_empty() {
    let fixture = Fixture::new();
    let path = transcript(&fixture, |content| content);
    let session = format!("s{HOSTILE_PIECE}1");
    // How the refusal quotes the id: escaped, so it is inert already.
    let refusal = "session id \"s\\u{1b}[2J\\u{1b}]52;c;SGVsbG8=\\u{7}\\u{202e}\\u{2028}1\" is not a safe identifier\n";

    let output = fixture.cvc(&[
        "ingest",
        "claude-code",
        "--transcript",
        &path,
        "--session",
        &session,
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(stderr(&output), refusal);

    // Hook mode: the id comes in the payload Claude Code writes on stdin.
    let payload = serde_json::json!({
        "session_id": session,
        "transcript_path": path,
        "cwd": fixture.repo,
        "hook_event_name": "Stop",
    })
    .to_string();
    let mut child = fixture
        .cvc_command(&["ingest", "claude-code", "--hook"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "a visible failure, never the blocking status 2"
    );
    assert!(
        output.stdout.is_empty(),
        "hook mode prints nothing on stdout: {output:?}"
    );
    assert_eq!(stderr(&output), refusal);
    // Nothing of the session was recorded.
    let recorded: i64 = Connection::open(fixture.index_db_path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(recorded, 0);
}

#[test]
fn a_command_line_mistake_keeps_the_lines_of_its_usage() {
    let fixture = Fixture::new();

    let output = fixture.cvc(&["nonsense"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(is_inert(&message), "{message:?}");
    assert!(message.lines().count() >= 3, "{message:?}");
    assert!(message.contains("\nUsage: cvc "), "{message:?}");
    assert!(!message.contains('?'), "{message:?}");
}
