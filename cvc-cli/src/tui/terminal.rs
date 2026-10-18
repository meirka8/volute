//! The terminal's modes: entering the full-screen view, leaving it so that a
//! flow can print and ask on the main screen, and putting everything back on
//! every way out.
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

/// Whether the terminal is in the view's modes: raw mode, the alternate screen
/// and bracketed paste. The panic hook and the signal thread put the terminal
/// back only if it is, and only once.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// How long draining pending input may take before it gives up. A terminal that
/// is being flooded has more to worry about than a stray key.
const DRAIN_LIMIT: Duration = Duration::from_millis(500);

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
    /// view appeared is discarded, so it cannot act in the list.
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
        discard_pending_input();
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
    /// and drains every pending input event before the caller reads one, so
    /// nothing typed during or before a challenge acts in the list.
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
        discard_pending_input();
        Ok(())
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Throws away input nobody has read: what the terminal itself holds (the same
/// flush a typed challenge starts with), then every event crossterm already
/// read and parsed, by polling with no timeout until none is left.
fn discard_pending_input() {
    if let Ok(mut tty) = crate::tty::Terminal::open() {
        let _ = tty.discard_pending_input();
    }
    let deadline = Instant::now() + DRAIN_LIMIT;
    while Instant::now() < deadline && event::poll(Duration::ZERO).unwrap_or(false) {
        if event::read().is_err() {
            break;
        }
    }
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
