//! An on-demand viewer. Collection happens before entering the terminal UI;
//! rendering and navigation perform no filesystem scans or network requests.
mod view;

use crate::model::SnapshotV1;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io::{self, IsTerminal},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

/// Check before reading inputs or collecting so redirected/CI invocations do
/// not silently perform a live capture or write terminal escapes into a file.
pub fn require_terminal() -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("TUI requires an interactive terminal (stdin and stdout). Use doctor --snapshot FILE or report compare for redirected output; use ssh -t for an interactive SSH session.".into());
    }
    if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
        return Err("TUI needs a terminal with cursor/color support; use doctor --snapshot FILE for plain output.".into());
    }
    Ok(())
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        // Construct the guard before any fallible terminal output so partial
        // initialization still restores the caller's terminal.
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        Ok(guard)
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

struct ExitSignals {
    requested: Arc<AtomicBool>,
    #[cfg(unix)]
    registrations: Vec<signal_hook::SigId>,
}

impl ExitSignals {
    fn install() -> io::Result<Self> {
        let signals = Self {
            requested: Arc::new(AtomicBool::new(false)),
            #[cfg(unix)]
            registrations: Vec::new(),
        };
        #[cfg(unix)]
        {
            let mut signals = signals;
            for signal in [
                signal_hook::consts::SIGTERM,
                signal_hook::consts::SIGHUP,
                signal_hook::consts::SIGINT,
            ] {
                signals.registrations.push(signal_hook::flag::register(
                    signal,
                    signals.requested.clone(),
                )?);
            }
            Ok(signals)
        }
        #[cfg(not(unix))]
        Ok(signals)
    }
}

impl Drop for ExitSignals {
    fn drop(&mut self) {
        #[cfg(unix)]
        for registration in self.registrations.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}

pub fn run(
    current: &SnapshotV1,
    before: Option<&SnapshotV1>,
    profile: Option<&str>,
    temporal: bool,
    allow_legacy_identity: bool,
) -> Result<(), String> {
    require_terminal()?;
    let mut app = view::ViewState::new(current, before, profile, temporal, allow_legacy_identity);
    let signals = ExitSignals::install()
        .map_err(|e| format!("cannot install terminal signal handlers: {e}"))?;
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        restore_terminal();
        eprintln!("TUI stopped unexpectedly: {info}");
    }));
    let result = (|| -> io::Result<()> {
        let _guard = TerminalGuard::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;
        let mut redraw = true;
        while !signals.requested.load(Ordering::Relaxed) {
            if redraw {
                terminal.draw(|frame| app.draw(frame))?;
                redraw = false;
            }
            let ready = match event::poll(Duration::from_millis(200)) {
                Err(_) if signals.requested.load(Ordering::Relaxed) => break,
                other => other?,
            };
            if ready {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if app.handle_key(key) {
                            break;
                        }
                        redraw = true;
                    }
                    Event::Resize(_, _) => redraw = true,
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    std::panic::set_hook(previous_hook);
    result.map_err(|e| format!("terminal UI failed: {e}"))
}
