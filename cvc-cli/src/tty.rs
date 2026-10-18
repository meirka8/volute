//! Direct, unbuffered access to the controlling terminal for typed
//! acknowledgements.
//!
//! The consent challenges exist to prove that a human consciously approved
//! *this specific* irreversible action after seeing the prompt. Reading the
//! answer from the process-global `std::io::stdin()` cannot prove that:
//! `Stdin` is a shared 8 KiB `BufReader`, so an earlier `read_line` (the
//! interactive conversation picker, or a preceding challenge) may already have
//! pulled the *next* line into userspace. A later challenge would then be
//! answered instantly from that stale line, before its prompt was ever
//! displayed. `tcflush` alone cannot fix that either: it drains the kernel
//! queue but has no reach into Rust's buffer.
//!
//! This module therefore talks to the terminal itself, with three layers:
//!
//! 1. the terminal's pending input is discarded immediately before the prompt
//!    is written, so the answer is provably typed *after* the prompt;
//! 2. the prompt is not written until the input has been quiet for a moment
//!    (see [`Terminal::discard_pending_input`]): a flush alone does not catch
//!    input that was sent earlier and is still on its way, such as a paste that
//!    a terminal emulator has not finished writing or a slow link; and
//! 3. the answer is read one byte at a time from an unbuffered handle, so a
//!    challenge never over-reads into the line meant for the next one.
//!
//! Roughly one syscall per typed character is immaterial for a prompt.

use std::fmt;
use std::io::{self, IsTerminal, Write};

#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// Upper bound on one answer line. Every challenge is far shorter; anything
/// longer is not a human typing an acknowledgement.
#[cfg(unix)]
const MAX_ANSWER_BYTES: usize = 4096;

/// How long the terminal's input must have been quiet before a prompt is
/// written.
#[cfg(unix)]
const QUIET_BEFORE_PROMPT: Duration = Duration::from_millis(250);

/// How long input may keep arriving before the prompt is refused instead.
#[cfg(unix)]
const QUIET_WAIT_LIMIT: Duration = Duration::from_secs(3);

/// What was typed at a prompt cannot be taken as an answer at all: it is not
/// UTF-8 text, it is longer than any acknowledgement, or input would not stop
/// arriving long enough to show the prompt. It travels as the source of an
/// `InvalidData` I/O error, so the I/O error type stays the prompt's only one,
/// and [`is_unreadable_answer`] tells it from a failure of the terminal itself.
#[derive(Debug)]
pub enum UnreadableAnswer {
    /// The bytes typed are not valid UTF-8.
    NotUtf8,
    /// The line is longer than the longest answer read (4096 bytes).
    #[cfg(unix)]
    TooLong,
    /// Input kept arriving, without a quiet moment, for longer than the prompt
    /// waits for one, so it was not shown: whatever is sending it, a person
    /// answering after seeing it cannot be told from it.
    #[cfg(unix)]
    InputKeptArriving,
}

impl fmt::Display for UnreadableAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 => f.write_str("acknowledgement answer is not valid UTF-8"),
            #[cfg(unix)]
            Self::TooLong => f.write_str("acknowledgement answer exceeds the maximum line length"),
            #[cfg(unix)]
            Self::InputKeptArriving => {
                f.write_str("input kept arriving at the terminal, so the prompt was not shown")
            }
        }
    }
}

impl std::error::Error for UnreadableAnswer {}

impl UnreadableAnswer {
    fn into_io_error(self) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, self)
    }
}

/// Whether `error` came from [`Terminal::prompt_line`] being typed something
/// that is not an answer (see [`UnreadableAnswer`]), and not from the terminal
/// failing. The caller treats the first as an answer that does not match: the
/// challenge is not confirmed, which is the fail-closed outcome.
pub fn is_unreadable_answer(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<UnreadableAnswer>())
}

/// Whether `error` says that input kept arriving until the prompt was given up
/// on (see [`UnreadableAnswer::InputKeptArriving`]).
pub fn is_input_kept_arriving(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error
            .get_ref()
            .and_then(|source| source.downcast_ref::<UnreadableAnswer>())
            .is_some_and(|answer| matches!(answer, UnreadableAnswer::InputKeptArriving))
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Whether a typed acknowledgement can be protected from type-ahead: whether
/// [`Terminal::discard_pending_input`] really discards what was queued before
/// the prompt, and the answer is read unbuffered from the terminal itself.
/// That is Unix only; on other platforms the fallback reads the shared, buffered
/// stdin, so a pasted or typed-ahead line could answer a challenge that was
/// never displayed.
pub const fn protects_challenges_from_type_ahead() -> bool {
    cfg!(unix)
}

/// Whether stdin and stdout are both terminals: the precondition for anything
/// that asks a human to type, whether a consent challenge, the conversation
/// picker, or the interactive conversations view. It is one definition so
/// those gates cannot drift apart.
pub fn stdio_is_terminal() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// A handle on the process's controlling terminal.
pub struct Terminal {
    #[cfg(unix)]
    file: File,
}

impl Terminal {
    /// Opens the controlling terminal. Callers are expected to have already
    /// established that stdin and stdout are terminals; this fails only when
    /// the process has no controlling terminal at all.
    #[cfg(unix)]
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        Ok(Self { file })
    }

    /// Wraps an already-open terminal device (a pty slave in tests).
    #[cfg(all(unix, test))]
    fn from_file(file: File) -> Self {
        Self { file }
    }

    /// Discards every byte queued on the terminal's input that has not been
    /// read yet, once, and returns at once.
    #[cfg(unix)]
    pub fn flush_input(&mut self) -> io::Result<()> {
        // SAFETY: `tcflush` only takes a valid open descriptor and a queue
        // selector; it neither retains the descriptor nor touches memory.
        if unsafe { libc::tcflush(self.file.as_raw_fd(), libc::TCIFLUSH) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Discards the terminal's pending input, and keeps discarding it until
    /// none has arrived for a quarter of a second, so that nothing sent before
    /// the prompt can answer it. Call it right before [`Terminal::prompt_line`]
    /// writes the prompt.
    ///
    /// A flush alone leaves a gap: input that was sent before the prompt and
    /// reaches the terminal after the flush (a terminal emulator whose write was
    /// blocked on a full buffer, a slow remote link) would be read as the
    /// answer to a prompt that had only just been shown, and a phrase that can
    /// be computed in advance would pass. A control character in such a stream
    /// can make it worse: the line-kill character erases a partial line, so
    /// filler followed by it and then the phrase leaves nothing but the phrase.
    ///
    /// The wait is done with line editing and echo off, because in canonical
    /// mode the terminal shows a reader only complete lines: a partial line is
    /// invisible to it, and a stream without a line feed would look quiet. Those
    /// settings come back before this returns, on every path.
    ///
    /// If input does not go quiet within three seconds the prompt is refused:
    /// the error is [`UnreadableAnswer::InputKeptArriving`], which callers treat
    /// as a challenge that was not confirmed.
    #[cfg(unix)]
    pub fn discard_pending_input(&mut self) -> io::Result<()> {
        self.discard_input_until_quiet(QUIET_BEFORE_PROMPT, QUIET_WAIT_LIMIT)
    }

    /// [`Terminal::discard_pending_input`] with the quiet time and the limit
    /// given, for a caller whose terminal is not about to be asked a phrase (the
    /// interactive view, before its keys may act).
    #[cfg(unix)]
    pub fn discard_input_until_quiet(
        &mut self,
        quiet: Duration,
        limit: Duration,
    ) -> io::Result<()> {
        self.flush_input()?;
        let editing = LineEditingOff::enter(self.file.as_raw_fd())?;
        let waited = self.wait_for_quiet_input(quiet, limit);
        // Line editing and echo are back before anything is written, and what
        // arrived since the last read is dropped with them. (Switching line
        // editing on makes the bytes waiting in the queue a line to be read.)
        drop(editing);
        let flushed = self.flush_input();
        waited.and(flushed)
    }

    /// Reads and discards input until `quiet` has passed without any, or fails
    /// when that has not happened within `limit`. Line editing must be off.
    #[cfg(unix)]
    fn wait_for_quiet_input(&mut self, quiet: Duration, limit: Duration) -> io::Result<()> {
        let started = Instant::now();
        let mut last_input = started;
        let mut buffer = [0u8; 256];
        loop {
            let now = Instant::now();
            let quiet_left = quiet.saturating_sub(now.duration_since(last_input));
            if quiet_left.is_zero() {
                return Ok(());
            }
            let limit_left = limit.saturating_sub(now.duration_since(started));
            if limit_left.is_zero() {
                return Err(UnreadableAnswer::InputKeptArriving.into_io_error());
            }
            if !input_is_ready(self.file.as_raw_fd(), quiet_left.min(limit_left))? {
                continue;
            }
            // Ready means the read does not wait. A read of nothing is the other
            // end of the terminal going away.
            match self.file.read(&mut buffer) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the terminal closed while its input was being cleared",
                    ));
                }
                Ok(_) => {
                    last_input = Instant::now();
                    // Flushed as it goes, so nothing waits in the queue to be read
                    // as the answer.
                    self.flush_input()?;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Writes `prompt` to the terminal and reads one line of answer, without
    /// the trailing line terminator. An answer that is not UTF-8, or is longer
    /// than any acknowledgement, is an `InvalidData` error whose source is
    /// [`UnreadableAnswer`]; see [`is_unreadable_answer`].
    #[cfg(unix)]
    pub fn prompt_line(&mut self, prompt: &str) -> io::Result<String> {
        self.file.write_all(prompt.as_bytes())?;
        self.file.flush()?;
        read_line_unbuffered(&mut self.file)
    }

    /// Non-Unix fallback: there is no portable way to reach the console
    /// device or flush its queue without a platform crate, so this keeps the
    /// process stdin path. Type-ahead protection is Unix-only.
    #[cfg(not(unix))]
    pub fn open() -> io::Result<Self> {
        Ok(Self {})
    }

    #[cfg(not(unix))]
    pub fn flush_input(&mut self) -> io::Result<()> {
        Ok(())
    }

    #[cfg(not(unix))]
    pub fn discard_pending_input(&mut self) -> io::Result<()> {
        Ok(())
    }

    #[cfg(not(unix))]
    pub fn prompt_line(&mut self, prompt: &str) -> io::Result<String> {
        let mut stdout = io::stdout();
        stdout.write_all(prompt.as_bytes())?;
        stdout.flush()?;
        let mut answer = String::new();
        match io::stdin().read_line(&mut answer) {
            Ok(_) => {}
            // `read_line` reports bytes that are not UTF-8 this way.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return Err(UnreadableAnswer::NotUtf8.into_io_error());
            }
            Err(error) => return Err(error),
        }
        Ok(answer.trim_end_matches(['\r', '\n']).to_owned())
    }
}

/// Reads up to and including the first `\n` one byte at a time. Deliberately
/// not a `BufReader`: a buffered reader would over-read whatever follows the
/// newline and reintroduce the stale-answer defect for the next prompt.
/// Returns what was read so far when the terminal closes without a newline,
/// matching `read_line`.
#[cfg(unix)]
fn read_line_unbuffered(reader: &mut impl Read) -> io::Result<String> {
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = match reader.read(&mut byte) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if read == 0 || byte[0] == b'\n' {
            break;
        }
        answer.push(byte[0]);
        if answer.len() > MAX_ANSWER_BYTES {
            return Err(UnreadableAnswer::TooLong.into_io_error());
        }
    }
    if answer.last() == Some(&b'\r') {
        answer.pop();
    }
    String::from_utf8(answer).map_err(|_| UnreadableAnswer::NotUtf8.into_io_error())
}

/// Waits up to `timeout` for the terminal to have input to read, and says
/// whether it has (a hangup counts: the read then reports it). `select` and not
/// `poll`: `poll` does not work on terminal devices on macOS.
#[cfg(unix)]
fn input_is_ready(fd: RawFd, timeout: Duration) -> io::Result<bool> {
    if fd < 0 || fd as usize >= libc::FD_SETSIZE {
        return Err(io::Error::other(
            "the terminal's descriptor is too large to wait on",
        ));
    }
    // SAFETY: an all-zero `fd_set` is the empty set; `fd` was checked to be
    // inside it, and the set and the timeout are valid for the call, which
    // keeps neither.
    unsafe {
        let mut readable: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut readable);
        libc::FD_SET(fd, &mut readable);
        let mut wait = libc::timeval {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_usec: timeout.subsec_micros() as libc::suseconds_t,
        };
        match libc::select(
            fd + 1,
            &mut readable,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut wait,
        ) {
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
            0 => Ok(false),
            _ => Ok(true),
        }
    }
}

/// The terminal with line editing and echo off, and reads that wait for at
/// least a byte, for as long as it lives; the attributes it found are put back
/// when it is dropped, whatever the reason. Signals stay enabled, so Ctrl-C
/// still interrupts. (A process that is killed here leaves the terminal so; a
/// shell sets its own modes again at its next prompt.)
#[cfg(unix)]
struct LineEditingOff {
    fd: RawFd,
    saved: libc::termios,
}

#[cfg(unix)]
impl LineEditingOff {
    fn enter(fd: RawFd) -> io::Result<Self> {
        // SAFETY: `tcgetattr` fills the `termios` it is given for a valid
        // descriptor; an all-zero `termios` is a valid value to overwrite.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut quiet = saved;
        quiet.c_lflag &= !(libc::ICANON | libc::ECHO);
        quiet.c_cc[libc::VMIN] = 1;
        quiet.c_cc[libc::VTIME] = 0;
        // SAFETY: `tcsetattr` reads the `termios` it is given and keeps nothing.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, saved })
    }
}

#[cfg(unix)]
impl Drop for LineEditingOff {
    fn drop(&mut self) {
        // SAFETY: puts back the attributes `enter` read from the same descriptor.
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;

    /// Opens a pseudo-terminal pair as (master, slave).
    fn openpty() -> (File, File) {
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: both out-pointers are valid for the call; the optional
        // name/termios/winsize arguments are permitted to be null.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty: {}", io::Error::last_os_error());
        // SAFETY: openpty returned two fresh descriptors we now own.
        unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
    }

    /// Bytes readable from `file` right now.
    fn pending_input(file: &File) -> usize {
        let mut count: libc::c_int = 0;
        // SAFETY: FIONREAD writes one c_int through the provided pointer.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), libc::FIONREAD as _, &mut count) };
        assert_eq!(rc, 0, "FIONREAD: {}", io::Error::last_os_error());
        count as usize
    }

    /// The line discipline processes master writes asynchronously; wait for
    /// the expected bytes to become readable before asserting on them.
    fn wait_for_pending(file: &File, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while pending_input(file) < expected {
            assert!(
                Instant::now() < deadline,
                "only {} of {expected} bytes became readable",
                pending_input(file)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The local modes (line editing, echo, signals ...) of the terminal.
    fn local_modes(file: &File) -> libc::tcflag_t {
        // SAFETY: `tcgetattr` fills the `termios` it is given for a valid
        // descriptor; an all-zero `termios` is a valid value to be overwritten.
        let mut attributes: libc::termios = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::tcgetattr(file.as_raw_fd(), &mut attributes) };
        assert_eq!(rc, 0, "tcgetattr: {}", io::Error::last_os_error());
        attributes.c_lflag
    }

    /// Writes each chunk to the terminal after its delay, from another thread,
    /// like a sender whose bytes are still on their way.
    fn send_later(master: &File, chunks: Vec<(Duration, Vec<u8>)>) -> std::thread::JoinHandle<()> {
        let mut master = master.try_clone().unwrap();
        std::thread::spawn(move || {
            for (delay, bytes) in chunks {
                std::thread::sleep(delay);
                if master.write_all(&bytes).is_err() {
                    return;
                }
            }
        })
    }

    #[test]
    fn flushing_drops_what_is_queued_at_once() {
        let (mut master, slave) = openpty();
        let mut terminal = Terminal::from_file(slave);
        master.write_all(b"queued\n").unwrap();
        wait_for_pending(&terminal.file, b"queued\n".len());

        terminal.flush_input().unwrap();
        assert_eq!(pending_input(&terminal.file), 0);
    }

    #[test]
    fn input_still_on_its_way_is_discarded_until_it_stops_even_when_it_ends_in_the_phrase() {
        let (mut master, slave) = openpty();
        let mut terminal = Terminal::from_file(slave);
        let modes = local_modes(&terminal.file);

        // What a flush alone cannot stop: filler that reaches the terminal after
        // the flush, then a line-kill (^U) to erase it and the phrase, which
        // can be worked out in advance. In canonical mode, after a prompt, the
        // phrase would be all that is left of the line.
        let mut chunks: Vec<(Duration, Vec<u8>)> = (0..10)
            .map(|_| (Duration::from_millis(30), b"filler".to_vec()))
            .collect();
        chunks.push((
            Duration::from_millis(60),
            b"\x15I AUTHORIZE SHARING abc\n".to_vec(),
        ));
        let sender = send_later(&master, chunks);

        let started = Instant::now();
        terminal
            .discard_input_until_quiet(Duration::from_millis(300), Duration::from_secs(10))
            .unwrap();
        // It waited out the whole stream (10 x 30 ms and 60 ms) and then the
        // quiet that followed.
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "{:?}",
            started.elapsed()
        );
        sender.join().unwrap();

        assert_eq!(pending_input(&terminal.file), 0);
        assert_eq!(
            local_modes(&terminal.file),
            modes,
            "line editing and echo are back"
        );
        // Only what is typed now is an answer.
        master.write_all(b"typed after the prompt\n").unwrap();
        assert_eq!(
            terminal.prompt_line("prompt: ").unwrap(),
            "typed after the prompt"
        );
    }

    #[test]
    fn a_quiet_terminal_is_waited_on_only_for_the_quiet_time() {
        let (_master, slave) = openpty();
        let mut terminal = Terminal::from_file(slave);
        let started = Instant::now();
        terminal
            .discard_input_until_quiet(Duration::from_millis(200), Duration::from_secs(10))
            .unwrap();
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(200), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
    }

    #[test]
    fn input_that_never_stops_is_refused_and_the_terminal_is_given_back() {
        let (master, slave) = openpty();
        let mut terminal = Terminal::from_file(slave);
        let modes = local_modes(&terminal.file);
        // A second of input, a byte every 10 ms: never quiet for 200 ms.
        let chunks = (0..100)
            .map(|_| (Duration::from_millis(10), b"x".to_vec()))
            .collect();
        let sender = send_later(&master, chunks);

        let started = Instant::now();
        let error = terminal
            .discard_input_until_quiet(Duration::from_millis(200), Duration::from_millis(600))
            .unwrap_err();
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "{:?}",
            started.elapsed()
        );
        // It reads as an answer that cannot match, and says what happened.
        assert!(is_input_kept_arriving(&error), "{error}");
        assert!(is_unreadable_answer(&error), "{error}");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "input kept arriving at the terminal, so the prompt was not shown"
        );
        assert_eq!(
            local_modes(&terminal.file),
            modes,
            "line editing and echo are back"
        );
        sender.join().unwrap();
    }

    #[test]
    fn a_terminal_that_goes_away_is_an_error_and_not_a_quiet_terminal() {
        let (master, slave) = openpty();
        let mut terminal = Terminal::from_file(slave);
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(master);
        });
        let started = Instant::now();
        let error = terminal
            .discard_input_until_quiet(Duration::from_secs(20), Duration::from_secs(30))
            .unwrap_err();
        closer.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!is_unreadable_answer(&error), "{error}");
        assert!(!is_input_kept_arriving(&error), "{error}");
    }

    #[test]
    fn unbuffered_read_consumes_exactly_one_line() {
        let (mut master, mut slave) = openpty();
        // One write delivering two lines is precisely the type-ahead case:
        // a buffered reader would swallow both.
        master.write_all(b"first\nsecond\n").unwrap();
        wait_for_pending(&slave, b"first\nsecond\n".len());

        assert_eq!(read_line_unbuffered(&mut slave).unwrap(), "first");
        assert_eq!(pending_input(&slave), b"second\n".len());
        assert_eq!(read_line_unbuffered(&mut slave).unwrap(), "second");
        assert_eq!(pending_input(&slave), 0);
    }

    #[test]
    fn discarding_pending_input_drops_type_ahead_but_keeps_the_terminal_usable() {
        let (mut master, slave) = openpty();
        let mut terminal = Terminal::from_file(slave);
        master.write_all(b"typed ahead\n").unwrap();
        wait_for_pending(&terminal.file, b"typed ahead\n".len());

        terminal.discard_pending_input().unwrap();
        assert_eq!(pending_input(&terminal.file), 0);

        master.write_all(b"after the prompt\n").unwrap();
        assert_eq!(
            terminal.prompt_line("prompt: ").unwrap(),
            "after the prompt"
        );
    }

    #[test]
    fn a_chained_challenge_is_not_answered_by_the_previous_lines_type_ahead() {
        let (mut master, slave) = openpty();
        let mut terminal = Terminal::from_file(slave);
        // The reproduction from the issue: both answers arrive in a single
        // write while the first challenge is pending.
        master
            .write_all(b"I SHARE first\nI PUBLISH stale\n")
            .unwrap();
        wait_for_pending(&terminal.file, b"I SHARE first\nI PUBLISH stale\n".len());

        terminal.discard_pending_input().unwrap();
        // Everything queued before the first prompt is gone, including the
        // first answer itself: the human has to type it after seeing the prompt.
        assert_eq!(pending_input(&terminal.file), 0);
        master
            .write_all(b"I SHARE first\nI PUBLISH stale\n")
            .unwrap();
        assert_eq!(terminal.prompt_line("first: ").unwrap(), "I SHARE first");
        wait_for_pending(&terminal.file, b"I PUBLISH stale\n".len());

        // The second challenge must block on fresh input rather than consume
        // the stale second line.
        terminal.discard_pending_input().unwrap();
        assert_eq!(pending_input(&terminal.file), 0);
        master.write_all(b"I PUBLISH fresh\n").unwrap();
        assert_eq!(terminal.prompt_line("second: ").unwrap(), "I PUBLISH fresh");
    }

    #[test]
    fn prompt_is_written_to_the_terminal() {
        let (mut master, slave) = openpty();
        let mut terminal = Terminal::from_file(slave);
        master.write_all(b"yes\n").unwrap();
        assert_eq!(terminal.prompt_line("Type it: ").unwrap(), "yes");
        // The prompt (and, with echo on, the answer) is visible on the master;
        // reads may return the echo and the prompt separately.
        let mut shown = Vec::new();
        let mut chunk = [0u8; 64];
        for _ in 0..8 {
            if String::from_utf8_lossy(&shown).contains("Type it: ") {
                break;
            }
            let n = master.read(&mut chunk).unwrap();
            shown.extend_from_slice(&chunk[..n]);
        }
        assert!(
            String::from_utf8_lossy(&shown).contains("Type it: "),
            "{:?}",
            String::from_utf8_lossy(&shown)
        );
    }

    #[test]
    fn read_line_strips_carriage_return_and_stops_at_eof() {
        let mut input = io::Cursor::new(b"answer\r\nrest".to_vec());
        assert_eq!(read_line_unbuffered(&mut input).unwrap(), "answer");
        assert_eq!(read_line_unbuffered(&mut input).unwrap(), "rest");
        assert_eq!(read_line_unbuffered(&mut input).unwrap(), "");
    }

    #[test]
    fn read_line_rejects_absurdly_long_answers() {
        let mut input = io::Cursor::new(vec![b'x'; MAX_ANSWER_BYTES + 1]);
        let error = read_line_unbuffered(&mut input).unwrap_err();
        assert!(is_unreadable_answer(&error), "{error}");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "acknowledgement answer exceeds the maximum line length"
        );
        // The longest answer allowed is still read.
        let mut input = io::Cursor::new([vec![b'x'; MAX_ANSWER_BYTES], b"\n".to_vec()].concat());
        assert_eq!(
            read_line_unbuffered(&mut input).unwrap().len(),
            MAX_ANSWER_BYTES
        );
    }

    #[test]
    fn read_line_rejects_answers_that_are_not_utf8() {
        for bytes in [
            &b"\xff\xfe\n"[..],
            b"I SHARE \xc3\n",
            b"\x80\n",
            // A truncated multi-byte sequence at the end of the input.
            b"I SHARE \xe2\x82",
        ] {
            let error = read_line_unbuffered(&mut io::Cursor::new(bytes)).unwrap_err();
            assert!(is_unreadable_answer(&error), "{bytes:?}: {error}");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                error.to_string(),
                "acknowledgement answer is not valid UTF-8"
            );
        }
        // Valid UTF-8 that is not ASCII is an answer like any other.
        let mut input = io::Cursor::new("שלום ✓\n".as_bytes().to_vec());
        assert_eq!(read_line_unbuffered(&mut input).unwrap(), "שלום ✓");
    }

    #[test]
    fn a_failure_of_the_terminal_is_not_an_unreadable_answer() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "terminal went away",
                ))
            }
        }
        let error = read_line_unbuffered(&mut Broken).unwrap_err();
        assert!(!is_unreadable_answer(&error), "{error}");
        // Not even when the kind is the same.
        assert!(!is_unreadable_answer(&io::Error::new(
            io::ErrorKind::InvalidData,
            "some other invalid data"
        )));
        assert!(!is_unreadable_answer(&io::Error::from(
            io::ErrorKind::InvalidData
        )));
    }
}
