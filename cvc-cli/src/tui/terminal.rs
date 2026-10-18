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
//! shell behind. Only SIGKILL cannot be covered.

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
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
/// and bracketed paste. The panic hook and the signal thread put the terminal
/// back only if it is, and only once.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// How long the terminal's input must have been quiet before a key may act.
const INPUT_QUIET: Duration = Duration::from_millis(300);

/// How long input may keep arriving before the view gives up waiting for it to
/// stop. It then ends instead of acting on what is still coming: a terminal
/// that cannot be quiet for a moment is not one to take keys from.
const INPUT_SETTLE_LIMIT: Duration = Duration::from_secs(3);

/// How long the events crossterm has already parsed may take to be thrown away.
const PARSED_EVENTS_LIMIT: Duration = Duration::from_millis(500);

/// Puts the terminal back as the shell had it: raw mode off, the main screen,
/// the cursor shown, bracketed paste off. Does nothing when the view's modes
/// are not on, so every way out can call it.
pub fn restore_terminal() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let mut out = io::stdout();
        let _ = disable_raw_mode();
        // One command at a time: a terminal that cannot do one of them (bracketed
        // paste on an old Windows console, say) must not keep the rest from running.
        let _ = execute!(out, DisableBracketedPaste);
        let _ = execute!(out, LeaveAlternateScreen);
        let _ = execute!(out, Show);
        let _ = out.flush();
    }
}

fn enter_modes() -> io::Result<()> {
    enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
        restore_terminal();
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

/// Makes a panic put the terminal back before its message prints.
fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
    });
}

/// Watches for SIGTERM, SIGHUP and SIGINT: puts the terminal back and then
/// lets the signal have its default effect. While the view is suspended there
/// is nothing to put back, and the effect is what it would have been without
/// the view. Installed once per process and left in place, so the signals keep
/// their default effect afterwards too. Best effort: without it the view still
/// works.
#[cfg(unix)]
fn install_signal_watch() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;
        let Ok(mut signals) = Signals::new([SIGHUP, SIGINT, SIGTERM]) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("cvc-signal-watch".into())
            .spawn(move || {
                if let Some(signal) = signals.forever().next() {
                    restore_terminal();
                    let _ = signal_hook::low_level::emulate_default_handler(signal);
                    std::process::exit(128 + signal);
                }
            });
    });
}

#[cfg(not(unix))]
fn install_signal_watch() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_a_terminal_that_was_never_entered_does_nothing() {
        assert!(!ACTIVE.load(Ordering::SeqCst));
        restore_terminal();
        restore_terminal();
        assert!(!ACTIVE.load(Ordering::SeqCst));
    }
}
