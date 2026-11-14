//! Terminal setup/teardown + UI submodules.

mod actions;
pub mod app;
pub mod render;
mod scene;
pub mod splash;
mod view;
pub mod wizards;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Switch the terminal into the alternate screen with raw mode + mouse capture.
pub fn init() -> Result<Tui> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Paste arrives as one event instead of a burst of keys. Legacy consoles
    // don't support it, so a failure here is fine.
    let _ = crossterm::execute!(stdout, EnableBracketedPaste);
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to its original state on exit (also on panic paths).
pub fn restore() {
    let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste);
    let _ =crossterm::execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    let _ = crossterm::terminal::disable_raw_mode();
}
