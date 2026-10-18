//! Shared fixtures for tests that drive the `cvc` binary: an isolated
//! repository with a local bare remote, and a pseudo-terminal session that
//! makes the binary's consent challenges answerable from a test.
#![allow(dead_code)]

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const WAIT_LIMIT: Duration = Duration::from_secs(60);

pub struct Fixture {
    pub temp: TempDir,
    pub home: PathBuf,
    pub repo: PathBuf,
    /// A bare repository registered as `origin`, so publication can be
    /// exercised end to end without a network.
    pub remote: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let remote = temp.path().join("remote.git");
        fs::create_dir(&home).unwrap();
        fs::create_dir(home.join("templates")).unwrap();
        fs::create_dir(&repo).unwrap();
        git2::Repository::init_bare(&remote).unwrap();
        let fixture = Self {
            temp,
            home,
            repo,
            remote,
        };
        fixture.git(&["init"]);
        fixture.git(&["config", "user.email", "test@example.invalid"]);
        fixture.git(&["config", "user.name", "CVC test"]);
        let remote = fixture.remote.to_str().unwrap().to_owned();
        fixture.git(&["remote", "add", "origin", &remote]);
        fs::write(fixture.repo.join("tracked"), "initial\n").unwrap();
        fixture.git(&["add", "tracked"]);
        fixture.git(&["commit", "-m", "initial"]);
        // Advertise one ref: git2's `Remote::list` trips a debug-build
        // pointer check on a remote with no refs at all.
        fixture.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
        fixture
    }

    /// A command isolated from the developer's own Git and CVC configuration.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("empty.gitconfig"))
            .env("GIT_TEMPLATE_DIR", self.home.join("templates"))
            .stdin(Stdio::null());
        command
    }

    pub fn git(&self, args: &[&str]) {
        let output = self.command("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub fn cvc_command(&self, args: &[&str]) -> Command {
        let mut command = self.command(env!("CARGO_BIN_EXE_cvc"));
        command.args(args);
        command
    }

    pub fn cvc(&self, args: &[&str]) -> Output {
        self.cvc_command(args).output().unwrap()
    }

    pub fn cvc_ok(&self, args: &[&str]) -> String {
        let output = self.cvc(args);
        assert!(
            output.status.success(),
            "cvc {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    pub fn open_repo(&self) -> git2::Repository {
        git2::Repository::open(&self.repo).unwrap()
    }

    /// Grants destination sharing consent for `origin` directly through the
    /// library, the way the interactive challenge would have recorded it.
    pub fn consent_to_sharing(&self) -> cvc_core::privacy::RemoteDestination {
        let repo = self.open_repo();
        let destination = cvc_core::privacy::remote_destination(&repo, "origin").unwrap();
        cvc_core::privacy::acknowledge_sharing_destination(&repo, &destination).unwrap();
        destination
    }

    /// Acknowledges auto-push for `origin` directly through the library.
    /// Requires sharing consent.
    pub fn enable_auto_push(&self) {
        let repo = self.open_repo();
        let destination = cvc_core::privacy::remote_destination(&repo, "origin").unwrap();
        cvc_core::privacy::set_auto_push_destination(&repo, &destination, true).unwrap();
    }

    /// The id of the single `cvc run` conversation captured so far.
    pub fn only_run_conversation_id(&self) -> String {
        let ids = self.run_conversation_ids();
        assert_eq!(ids.len(), 1, "{ids:?}");
        ids[0].clone()
    }

    /// The ids of every `cvc run` conversation captured so far, most recent
    /// first (the order of `cvc conversations`).
    pub fn run_conversation_ids(&self) -> Vec<String> {
        self.cvc_ok(&["conversations"])
            .split_whitespace()
            .filter(|token| token.starts_with("run-"))
            .map(str::to_owned)
            .collect()
    }

    /// The SQLite index shared by the repository's worktrees.
    pub fn index_db_path(&self) -> PathBuf {
        self.open_repo().commondir().join("cvc").join("index.db")
    }
}

/// A child process whose stdin, stdout, and stderr are the slave side of a
/// pseudo-terminal that is also its controlling terminal, exactly as when a
/// human runs it from a shell.
pub struct PtySession {
    child: Child,
    master: File,
    output: Arc<Mutex<Vec<u8>>>,
    /// The window the pty was opened with, columns by rows.
    size: (u16, u16),
}

impl PtySession {
    /// Spawns `command` on a fresh pty. `queued_input` is written to the
    /// terminal *before* the process starts, which is what type-ahead or a
    /// paste looks like from the process's side.
    ///
    /// The pty has no window size (0 by 0), which is enough for everything that
    /// prompts line by line; use [`PtySession::spawn_sized`] for a full-screen
    /// program.
    pub fn spawn(command: Command, queued_input: &[u8]) -> Self {
        Self::spawn_on(command, queued_input, None)
    }

    /// As [`PtySession::spawn`], on a pty whose window is `columns` by `rows`
    /// cells (set with TIOCSWINSZ before the process starts, as a terminal
    /// emulator does), so that a program asking for its size gets an answer.
    pub fn spawn_sized(command: Command, queued_input: &[u8], columns: u16, rows: u16) -> Self {
        Self::spawn_on(command, queued_input, Some((columns, rows)))
    }

    fn spawn_on(mut command: Command, queued_input: &[u8], size: Option<(u16, u16)>) -> Self {
        let (mut master, slave) = openpty(size);
        master.write_all(queued_input).unwrap();
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // SAFETY: the hook only calls async-signal-safe syscalls that make the
        // pty the child's controlling terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        // Dropping the command releases the parent's slave descriptors, so
        // the master sees hangup once the child exits.
        drop(command);
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().unwrap();
        let sink = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink.lock().unwrap().extend_from_slice(&chunk[..n]),
                }
            }
        });
        Self {
            child,
            master,
            output,
            size: size.unwrap_or((80, 24)),
        }
    }

    /// Types `bytes` at the terminal.
    pub fn write(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    /// Everything the process has shown so far, including echoed input.
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            let output = self.output();
            if done(&output) {
                return output;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                // Give the reader a moment to drain what was written last.
                std::thread::sleep(Duration::from_millis(100));
                let output = self.output();
                if done(&output) {
                    return output;
                }
                panic!("process exited ({status}) before {what}:\n{output}");
            }
            assert!(
                Instant::now() < deadline,
                "timed out before {what}:\n{}",
                self.output()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until the `n`th typed-acknowledgement prompt has been displayed
    /// and returns the challenge it demands.
    pub fn wait_for_challenge(&mut self, n: usize) -> String {
        let output = self.wait_until(&format!("challenge #{n}"), |output| {
            challenges(output).len() >= n
        });
        challenges(&output)[n - 1].clone()
    }

    /// Waits until `needle` has been displayed.
    pub fn wait_for_text(&mut self, needle: &str) -> String {
        self.wait_until(&format!("{needle:?}"), |output| output.contains(needle))
    }

    /// Everything the process has shown so far with the terminal control
    /// sequences removed: the text a person would read, in the order it was
    /// written (cursor movement is not replayed).
    pub fn plain_output(&self) -> String {
        strip_ansi(&self.output())
    }

    /// Waits until `needle` has been displayed as plain text, and returns the
    /// plain output so far. A full-screen program draws word by word with
    /// cursor movements in between, so the plain output of one can lack its
    /// spaces; use [`PtySession::wait_for_screen_text`] for what is on screen.
    pub fn wait_for_plain_text(&mut self, needle: &str) -> String {
        self.wait_until(&format!("{needle:?}"), |output| {
            strip_ansi(output).contains(needle)
        });
        self.plain_output()
    }

    /// The rows of the terminal as they look now: everything the process has
    /// written, replayed on a virtual screen the size of the pty's window.
    pub fn screen(&self) -> Vec<String> {
        let (columns, rows) = self.size;
        let mut screen = VirtualScreen::new(columns as usize, rows as usize);
        screen.feed(&self.output());
        screen.rows()
    }

    /// Waits until a row of the screen contains `needle` and returns the rows.
    pub fn wait_for_screen_text(&mut self, needle: &str) -> Vec<String> {
        let (columns, rows) = self.size;
        self.wait_until(&format!("{needle:?} on the screen"), |output| {
            let mut screen = VirtualScreen::new(columns as usize, rows as usize);
            screen.feed(output);
            screen.rows().iter().any(|row| row.contains(needle))
        });
        self.screen()
    }

    /// Waits for exit and returns the status with the full terminal output.
    pub fn wait_exit(mut self) -> (ExitStatus, String) {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                // Drop our master handle only after the reader thread has had
                // the chance to see the hangup and drain the last output.
                std::thread::sleep(Duration::from_millis(100));
                return (status, self.output());
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("process did not exit:\n{}", self.output());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Every challenge string prompted so far, in display order.
pub fn challenges(output: &str) -> Vec<String> {
    const LEAD: &str = "Type exactly '";
    const TRAIL: &str = "' to continue: ";
    output
        .match_indices(LEAD)
        .filter_map(|(at, _)| {
            let rest = &output[at + LEAD.len()..];
            rest.find(TRAIL).map(|end| rest[..end].to_owned())
        })
        .collect()
}

/// `output` without ANSI escape sequences: CSI sequences (colors, cursor
/// movement, modes such as `?1049h`), OSC strings, and two-byte escapes.
pub fn strip_ansi(output: &str) -> String {
    let mut plain = String::with_capacity(output.len());
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then a final byte 0x40..=0x7e.
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ST (ESC \).
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            // Any other escape is two bytes.
            _ => {}
        }
    }
    plain
}

/// A small virtual terminal: enough of VT100/xterm to replay what a
/// full-screen program wrote (cursor addressing, erasing, SGR, the alternate
/// screen, scrolling) and read back the rows a person would see.
pub struct VirtualScreen {
    columns: usize,
    rows: usize,
    main: Grid,
    alternate: Option<Grid>,
    saved_cursor: (usize, usize),
}

struct Grid {
    cells: Vec<Vec<char>>,
    row: usize,
    column: usize,
}

impl Grid {
    fn new(columns: usize, rows: usize) -> Self {
        Self {
            cells: vec![vec![' '; columns]; rows],
            row: 0,
            column: 0,
        }
    }
}

impl VirtualScreen {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            columns,
            rows,
            main: Grid::new(columns, rows),
            alternate: None,
            saved_cursor: (0, 0),
        }
    }

    fn grid(&mut self) -> &mut Grid {
        self.alternate.as_mut().unwrap_or(&mut self.main)
    }

    /// The rows now on screen, without trailing spaces.
    pub fn rows(&self) -> Vec<String> {
        let grid = self.alternate.as_ref().unwrap_or(&self.main);
        grid.cells
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|&&c| c != '\0')
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// Whether the alternate screen is in use.
    pub fn on_alternate_screen(&self) -> bool {
        self.alternate.is_some()
    }

    pub fn feed(&mut self, output: &str) {
        let mut chars = output.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\u{1b}' => match chars.next() {
                    Some('[') => {
                        let mut private = false;
                        let mut params = String::new();
                        let mut last = None;
                        for next in chars.by_ref() {
                            match next {
                                '?' | '>' | '<' | '=' => private = true,
                                '0'..='9' | ';' | ':' => params.push(next),
                                '\u{20}'..='\u{2f}' => {}
                                _ => {
                                    last = Some(next);
                                    break;
                                }
                            }
                        }
                        if let Some(last) = last {
                            self.csi(private, &params, last);
                        }
                    }
                    Some(']') => {
                        while let Some(next) = chars.next() {
                            if next == '\u{7}' {
                                break;
                            }
                            if next == '\u{1b}' {
                                chars.next_if_eq(&'\\');
                                break;
                            }
                        }
                    }
                    Some('7') => {
                        let grid = self.grid();
                        self.saved_cursor = (grid.row, grid.column);
                    }
                    Some('8') => {
                        let (row, column) = self.saved_cursor;
                        let grid = self.grid();
                        grid.row = row;
                        grid.column = column;
                    }
                    _ => {}
                },
                '\r' => self.grid().column = 0,
                '\n' => self.line_feed(),
                '\u{8}' => {
                    let grid = self.grid();
                    grid.column = grid.column.saturating_sub(1);
                }
                '\t' => {
                    let columns = self.columns;
                    let grid = self.grid();
                    grid.column = ((grid.column / 8 + 1) * 8).min(columns - 1);
                }
                c if c.is_control() => {}
                c => self.print(c),
            }
        }
    }

    fn line_feed(&mut self) {
        let rows = self.rows;
        let columns = self.columns;
        let grid = self.grid();
        if grid.row + 1 >= rows {
            grid.cells.remove(0);
            grid.cells.push(vec![' '; columns]);
        } else {
            grid.row += 1;
        }
    }

    fn print(&mut self, c: char) {
        let width = match c as u32 {
            0x1100..=0x115f
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1faff => 2,
            0x0300..=0x036f | 0x200b..=0x200f | 0xfe00..=0xfe0f => 0,
            _ => 1,
        };
        if width == 0 {
            return;
        }
        let columns = self.columns;
        if self.grid().column + width > columns {
            self.grid().column = 0;
            self.line_feed();
        }
        let grid = self.grid();
        grid.cells[grid.row][grid.column] = c;
        if width == 2 {
            grid.cells[grid.row][grid.column + 1] = '\0';
        }
        grid.column += width;
    }

    fn csi(&mut self, private: bool, params: &str, last: char) {
        let numbers: Vec<usize> = params
            .split(';')
            .map(|p| p.parse::<usize>().unwrap_or(0))
            .collect();
        let first = |default: usize| match numbers.first() {
            Some(&n) if n > 0 => n,
            _ => default,
        };
        let (columns, rows) = (self.columns, self.rows);
        if private {
            // Only the alternate screen matters here: the other private modes
            // (cursor visibility, bracketed paste) change nothing on screen.
            let alternate = numbers.iter().any(|n| matches!(n, 47 | 1047 | 1049));
            match last {
                'h' if alternate && self.alternate.is_none() => {
                    let grid = self.grid();
                    self.saved_cursor = (grid.row, grid.column);
                    self.alternate = Some(Grid::new(columns, rows));
                }
                'l' if alternate && self.alternate.take().is_some() => {
                    let (row, column) = self.saved_cursor;
                    self.main.row = row;
                    self.main.column = column;
                }
                _ => {}
            }
            return;
        }
        match last {
            'H' | 'f' => {
                let row = first(1) - 1;
                let column = numbers.get(1).copied().filter(|&n| n > 0).unwrap_or(1) - 1;
                let grid = self.grid();
                grid.row = row.min(rows - 1);
                grid.column = column.min(columns - 1);
            }
            'A' => {
                let n = first(1);
                let grid = self.grid();
                grid.row = grid.row.saturating_sub(n);
            }
            'B' => {
                let n = first(1);
                let grid = self.grid();
                grid.row = (grid.row + n).min(rows - 1);
            }
            'C' => {
                let n = first(1);
                let grid = self.grid();
                grid.column = (grid.column + n).min(columns - 1);
            }
            'D' => {
                let n = first(1);
                let grid = self.grid();
                grid.column = grid.column.saturating_sub(n);
            }
            'G' => {
                let n = first(1);
                self.grid().column = (n - 1).min(columns - 1);
            }
            'd' => {
                let n = first(1);
                self.grid().row = (n - 1).min(rows - 1);
            }
            'J' => {
                let mode = numbers.first().copied().unwrap_or(0);
                let grid = self.grid();
                let (row, column) = (grid.row, grid.column);
                match mode {
                    0 => {
                        for c in column..columns {
                            grid.cells[row][c] = ' ';
                        }
                        for r in row + 1..rows {
                            grid.cells[r] = vec![' '; columns];
                        }
                    }
                    1 => {
                        for r in 0..row {
                            grid.cells[r] = vec![' '; columns];
                        }
                        for c in 0..=column.min(columns - 1) {
                            grid.cells[row][c] = ' ';
                        }
                    }
                    2 => {
                        for r in 0..rows {
                            grid.cells[r] = vec![' '; columns];
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = numbers.first().copied().unwrap_or(0);
                let grid = self.grid();
                let (row, column) = (grid.row, grid.column);
                match mode {
                    0 => (column..columns).for_each(|c| grid.cells[row][c] = ' '),
                    1 => (0..=column.min(columns - 1)).for_each(|c| grid.cells[row][c] = ' '),
                    2 => grid.cells[row] = vec![' '; columns],
                    _ => {}
                }
            }
            's' => {
                let grid = self.grid();
                self.saved_cursor = (grid.row, grid.column);
            }
            'u' => {
                let (row, column) = self.saved_cursor;
                let grid = self.grid();
                grid.row = row;
                grid.column = column;
            }
            _ => {}
        }
    }
}

fn openpty(size: Option<(u16, u16)>) -> (File, File) {
    let mut master = -1;
    let mut slave = -1;
    let mut window = size.map(|(columns, rows)| libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });
    let window_pointer = window
        .as_mut()
        .map_or(std::ptr::null_mut(), |window| window as *mut libc::winsize);
    // SAFETY: both out-pointers are valid, and the window pointer is null or
    // points to a live `winsize`; the other optional arguments may be null.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            window_pointer,
        )
    };
    assert_eq!(rc, 0, "openpty: {}", io::Error::last_os_error());
    // SAFETY: openpty returned two fresh descriptors that we now own.
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}
