mod app;
mod config;
mod git;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;

#[derive(Parser, Debug)]
#[command(
    name = "diffview",
    about = "Interactive git diff viewer with staging support",
    version
)]
struct Args {
    /// Diff tool to use (raw | delta | difftastic)
    #[arg(long, value_name = "TOOL")]
    tool: Option<String>,

    /// Repository path (default: current directory)
    path: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // ── Setup terminal ──────────────────────────────────────────────────
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ── Create app ──────────────────────────────────────────────────────
    let result = (|| -> Result<()> {
        let mut app = app::App::new(args.tool, args.path)?;
        app.run(&mut terminal)
    })();

    // ── Restore terminal (always, even on error) ────────────────────────
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = result {
        eprintln!("Error: {:#}", err);
        std::process::exit(1);
    }

    Ok(())
}
