//! The terminal's modes: entering the full-screen view, leaving it so that a
//! flow can print and ask on the main screen, and putting everything back on
//! every way out.
//!
//! Nothing that was typed, pasted or is still on its way when the view appears
//! (and again when it returns from a flow) may act in it: before the first key
//! is read, the input must have been quiet for 300 ms, and what arrives until
//! then is discarded.
//!
//! The way out is covered on every path there is. A normal quit and an error
//! return drop the [`Screen`]. A panic runs the hook installed here, which puts
//! the terminal back before the panic message prints, so the message is
//! readable. Ctrl-C arrives as a key while the terminal is in raw mode and is
//! handled by the model as a quit. On Unix SIGTERM, SIGHUP and SIGINT are
//! watched by a thread that restores the terminal and then dies the way the
//! default action would have made the process die, so a `kill` leaves a usable
//! shell behind; a signal that was ignored when the process started (`nohup`)
//! stays ignored. Only SIGKILL cannot be covered.
//!
//! Turning the modes on and off is done under one lock, so that a restore from
//! the panic hook or the signal thread cannot land in the middle of an enter and
//! leave the alternate screen on behind it. Those two never wait for anything
//! for long: they take the lock for at most a moment, and write what they have
//! to write on a handle of their own that does not wait for the terminal (the
//! thread they would otherwise wait for may itself be stuck writing to a
//! terminal that takes no more), so the process can always be ended.

use std::io::{self, Stdout, Write};
use std::sync::{Mutex, MutexGuard, Once, TryLockError};
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::{Frame, Terminal};

use super::words;

/// Whether the terminal is in the view's modes: raw mode, the alternate screen
/// and bracketed paste. The lock is held across turning them on and across
/// turning them off, so that nothing restores the terminal half way through an
/// enter, or twice at once; the panic hook and the signal thread put the
/// terminal back only if it is on, and only once.
static MODES: Mutex<bool> = Mutex::new(false);

/// How long the panic hook and the signal thread wait for [`MODES`] before they
/// go on without it.
const LOCK_WAIT: Duration = Duration::from_millis(250);

/// How long they wait for a terminal that is slow to take the bytes that put it
/// back, before they leave it at that.
#[cfg(unix)]
const RESTORE_WRITE_WAIT: Duration = Duration::from_millis(200);

/// How long the terminal's input must have been quiet before a key may act.
const INPUT_QUIET: Duration = Duration::from_millis(300);

/// How long input may keep arriving before the view gives up waiting for it to
/// stop. It then ends instead of acting on what is still coming: a terminal
/// that cannot be quiet for a moment is not one to take keys from.
const INPUT_SETTLE_LIMIT: Duration = Duration::from_secs(3);

/// How long the events crossterm has already parsed may take to be thrown away.
const PARSED_EVENTS_LIMIT: Duration = Duration::from_millis(500);

/// The lock on the modes, for the thread that runs the view. A panic while it
/// was held leaves it poisoned but not wrong: it holds a flag.
fn lock_modes() -> MutexGuard<'static, bool> {
    MODES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `mutex`, if it can be had within `wait`; poisoned or not.
fn lock_within<T>(mutex: &Mutex<T>, wait: Duration) -> Option<MutexGuard<'_, T>> {
    let deadline = Instant::now() + wait;
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// Puts the terminal back as the shell had it: raw mode off, the main screen,
/// the cursor shown, bracketed paste off. Does nothing when the view's modes
/// are not on, so every way out can call it.
pub fn restore_terminal() {
    let mut on = lock_modes();
    if std::mem::take(&mut *on) {
        restore_through_stdout();
    }
}

fn restore_through_stdout() {
    let mut out = io::stdout();
    let _ = disable_raw_mode();
    // One command at a time: a terminal that cannot do one of them (bracketed
    // paste on an old Windows console, say) must not keep the rest from running.
    let _ = execute!(out, DisableBracketedPaste);
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = execute!(out, Show);
    let _ = out.flush();
}

/// Puts the terminal back from a thread that cannot afford to wait on anything:
/// the panic hook, and the thread that watches for signals. It waits a moment
/// for the lock and, failing that, goes on without it: the thread that holds it
/// is entering or leaving the modes (or is stuck doing so, in which case nothing
/// else would put the terminal back), and doing this twice does no harm.
///
/// Returns the lock if it had it, so that a caller that is about to end the
/// process can keep the view from entering the modes again until then.
fn restore_terminal_now() -> Option<MutexGuard<'static, bool>> {
    let mut held = lock_within(&MODES, LOCK_WAIT);
    let was_on = match held.as_deref_mut() {
        Some(on) => std::mem::take(on),
        None => true,
    };
    if was_on {
        restore_without_waiting();
    }
    held
}

#[cfg(unix)]
fn restore_without_waiting() {
    let _ = disable_raw_mode();
    // The bytes are made here and written to the terminal on a handle of their
    // own, not through `Stdout`: its lock is held by whoever is writing to it,
    // and that is the thread that may be stuck.
    let mut bytes = Vec::new();
    let _ = execute!(bytes, DisableBracketedPaste);
    let _ = execute!(bytes, LeaveAlternateScreen);
    let _ = execute!(bytes, Show);
    write_to_terminal_without_waiting(&bytes);
}

#[cfg(not(unix))]
fn restore_without_waiting() {
    restore_through_stdout();
}

/// Writes `bytes` to the controlling terminal, giving up after
/// [`RESTORE_WRITE_WAIT`] if it will not take them.
#[cfg(unix)]
fn write_to_terminal_without_waiting(bytes: &[u8]) {
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(mut terminal) = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open("/dev/tty")
    else {
        return;
    };
    let _ = write_all_without_waiting(&mut terminal, bytes, RESTORE_WRITE_WAIT);
}

/// Writes all of `bytes` to a file opened not to block, trying again for up to
/// `wait` when it is full; what it did not take by then is left unwritten.
#[cfg(unix)]
fn write_all_without_waiting(
    file: &mut std::fs::File,
    mut bytes: &[u8],
    wait: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + wait;
    while !bytes.is_empty() {
        match file.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => bytes = &bytes[written..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn enter_modes() -> io::Result<()> {
    let mut on = lock_modes();
    enable_raw_mode()?;
    *on = true;
    if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
        *on = false;
        restore_through_stdout();
        return Err(error);
    }
    // Bracketed paste makes a paste arrive as one event, which the view ignores,
    // instead of as keys that could open a bar and walk through it. A terminal
    // that cannot do it is not a reason to refuse to run: it then has the
    // protections that remain (a bar needs its own key, and a flow its phrase).
    let _ = execute!(io::stdout(), EnableBracketedPaste);
    Ok(())
}

/// The size of the terminal in columns and rows.
pub fn size() -> io::Result<(u16, u16)> {
    ratatui::crossterm::terminal::size()
}

/// The full-screen view, owning the terminal's modes while it is on.
pub struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Screen {
    /// Enters raw mode and the alternate screen. Whatever was typed before the
    /// view appeared, or is still arriving, is discarded until the input has
    /// been quiet for a moment, so it cannot act in the list; if it never is,
    /// the view does not start.
    pub fn enter() -> io::Result<Self> {
        install_panic_hook();
        install_signal_watch();
        enter_modes()?;
        let terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => terminal,
            Err(error) => {
                restore_terminal();
                return Err(error);
            }
        };
        if let Err(error) = settle_input() {
            restore_terminal();
            return Err(error);
        }
        Ok(Self { terminal })
    }

    pub fn draw(&mut self, render: impl FnOnce(&mut Frame)) -> io::Result<()> {
        self.terminal.draw(render).map(|_| ())
    }

    /// Leaves the alternate screen, turns raw mode off, shows the cursor and
    /// flushes, so that what comes next is printed on the main screen and read
    /// from a terminal in its ordinary state.
    pub fn suspend(&mut self) -> io::Result<()> {
        restore_terminal();
        io::stdout().flush()
    }

    /// Re-enters the modes `suspend` left, redraws everything on the next draw,
    /// and waits for the input to be quiet, discarding what arrives until it is,
    /// before the caller reads a key, so nothing typed during or before a
    /// challenge, or still on its way, acts in the list. An error says that the
    /// input never was; the terminal is then in the view's modes still, for the
    /// caller to give back.
    pub fn resume(&mut self) -> io::Result<()> {
        enter_modes()?;
        // A full repaint on the next draw. `Terminal::clear` would do it, but it
        // first asks the terminal where the cursor is (`CSI 6 n`, answered on
        // stdin) to put it back afterwards, and a terminal or a test harness that
        // does not answer turns that into a two second wait and an error that
        // ends the view. The view hides its cursor and has no use for the old
        // position, so it clears by resizing to the size it already has: that
        // erases the screen and forgets the previous frame, and asks nothing.
        // Nothing else in the view reads the cursor either: it draws a full
        // screen viewport, never an inline one, and calls no cursor query.
        let size = self.terminal.size()?;
        self.terminal.resize(size.into())?;
        settle_input()
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// What ends the view when input will not stop arriving.
fn input_would_not_settle() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, words::INPUT_WOULD_NOT_SETTLE)
}

/// Waits until the terminal's input has been quiet for [`INPUT_QUIET`], and
/// throws away everything that arrives meanwhile, so that no key typed or pasted
/// before the view (or still on its way: a paste into a terminal that does not
/// bracket it, a slow link) is read as a key of the view. Fails when that does
/// not happen within [`INPUT_SETTLE_LIMIT`].
///
/// The bytes are looked at on the terminal itself, where nothing is parsed and
/// nothing can hide in a sequence that never completes, and then the events
/// crossterm had already read and parsed are thrown away as well.
fn settle_input() -> io::Result<()> {
    #[cfg(unix)]
    let looked_at_the_terminal = match crate::tty::Terminal::open() {
        Ok(mut tty) => {
            tty.discard_input_until_quiet(INPUT_QUIET, INPUT_SETTLE_LIMIT)
                .map_err(|error| {
                    if crate::tty::is_input_kept_arriving(&error) {
                        input_would_not_settle()
                    } else {
                        error
                    }
                })?;
            true
        }
        // No controlling terminal to look at; a phrase could not be asked on one
        // either.
        Err(_) => false,
    };
    #[cfg(not(unix))]
    let looked_at_the_terminal = false;

    if !looked_at_the_terminal {
        wait_for_quiet_events()?;
    }
    discard_parsed_events()
}

/// The same wait, by the events crossterm reports, for where the terminal
/// itself cannot be looked at.
fn wait_for_quiet_events() -> io::Result<()> {
    let started = Instant::now();
    while event::poll(INPUT_QUIET)? {
        event::read()?;
        if started.elapsed() >= INPUT_SETTLE_LIMIT {
            return Err(input_would_not_settle());
        }
    }
    Ok(())
}

/// Throws away the events crossterm already holds, by polling with no timeout
/// until none is left. The input is quiet by now, so it has few; one that keeps
/// producing them for half a second is not.
fn discard_parsed_events() -> io::Result<()> {
    let deadline = Instant::now() + PARSED_EVENTS_LIMIT;
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if Instant::now() >= deadline {
            return Err(input_would_not_settle());
        }
        if event::read().is_err() {
            break;
        }
    }
    Ok(())
}

/// Makes a panic put the terminal back before its message prints. The panicking
/// thread may be the one that holds the lock on the modes (a panic while they
/// were being changed), so the hook does not wait for it.
fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            drop(restore_terminal_now());
            previous(info);
        }));
    });
}

/// Watches for SIGTERM, SIGHUP and SIGINT: puts the terminal back and then
/// lets the signal have its default effect. While the view is suspended there
/// is nothing to put back, and the effect is what it would have been without
/// the view. Installed once per process and left in place, so the signals keep
/// their default effect afterwards too. A signal that was ignored when the
/// process started is left alone: `nohup` ignores SIGHUP so that the process
/// outlives its terminal, and the view must not turn that into a death. Best
/// effort: without it the view still works.
#[cfg(unix)]
fn install_signal_watch() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;
        let watched: Vec<_> = [SIGHUP, SIGINT, SIGTERM]
            .into_iter()
            .filter(|&signal| !is_ignored(signal))
            .collect();
        if watched.is_empty() {
            return;
        }
        let Ok(mut signals) = Signals::new(&watched) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("cvc-signal-watch".into())
            .spawn(move || {
                if let Some(signal) = signals.forever().next() {
                    end_process_by(signal);
                }
            });
    });
}

/// Whether `signal` is set to be ignored, which is how a process is told, by
/// `nohup` or by a shell running it in the background, to carry on regardless.
#[cfg(unix)]
fn is_ignored(signal: libc::c_int) -> bool {
    // SAFETY: with no new action, `sigaction` only writes the current one into
    // the struct it is given, and an all-zero `sigaction` is a valid value for
    // it to overwrite.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut current) == 0
            && current.sa_sigaction == libc::SIG_IGN
    }
}

/// Puts the terminal back and then makes `signal` do what it would have done
/// without the view, whatever happens while the terminal is put back: the
/// process always ends, and not by any other way than the signal's own if that
/// can be had.
#[cfg(unix)]
fn end_process_by(signal: libc::c_int) -> ! {
    // The lock is kept until the process is gone, so the view cannot enter the
    // modes again in the meantime.
    let _terminal = std::panic::catch_unwind(restore_terminal_now)
        .ok()
        .flatten();
    let _ = signal_hook::low_level::emulate_default_handler(signal);
    // Only reached when that did not end the process. `_exit`, not `exit`: exit
    // flushes `Stdout`, whose lock a stuck thread may hold.
    // SAFETY: `_exit` takes a status and never returns.
    unsafe { libc::_exit(128 + signal) }
}

#[cfg(not(unix))]
fn install_signal_watch() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_a_terminal_that_was_never_entered_does_nothing() {
        assert!(!*lock_modes());
        restore_terminal();
        restore_terminal();
        drop(restore_terminal_now());
        assert!(!*lock_modes());
    }

    #[test]
    fn a_thread_that_cannot_wait_gets_the_lock_when_it_is_free_and_goes_on_when_it_is_not() {
        let mutex = Mutex::new(7);
        assert_eq!(
            lock_within(&mutex, Duration::from_millis(50)).as_deref(),
            Some(&7)
        );

        let held = mutex.lock().unwrap();
        let started = Instant::now();
        assert!(lock_within(&mutex, Duration::from_millis(50)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(held);
        assert!(lock_within(&mutex, Duration::from_millis(50)).is_some());
    }

    #[test]
    fn a_lock_a_panic_left_poisoned_is_still_taken() {
        let mutex = std::sync::Arc::new(Mutex::new(false));
        let poisoner = std::sync::Arc::clone(&mutex);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("the thread that held the lock panicked");
        })
        .join();
        assert!(mutex.is_poisoned());
        assert!(lock_within(&mutex, Duration::from_millis(50)).is_some());
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::fs::File;
        use std::io::Read;
        use std::os::unix::io::{AsRawFd, FromRawFd};

        fn openpty() -> (File, File) {
            let (mut master, mut slave) = (-1, -1);
            // SAFETY: both out-pointers are valid for the call; the optional
            // arguments may be null.
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

        fn do_not_block(file: &File) {
            // SAFETY: `fcntl` with these arguments only changes status flags.
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0);
            let rc =
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
            assert_eq!(rc, 0);
        }

        #[test]
        fn what_a_terminal_takes_is_written_and_what_it_does_not_is_given_up_on() {
            let (mut master, mut slave) = openpty();
            do_not_block(&slave);

            write_all_without_waiting(&mut slave, b"put back", Duration::from_millis(100)).unwrap();
            let mut seen = [0u8; 8];
            master.read_exact(&mut seen).unwrap();
            assert_eq!(&seen, b"put back");

            // Nobody reads the master any more: the terminal fills up and then
            // takes nothing. The write returns instead of waiting for room that
            // never comes, which a blocking write would do for good.
            let chunk = [b'x'; 1024];
            let started = Instant::now();
            let mut written = 0usize;
            let error = loop {
                match write_all_without_waiting(&mut slave, &chunk, Duration::from_millis(100)) {
                    Ok(()) => written += chunk.len(),
                    Err(error) => break error,
                }
                assert!(written < 64 << 20, "a pty that takes everything");
            };
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            assert!(started.elapsed() < Duration::from_secs(20));
        }

        #[test]
        fn a_signal_set_to_be_ignored_is_seen_as_ignored() {
            // SIGUSR2 is used by nothing here.
            let signal = libc::SIGUSR2;
            // SAFETY: sets and puts back the disposition of a signal nothing
            // else in this process uses.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                assert!(!is_ignored(signal));
                libc::signal(signal, libc::SIG_IGN);
                assert!(is_ignored(signal));
                libc::signal(signal, libc::SIG_DFL);
            }
            assert!(!is_ignored(signal));
        }
    }
}
