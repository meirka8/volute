//! Support for the acceptance tests of the interactive `cvc conversations`
//! view (issue #112): a captured Claude Code session with subagents, a view on
//! a pseudo-terminal that records what is typed at it, and readers for what a
//! share, an unshare, a publish or an authorization leaves behind.
//!
//! Every read of that state goes around the view (the plain listing, `cvc
//! privacy status`, the index database, the remote's `refs/cvc/main`), so a
//! test never takes the view's word for what the view did.
//!
//! The including test crate declares `mod common;` as well: this module builds
//! on its fixture, pseudo-terminal and virtual screen.
#![allow(dead_code)]

use crate::common::{challenges, Fixture, PtySession, VirtualScreen};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

/// The window every test of the view opens its terminal with.
pub const COLUMNS: u16 = 120;
pub const ROWS: u16 = 40;

pub const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
pub const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";

/// The longest a test waits for the view to show something. Passing tests wait
/// for a fraction of it: the waits are on what the screen shows, never on time.
const WAIT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// The captured session
// ---------------------------------------------------------------------------

/// A Claude Code session that delegated to two subagents.
pub const SESSION: &str = "5e0c7a52-3a0a-4b1e-9c60-0d2a4f6b8c11";
pub const AGENT_A: &str = "agent-a17d3c0e5b2f49a81";
pub const AGENT_B: &str = "agent-b4f6e29c10d7a3582";

pub const SESSION_TITLE: &str = "Review the keymap";
pub const AGENT_A_TITLE: &str = "ux-psychologist: Design the keymap";
pub const AGENT_B_TITLE: &str = "security-reviewer: Audit the consent paths";

/// Thoughts (assistant responses) each conversation of the session holds.
pub const SESSION_THOUGHTS: usize = 3;
pub const AGENT_A_THOUGHTS: usize = 2;
pub const AGENT_B_THOUGHTS: usize = 1;

pub fn agent_a() -> String {
    format!("{SESSION}:{AGENT_A}")
}

pub fn agent_b() -> String {
    format!("{SESSION}:{AGENT_B}")
}

/// One user prompt and the one assistant response to it, as the lines Claude
/// Code writes to a transcript. A subagent's are sidechains with its id.
struct Exchange<'a> {
    agent: Option<&'a str>,
    /// Names the entries and the request: `u-<tag>`, `s-<tag>`, `req_<tag>`.
    tag: &'a str,
    /// The entry the prompt follows.
    parent: Option<&'a str>,
    asked_at: &'a str,
    answered_at: &'a str,
    prompt: &'a str,
    answer: &'a str,
}

impl Exchange<'_> {
    fn lines(&self, root: &Path) -> Vec<Value> {
        let tag = self.tag;
        let (user, assistant) = (format!("u-{tag}"), format!("s-{tag}"));
        let mut lines = vec![
            json!({
                "parentUuid": self.parent,
                "isSidechain": self.agent.is_some(),
                "type": "user",
                "message": {"role": "user", "content": self.prompt},
                "uuid": user,
                "timestamp": self.asked_at,
                "sessionId": SESSION,
                "version": "2.1.219",
                "cwd": root,
            }),
            json!({
                "parentUuid": user,
                "isSidechain": self.agent.is_some(),
                "requestId": format!("req_{tag}"),
                "type": "assistant",
                "message": {
                    "model": "claude-fixture-1",
                    "id": format!("msg_{tag}"),
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": self.answer}],
                    "stop_reason": "end_turn",
                    "stop_sequence": null,
                    "usage": {"input_tokens": 1, "output_tokens": 1},
                },
                "uuid": assistant,
                "timestamp": self.answered_at,
                "sessionId": SESSION,
                "version": "2.1.219",
                "cwd": root,
            }),
        ];
        if let Some(agent) = self.agent {
            for line in &mut lines {
                line["agentId"] = json!(agent.trim_start_matches("agent-"));
            }
        }
        lines
    }
}

fn write_jsonl(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, text).unwrap();
}

/// Captures the session through `cvc ingest claude-code`, the way a person
/// catches up on a finished session: three thoughts in the session itself, two
/// in the first subagent and one in the second.
///
/// The times are fixed, so the listing is the same on every run: the session
/// was last active at 22:10, the second agent at 22:08 and the first at 22:06,
/// the first agent was spawned before the second.
pub fn ingest_the_session(fixture: &Fixture) {
    ingest_a_session(fixture, SESSION_TITLE, "Design the keymap");
}

/// [`ingest_the_session`] with the text a harness (and so anything that can
/// steer the agent) chooses: the session's title, and the description that
/// titles the first subagent (`ux-psychologist: <description>`).
pub fn ingest_a_session(fixture: &Fixture, session_title: &str, agent_a_description: &str) {
    fixture.cvc_ok(&["init"]);
    // The acknowledgement is a typed TTY challenge; the library records it as
    // the challenge would.
    cvc_core::privacy::acknowledge_capture(&fixture.open_repo()).unwrap();
    let root = fs::canonicalize(&fixture.repo).unwrap();
    let directory = fixture.temp.path().join("transcripts");

    let mut main = Exchange {
        agent: None,
        tag: "m1",
        parent: None,
        asked_at: "2026-08-10T22:00:00.000Z",
        answered_at: "2026-08-10T22:00:05.000Z",
        prompt: "Plan the keymap review",
        answer: "Plan ready.",
    }
    .lines(&root);
    main.extend(
        Exchange {
            agent: None,
            tag: "m2",
            parent: Some("s-m1"),
            asked_at: "2026-08-10T22:01:00.000Z",
            answered_at: "2026-08-10T22:01:05.000Z",
            prompt: "Delegate the design and the audit",
            answer: "Delegating to two subagents.",
        }
        .lines(&root),
    );
    main.extend(
        Exchange {
            agent: None,
            tag: "m3",
            parent: Some("s-m2"),
            asked_at: "2026-08-10T22:10:00.000Z",
            answered_at: "2026-08-10T22:10:05.000Z",
            prompt: "Wrap up",
            answer: "Both reports are in.",
        }
        .lines(&root),
    );
    main.push(json!({"type": "ai-title", "aiTitle": session_title, "sessionId": SESSION}));
    let transcript = directory.join(format!("{SESSION}.jsonl"));
    write_jsonl(&transcript, &main);

    let subagents = directory.join(SESSION).join("subagents");
    let mut first = Exchange {
        agent: Some(AGENT_A),
        tag: "a1",
        parent: None,
        asked_at: "2026-08-10T22:02:00.000Z",
        answered_at: "2026-08-10T22:02:10.000Z",
        prompt: "Design the keymap.",
        answer: "Keymap drafted.",
    }
    .lines(&root);
    first.extend(
        Exchange {
            agent: Some(AGENT_A),
            tag: "a2",
            parent: Some("s-a1"),
            asked_at: "2026-08-10T22:06:00.000Z",
            answered_at: "2026-08-10T22:06:30.000Z",
            prompt: "Revise it.",
            answer: "Keymap revised.",
        }
        .lines(&root),
    );
    write_jsonl(&subagents.join(format!("{AGENT_A}.jsonl")), &first);
    fs::write(
        subagents.join(format!("{AGENT_A}.meta.json")),
        json!({"agentType": "ux-psychologist", "description": agent_a_description}).to_string(),
    )
    .unwrap();
    let second = Exchange {
        agent: Some(AGENT_B),
        tag: "b1",
        parent: None,
        asked_at: "2026-08-10T22:08:00.000Z",
        answered_at: "2026-08-10T22:08:20.000Z",
        prompt: "Audit the consent paths.",
        answer: "No findings.",
    }
    .lines(&root);
    write_jsonl(&subagents.join(format!("{AGENT_B}.jsonl")), &second);
    fs::write(
        subagents.join(format!("{AGENT_B}.meta.json")),
        r#"{"agentType":"security-reviewer","description":"Audit the consent paths"}"#,
    )
    .unwrap();

    let ingested = fixture.cvc_ok(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
    ]);
    assert!(
        ingested.contains(&format!("{SESSION_THOUGHTS} new thought(s)")),
        "{ingested}"
    );
    assert!(
        ingested.contains(&format!(
            "plus 2 subagent session(s): {} new thought(s)",
            AGENT_A_THOUGHTS + AGENT_B_THOUGHTS
        )),
        "{ingested}"
    );
}

/// The session goes on: a fourth thought is appended to its transcript and
/// captured, as it is when a person keeps working while the view is open.
pub fn capture_one_more_thought(fixture: &Fixture) {
    let root = fs::canonicalize(&fixture.repo).unwrap();
    let transcript = fixture
        .temp
        .path()
        .join("transcripts")
        .join(format!("{SESSION}.jsonl"));
    let more = Exchange {
        agent: None,
        tag: "m4",
        parent: Some("s-m3"),
        asked_at: "2026-08-10T22:20:00.000Z",
        answered_at: "2026-08-10T22:20:05.000Z",
        prompt: "One more thing",
        answer: "Done.",
    }
    .lines(&root);
    let mut text = fs::read_to_string(&transcript).unwrap();
    for line in &more {
        text.push_str(&format!("{line}\n"));
    }
    fs::write(&transcript, text).unwrap();
    let ingested = fixture.cvc_ok(&[
        "ingest",
        "claude-code",
        "--transcript",
        transcript.to_str().unwrap(),
    ]);
    assert!(ingested.contains("1 new thought(s)"), "{ingested}");
}

/// `cvc init` and one `cvc run` conversation (one thought); returns its id.
pub fn captured_conversation(fixture: &Fixture) -> String {
    fixture.cvc_ok(&["init"]);
    fixture.cvc_ok(&["run", "--", "printf", "captured"]);
    fixture.only_run_conversation_id()
}

/// A repository whose `origin` is authorized for publishing and that holds one
/// captured conversation.
pub fn authorized_with_a_conversation() -> (Fixture, String) {
    let fixture = Fixture::new();
    let id = captured_conversation(&fixture);
    fixture.consent_to_sharing();
    (fixture, id)
}

/// A second bare repository registered as the remote `second`, and its path.
/// Its destination is not the one of `origin`, so nothing authorized for one is
/// authorized for the other.
pub fn add_a_second_remote(fixture: &Fixture) -> std::path::PathBuf {
    let path = fixture.temp.path().join("second.git");
    git2::Repository::init_bare(&path).unwrap();
    fixture.git(&["remote", "add", "second", path.to_str().unwrap()]);
    // A remote with no refs at all trips a debug-build check in libgit2.
    fixture.git(&["push", "-q", "second", "HEAD:refs/heads/main"]);
    path
}

// ---------------------------------------------------------------------------
// The commands that set a state up
// ---------------------------------------------------------------------------

/// Shares `id` for `origin` with `cvc share`, typing the phrase it displays.
pub fn share_with_the_command(fixture: &Fixture, id: &str) {
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", id, "--remote", "origin"]),
        b"",
    );
    let phrase = session.wait_for_challenge(1);
    assert!(phrase.starts_with("I SHARE "), "{phrase}");
    session.write(format!("{phrase}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
}

/// The phrase `cvc share` asks for `id` and `origin`, learned by starting the
/// command and walking away from its prompt. It is a function of the thoughts
/// shared, so the view asks the very same one.
pub fn the_share_phrase(fixture: &Fixture, id: &str) -> String {
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["share", id, "--remote", "origin"]),
        b"",
    );
    session.wait_for_challenge(1)
}

/// Publishes what is shared for `origin` with `cvc push --manual`, typing the
/// phrase it displays.
pub fn publish_with_the_command(fixture: &Fixture) {
    let mut session = PtySession::spawn(
        fixture.cvc_command(&["push", "--manual", "--remote", "origin"]),
        b"",
    );
    let phrase = session.wait_for_challenge(1);
    assert!(phrase.starts_with("I PUBLISH "), "{phrase}");
    session.write(format!("{phrase}\n").as_bytes());
    let (status, output) = session.wait_exit();
    assert!(status.success(), "{output}");
}

// ---------------------------------------------------------------------------
// The state, read around the view
// ---------------------------------------------------------------------------

/// The destination fingerprint of the remote called `name`.
pub fn fingerprint(fixture: &Fixture, name: &str) -> String {
    cvc_core::privacy::remote_destination(&fixture.open_repo(), name)
        .unwrap()
        .fingerprint
}

/// What the index records of a conversation for one destination.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ShareRecord {
    /// Thoughts shared.
    pub shared_thoughts: i64,
    /// What happens to later thoughts: `None` when no share decision was ever
    /// recorded for the conversation and the destination.
    pub future: Option<bool>,
    /// The publication state of each thought the destination has a state for.
    pub publications: Vec<String>,
}

pub fn share_record(fixture: &Fixture, conversation: &str, fingerprint: &str) -> ShareRecord {
    let db = rusqlite::Connection::open(fixture.index_db_path()).unwrap();
    let shared_thoughts = db
        .query_row(
            "SELECT COUNT(*) FROM interaction_shares s \
             JOIN interactions i ON i.id = s.interaction_id \
             WHERE i.conversation_id = ?1 AND s.remote_fingerprint = ?2",
            [conversation, fingerprint],
            |row| row.get(0),
        )
        .unwrap();
    let future = db
        .query_row(
            "SELECT share_future FROM conversation_shares \
             WHERE conversation_id = ?1 AND remote_fingerprint = ?2",
            [conversation, fingerprint],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .map(|flag| flag != 0);
    let mut statement = db
        .prepare(
            "SELECT p.state FROM publications p \
             JOIN interactions i ON i.id = p.interaction_id \
             WHERE i.conversation_id = ?1 AND p.remote_fingerprint = ?2 ORDER BY p.state",
        )
        .unwrap();
    let publications = statement
        .query_map([conversation, fingerprint], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    ShareRecord {
        shared_thoughts,
        future,
        publications,
    }
}

/// `cvc privacy status --remote <name>`, a field of it.
pub fn privacy_field(fixture: &Fixture, remote: &str, field: &str) -> String {
    let status = fixture.cvc_ok(&["privacy", "status", "--remote", remote]);
    status
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{field}: ")))
        .unwrap_or_else(|| panic!("no {field} in:\n{status}"))
        .to_owned()
}

/// Whether publishing to `remote` is authorized in this clone.
pub fn is_authorized(fixture: &Fixture, remote: &str) -> bool {
    privacy_field(fixture, remote, "sharing_consented") == "true"
}

/// Whether `auto_push` is on for `remote`.
pub fn auto_push_is_on(fixture: &Fixture, remote: &str) -> bool {
    privacy_field(fixture, remote, "auto_push") == "true"
}

/// The plain listing, as a script gets it.
pub fn plain_listing(fixture: &Fixture) -> String {
    fixture.cvc_ok(&["conversations"])
}

/// The bracketed share state the plain listing gives `id`, such as `[private]`.
pub fn listed_state(fixture: &Fixture, id: &str) -> String {
    let listing = plain_listing(fixture);
    let line = listing
        .lines()
        .find(|line| line.split_whitespace().any(|word| word == id))
        .unwrap_or_else(|| panic!("{id} is not listed:\n{listing}"));
    line[line
        .rfind('[')
        .unwrap_or_else(|| panic!("no state in {line:?}"))..]
        .to_owned()
}

/// The conversations the remote's `refs/cvc/main` holds nodes of, with how many
/// each: `None` while the remote has no such ref.
pub fn published_conversations(bare_remote: &Path) -> Option<BTreeMap<String, usize>> {
    let remote = git2::Repository::open_bare(bare_remote).unwrap();
    let tree = remote
        .find_reference("refs/cvc/main")
        .ok()?
        .peel_to_commit()
        .unwrap()
        .tree()
        .unwrap();
    let mut held = BTreeMap::new();
    let Some(nodes) = tree.get_name("nodes") else {
        return Some(held);
    };
    let nodes = nodes.to_object(&remote).unwrap().peel_to_tree().unwrap();
    for shard in nodes.iter() {
        let shard = shard.to_object(&remote).unwrap().peel_to_tree().unwrap();
        for node in shard.iter() {
            let blob = node.to_object(&remote).unwrap().peel_to_blob().unwrap();
            let node: Value = serde_json::from_slice(blob.content()).unwrap();
            let conversation = node["interaction"]["conversation_id"]
                .as_str()
                .expect("a node names its conversation")
                .to_owned();
            *held.entry(conversation).or_insert(0) += 1;
        }
    }
    Some(held)
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// `cvc` as a person's terminal runs it: a terminal type that can redraw, a
/// UTF-8 locale, and no `NO_COLOR` from the developer's environment.
pub fn tui_command(fixture: &Fixture, args: &[&str]) -> Command {
    let mut command = fixture.cvc_command(args);
    command
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env_remove("NO_COLOR");
    command
}

/// The rows of a screen as one line of words: a needle that the view wrapped
/// onto a second row still matches, and so does one the table spaced out.
pub fn flat(rows: &[String]) -> String {
    rows.iter()
        .flat_map(|row| row.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The text of the overlay that is open (details, help, the picker) as one
/// line: the rows between its borders, with a wrapped value joined up.
pub fn overlay_text(session: &PtySession) -> String {
    let rows: Vec<String> = session
        .screen()
        .iter()
        .filter_map(|row| {
            let first = row.find('│')?;
            let last = row.rfind('│')?;
            (last > first).then(|| row[first + '│'.len_utf8()..last].to_owned())
        })
        .collect();
    flat(&rows)
}

/// Whether the terminal shows the alternate screen (where the view draws), and
/// the rows of whichever screen it shows.
pub fn screen_state(session: &PtySession) -> (bool, Vec<String>) {
    let mut screen = VirtualScreen::new(COLUMNS as usize, ROWS as usize);
    screen.feed(&session.output());
    (screen.on_alternate_screen(), screen.rows())
}

/// What a failing wait shows: the screen as it is, and the end of the output.
pub fn describe(session: &PtySession) -> String {
    let mut text = String::from("--- the screen ---\n");
    for row in session.screen().iter().filter(|row| !row.is_empty()) {
        text.push_str(row);
        text.push('\n');
    }
    let output = session.output();
    let start = output
        .char_indices()
        .rev()
        .nth(500)
        .map_or(0, |(index, _)| index);
    text.push_str(&format!(
        "--- the end of the output ---\n{:?}",
        &output[start..]
    ));
    text
}

/// Waits until `ready` says so, failing with the screen if the process ends
/// first or `WAIT` passes.
pub fn wait_until(
    session: &mut PtySession,
    what: &str,
    mut ready: impl FnMut(&PtySession) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        if ready(session) {
            return;
        }
        if session.has_exited() {
            // What was written last may not have been read yet.
            std::thread::sleep(Duration::from_millis(150));
            if ready(session) {
                return;
            }
            panic!("the process ended before {what}:\n{}", describe(session));
        }
        assert!(
            Instant::now() < deadline,
            "timed out before {what}:\n{}",
            describe(session)
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Waits until the output has stopped growing. The view writes a frame from the
/// top of the screen to the bottom, in several pieces, and the terminal hands
/// them over as they come: a screen read the moment its first row shows can
/// still lack the rows under it, on a machine busy enough to hold the view
/// between two pieces. Three polls without a new byte take that for done.
pub fn wait_for_the_frame(session: &mut PtySession) {
    let deadline = Instant::now() + WAIT;
    let mut last = session.output().len();
    let mut calm = 0;
    while calm < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(15));
        let now = session.output().len();
        if now == last {
            calm += 1;
        } else {
            calm = 0;
            last = now;
        }
    }
}

/// A key, by what a person presses.
#[derive(Clone, Copy, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Up,
    Down,
    Left,
    Right,
    CtrlC,
}

impl Key {
    fn bytes(self) -> Vec<u8> {
        match self {
            Key::Char(c) => c.to_string().into_bytes(),
            // In raw mode Enter arrives as a carriage return; a line feed is
            // Ctrl-J.
            Key::Enter => b"\r".to_vec(),
            Key::Esc => b"\x1b".to_vec(),
            Key::Up => b"\x1b[A".to_vec(),
            Key::Down => b"\x1b[B".to_vec(),
            Key::Right => b"\x1b[C".to_vec(),
            Key::Left => b"\x1b[D".to_vec(),
            Key::CtrlC => vec![0x03],
        }
    }

    fn name(self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::Enter => "Enter".to_owned(),
            Key::Esc => "Esc".to_owned(),
            Key::Up => "Up".to_owned(),
            Key::Down => "Down".to_owned(),
            Key::Left => "Left".to_owned(),
            Key::Right => "Right".to_owned(),
            Key::CtrlC => "Ctrl-C".to_owned(),
        }
    }
}

/// What a phrase prompt is answered with, in a script of what was typed.
pub const A_PHRASE: &str = "<phrase>";

/// The interactive view on a pseudo-terminal, together with a record of
/// everything typed at it, so that a test can say what was *not* typed.
pub struct View {
    pub session: PtySession,
    script: Vec<String>,
    phrases: Vec<String>,
}

impl View {
    /// Opens `cvc conversations` and waits for the first frame.
    pub fn open(fixture: &Fixture) -> Self {
        Self::open_with(fixture, &["conversations"])
    }

    pub fn open_with(fixture: &Fixture, args: &[&str]) -> Self {
        let session = PtySession::spawn_sized(tui_command(fixture, args), b"", COLUMNS, ROWS);
        Self::attach(session)
    }

    /// Takes over a session of the view that is already started, and waits for
    /// its first frame.
    pub fn attach(session: PtySession) -> Self {
        let mut view = Self {
            session,
            script: Vec::new(),
            phrases: Vec::new(),
        };
        view.wait_for("CVC conversations");
        view
    }

    /// The screen as one line of words.
    pub fn flat(&self) -> String {
        flat(&self.session.screen())
    }

    /// Waits until the view shows `needle` (words separated by single spaces)
    /// on its screen, and returns the screen as one line.
    ///
    /// Only the view's own (alternate) screen counts. A flow prints on the main
    /// screen, sometimes the very words the view shows afterwards, and a key
    /// sent at that moment would reach a view that is not reading keys yet.
    pub fn wait_for(&mut self, needle: &str) -> String {
        wait_until(
            &mut self.session,
            &format!("{needle:?} on the view's screen"),
            |s| {
                let (on_alternate, rows) = screen_state(s);
                on_alternate && flat(&rows).contains(needle)
            },
        );
        wait_for_the_frame(&mut self.session);
        self.flat()
    }

    /// Waits until the view is up and its screen no longer shows `needle`.
    pub fn wait_until_gone(&mut self, needle: &str) {
        wait_until(
            &mut self.session,
            &format!("{needle:?} to leave the view's screen"),
            |s| {
                let (on_alternate, rows) = screen_state(s);
                on_alternate && !flat(&rows).contains(needle)
            },
        );
        wait_for_the_frame(&mut self.session);
    }

    /// Waits until the main screen, where a flow prints and asks, shows
    /// `needle`, and returns it as one line.
    pub fn wait_for_main_screen(&mut self, needle: &str) -> String {
        wait_until(
            &mut self.session,
            &format!("{needle:?} on the main screen"),
            |s| {
                let (on_alternate, rows) = screen_state(s);
                !on_alternate && flat(&rows).contains(needle)
            },
        );
        wait_for_the_frame(&mut self.session);
        self.flat()
    }

    /// Lets `window` pass, failing if the view ends meanwhile.
    pub fn wait_quietly(&mut self, window: Duration) {
        let end = Instant::now() + window;
        while Instant::now() < end {
            assert!(
                !self.session.has_exited(),
                "the view ended:\n{}",
                describe(&self.session)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Presses a key. `Esc` is sent alone: the caller waits for what it does
    /// before the next key, or a terminal reads the two as one Alt chord.
    pub fn press(&mut self, key: Key) {
        self.script.push(key.name());
        self.session.write(&key.bytes());
    }

    /// Presses a letter.
    pub fn key(&mut self, c: char) {
        self.press(Key::Char(c));
    }

    /// Waits for the `n`th phrase prompt and returns the phrase it asks.
    pub fn prompt(&mut self, n: usize) -> String {
        wait_until(&mut self.session, &format!("phrase prompt #{n}"), |s| {
            challenges(&s.output()).len() >= n
        });
        challenges(&self.session.output())[n - 1].clone()
    }

    /// Types `phrase` back as the prompt displays it.
    pub fn type_back(&mut self, phrase: &str) {
        self.script.push(A_PHRASE.to_owned());
        self.phrases.push(phrase.to_owned());
        self.session.write(format!("{phrase}\n").as_bytes());
    }

    /// Waits for the `n`th prompt and types its phrase back.
    pub fn type_phrase(&mut self, n: usize) -> String {
        let phrase = self.prompt(n);
        self.type_back(&phrase);
        phrase
    }

    /// Waits for the `n`th prompt and answers it with `answer(phrase)` (a wrong
    /// phrase, or an empty line); returns the phrase it asked.
    pub fn answer_with(&mut self, n: usize, answer: impl FnOnce(&str) -> String) -> String {
        let phrase = self.prompt(n);
        let answer = answer(&phrase);
        self.script.push(format!("<answer: {answer:?}>"));
        self.session.write(format!("{answer}\n").as_bytes());
        phrase
    }

    /// What was typed so far: keys by name, phrases as [`A_PHRASE`].
    pub fn script(&self) -> Vec<&str> {
        self.script.iter().map(String::as_str).collect()
    }

    /// The phrases typed back, as displayed.
    pub fn phrases_typed(&self) -> &[String] {
        &self.phrases
    }

    /// How many phrase prompts have been displayed so far.
    pub fn prompts_so_far(&self) -> usize {
        challenges(&self.session.output()).len()
    }

    /// Proves the view has taken every key sent so far and is waiting at the
    /// list: `?` opens the help from the list and from nowhere else.
    pub fn is_at_the_list(&mut self) {
        self.key('?');
        self.wait_for("private --s--> shared");
        self.key('?');
        self.wait_until_gone("private --s--> shared");
    }

    /// Quits with `q` from the list and returns how the process ended.
    pub fn quit(&mut self) -> ExitStatus {
        self.key('q');
        self.session.wait_for_exit()
    }
}

/// The terminal is as a shell leaves it: line editing and echo are on, the
/// alternate screen is left, the cursor is shown and bracketed paste is off.
pub fn assert_terminal_restored(session: &PtySession) {
    assert!(session.terminal_is_cooked(), "raw mode was left on");
    let output = session.output();
    let mut screen = VirtualScreen::new(COLUMNS as usize, ROWS as usize);
    screen.feed(&output);
    assert!(
        !screen.on_alternate_screen(),
        "the alternate screen was not left:\n{}",
        describe(session)
    );
    for (off, on, what) in [
        ("\x1b[?25h", "\x1b[?25l", "the cursor was left hidden"),
        ("\x1b[?2004l", "\x1b[?2004h", "bracketed paste was left on"),
    ] {
        if let Some(turned_on) = output.rfind(on) {
            assert!(
                output.rfind(off).is_some_and(|at| at > turned_on),
                "{what}:\n{output:?}"
            );
        }
    }
}

/// The terminal is in the view's modes: raw, on the alternate screen.
pub fn assert_view_is_up(session: &PtySession) {
    assert!(!session.terminal_is_cooked(), "the view is not in raw mode");
    let mut screen = VirtualScreen::new(COLUMNS as usize, ROWS as usize);
    screen.feed(&session.output());
    assert!(screen.on_alternate_screen(), "{}", describe(session));
}

/// The terminal is where a flow prints and asks: cooked, on the main screen.
pub fn assert_on_the_main_screen(session: &PtySession) {
    assert!(session.terminal_is_cooked(), "the terminal is still raw");
    let mut screen = VirtualScreen::new(COLUMNS as usize, ROWS as usize);
    screen.feed(&session.output());
    assert!(
        !screen.on_alternate_screen(),
        "a flow runs on the alternate screen:\n{}",
        describe(session)
    );
}
