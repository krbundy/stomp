//! `stomp tui` (§19): an ncdu-style browser over the user's roots.
//!
//! Browsing, grant assignment and the info and compute panels happen in the
//! browser. Estimating, archiving and restoring leave the screen and run the
//! CLI's own commands, with their usual questions and checks, then return.

mod edit;
mod model;
mod view;

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, bail};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::cmd;
use crate::config::Config;
use crate::tasks::Overrides;
use model::{App, Command};

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn enter() -> Result<Term> {
    enable_raw_mode()?;
    // A fresh alternate screen, cleared without asking the terminal where its
    // cursor is: that question can time out over a slow SSH connection.
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        Clear(ClearType::All)
    )?;
    Ok(Terminal::new(CrosstermBackend::new(std::io::stdout()))?)
}

fn leave() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
}

pub fn run(
    cfg: Config,
    config_path: &Path,
    data_dir: &Path,
    start: Option<&Path>,
) -> Result<ExitCode> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("stomp tui needs a terminal; for scripts, use `stomp du` or `stomp usage`");
    }
    let mut app = App::new(cfg, config_path, data_dir, start, true)?;
    // Put the terminal back before a panic message is printed.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        hook(info);
    }));
    let mut terminal = enter()?;
    let result = event_loop(&mut terminal, &mut app);
    leave();
    result
}

fn event_loop(terminal: &mut Term, app: &mut App) -> Result<ExitCode> {
    let mut dirty = true;
    loop {
        if app.drain() {
            dirty = true;
        }
        if dirty {
            terminal.draw(|frame| view::draw(frame, app))?;
            dirty = false;
        }
        // Short waits while sizes are still arriving; longer when idle.
        let wait = if app.pending.is_empty() { 500 } else { 100 };
        if !event::poll(Duration::from_millis(wait))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                dirty = true;
                match app.key(key) {
                    None => {}
                    Some(Command::Quit) => return Ok(ExitCode::SUCCESS),
                    Some(command) => {
                        leave();
                        run_command(app, &command);
                        *terminal = enter()?;
                        app.after_command();
                    }
                }
            }
            Event::Resize(..) => dirty = true,
            _ => {}
        }
    }
}

/// Runs an action the way the CLI would, with the browser out of the way.
fn run_command(app: &App, command: &Command) {
    println!();
    let result = match command {
        Command::Quit => return,
        Command::Estimate(dir) | Command::Archive(dir) => cmd::archive::archive(
            &app.cfg,
            &app.config_path,
            &app.data_dir,
            cmd::archive::ArchiveArgs {
                dir: dir.clone(),
                to: None,
                yes: false,
                dry_run: false,
                estimate: matches!(command, Command::Estimate(_)),
                here: false,
                resume: false,
                level: None,
                chunk_size: None,
                id: None,
                task: None,
                overrides: Overrides::default(),
                format: cmd::Format::Table,
            },
        ),
        Command::Restore(dir) => cmd::archive::restore(
            &app.cfg,
            &app.config_path,
            &app.data_dir,
            cmd::archive::RestoreArgs {
                what: dir.to_string_lossy().into_owned(),
                only: Vec::new(),
                to: None,
                overwrite: false,
                keep_archive: false,
                delete_remote: false,
                dry_run: false,
                yes: false,
                here: false,
                task: None,
                overrides: Overrides::default(),
            },
        ),
    };
    if let Err(e) = result {
        eprintln!("stomp: error: {e:#}");
    }
    print!("\nPress Enter to return to stomp tui. ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

#[cfg(test)]
mod tests;
