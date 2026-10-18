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
//! This module therefore talks to the terminal itself, with two layers:
//!
//! 1. the terminal's pending input is discarded immediately before the prompt
//!    is written, so the answer is provably typed *after* the prompt; and
//! 2. the answer is read one byte at a time from an unbuffered handle, so a
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
use std::os::unix::io::AsRawFd;

/// Upper bound on one answer line. Every challenge is far shorter; anything
/// longer is not a human typing an acknowledgement.
#[cfg(unix)]
const MAX_ANSWER_BYTES: usize = 4096;

/// What was typed at a prompt cannot be taken as an answer at all: it is not
/// UTF-8 text, or it is longer than any acknowledgement. It travels as the
/// source of an `InvalidData` I/O error, so the I/O error type stays the
/// prompt's only one, and [`is_unreadable_answer`] tells it from a failure of
/// the terminal itself.
#[derive(Debug)]
pub enum UnreadableAnswer {
    /// The bytes typed are not valid UTF-8.
    NotUtf8,
    /// The line is longer than the longest answer read (4096 bytes).
    #[cfg(unix)]
    TooLong,
}

impl fmt::Display for UnreadableAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 => f.write_str("acknowledgement answer is not valid UTF-8"),
            #[cfg(unix)]
            Self::TooLong => f.write_str("acknowledgement answer exceeds the maximum line length"),
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
    /// read yet, so nothing typed or pasted before the prompt can answer it.
    #[cfg(unix)]
    pub fn discard_pending_input(&mut self) -> io::Result<()> {
        // SAFETY: `tcflush` only takes a valid open descriptor and a queue
        // selector; it neither retains the descriptor nor touches memory.
        if unsafe { libc::tcflush(self.file.as_raw_fd(), libc::TCIFLUSH) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
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

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;
    use std::time::{Duration, Instant};

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
