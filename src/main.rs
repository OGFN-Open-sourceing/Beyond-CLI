//! Beyond CLI entry point.
//!
//! Flow: parse CLI args → load config (first-run wizard if needed) →
//! apply window sizing → splash screen (boot steps run concurrently) →
//! main chat loop → cleanup (MCP shutdown, terminal restore).

mod agent;
mod commands;
mod config;
mod installer;
mod mcp;
mod notify;
mod sessions;
mod skills;
mod sysinfo;
mod theme;
mod ui;
mod util;
mod voice;
mod win;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "beyond",
    version,
    about = "Beyond CLI — a galaxy-grade Claude coding agent for your terminal",
    long_about = None
)]
struct Cli {
    /// Override the configured theme for this session
    #[arg(long)]
    theme: Option<String>,

    /// Tag a project directory at launch (same as /project)
    #[arg(long)]
    project: Option<String>,

    /// Resume a session by id at launch (same as /loadsession)
    #[arg(long)]
    session: Option<String>,

    /// Skip the splash animation (also respects config `animations: false`)
    #[arg(long)]
    no_splash: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Re-run the setup wizard
    Setup,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            ui::restore();
            eprintln!("✗ beyond: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = config::load().context("loading config")?;

    // --- first-run setup wizard (MSI-like experience) -----------------
    let need_setup = match cli.command {
        Some(Commands::Setup) => true,
        None => !cfg.wizard_done && !cfg.has_auth(),
    };
    if need_setup {
        let mut term = ui::init()?;
        let mut inst = installer::Installer::new(cfg.clone());
        let finished = inst.run(&mut term);
        ui::restore();
        finished?;
        cfg = inst.cfg;
        config::save(&cfg)?;
    }
    installer::seed_extra_resources()?;

    // --- window sizing -------------------------------------------------
    if cfg.window.enabled {
        win::apply_window_config(&cfg.window);
    }

    // --- overrides ------------------------------------------------------
    if let Some(t) = &cli.theme {
        cfg.theme = t.clone();
    }
    // An older build could save loosely typed names ("opus5.5"); read them the
    // way `/model` does now so the saved setting keeps working.
    if let Ok(model) = agent::claude_code::resolve_model(&cfg.model) {
        cfg.model = model;
    }

    let app_theme = theme::resolve(&cfg.theme).unwrap_or_else(|_| theme::beyond());
    let loaded_skills = skills::load_all(&config::Paths::skills_dir());

    // --- boot: splash drives concurrent MCP discovery -------------------
    let (boot_tx, boot_rx) = mpsc::unbounded_channel();
    let (mcp_tx, mcp_rx) = tokio::sync::oneshot::channel::<Arc<mcp::McpManager>>();
    let mcps_dir = config::Paths::mcps_dir();
    let boot_tx_clone = boot_tx.clone();
    tokio::spawn(async move {
        use ui::splash::{BootState, BootUpdate};
        let _ = boot_tx_clone.send(BootUpdate { id: "theme", state: BootState::Done });
        let _ = boot_tx_clone.send(BootUpdate { id: "skills", state: BootState::Done });
        let _ = boot_tx_clone.send(BootUpdate { id: "system", state: BootState::Done });
        let _ = boot_tx_clone.send(BootUpdate { id: "sessions", state: BootState::Done });
        let manager = mcp::McpManager::load_all(&mcps_dir).await;
        let state = if manager.warnings.is_empty() {
            BootState::Done
        } else {
            BootState::Warn(format!("{} server(s) failed", manager.warnings.len()))
        };
        let _ = boot_tx_clone.send(BootUpdate { id: "mcp", state });
        let _ = mcp_tx.send(Arc::new(manager));
    });

    let mut term = ui::init()?;

    // splash unless disabled
    if !cli.no_splash && cfg.animations {
        let steps = vec![
            ui::splash::BootStep::pending("theme", "loading theme"),
            ui::splash::BootStep::pending("skills", "loading skills"),
            ui::splash::BootStep::pending("mcp", "connecting MCP servers"),
            ui::splash::BootStep::pending("sessions", "indexing sessions"),
            ui::splash::BootStep::pending("system", "reading system info"),
        ];
        let splash = ui::splash::Splash::new(app_theme.clone(), steps, boot_rx, false);
        run_splash(&mut term, splash)?;
    } else {
        // still drain the boot channel to keep things tidy
        drop(boot_tx);
    }

    // MCP manager must be ready before the app starts
    let mcps = mcp_rx
        .await
        .unwrap_or_else(|_| Arc::new(mcp::McpManager { servers: vec![], warnings: vec![] }));

    // --- reminders watcher ----------------------------------------------
    let (rem_tx, rem_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            notify::prune();
            for r in notify::list() {
                if !r.fired && r.when <= chrono::Local::now() {
                    let r2 = r.clone();
                    // toast is a blocking process spawn — keep it off the runtime
                    tokio::task::spawn_blocking(move || {
                        let _ = notify::fire(&r2);
                    })
                    .await
                    .ok();
                    let _ = rem_tx.send(r);
                }
            }
        }
    });

    // --- main app --------------------------------------------------------
    let mut app = ui::app::App::new(cfg, app_theme, loaded_skills, mcps.clone(), rem_rx);

    if let Some(p) = &cli.project {
        app.set_project_direct(std::path::PathBuf::from(p));
    }
    if let Some(id) = &cli.session {
        app.apply_cmd(commands::Cmd::LoadSession(Some(id.clone())));
    }

    let result = app.run(&mut term);

    // --- cleanup: interrupt agent, kill MCP servers, restore terminal ----
    if let Some(a) = app.agent.take() {
        a.interrupt.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // app was consumed by run(); if nothing else holds the manager, shut down
    if let Ok(mgr) = Arc::try_unwrap(mcps) {
        mgr.shutdown().await;
    }
    ui::restore();
    result
}

/// Drive the splash animation loop until it finishes.
fn run_splash(term: &mut ui::Tui, mut splash: ui::splash::Splash) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        let dt = 0.033; // ~30fps splash
        if splash.update(dt) {
            return Ok(());
        }
        term.draw(|f| splash.render(f))?;
        if crossterm::event::poll(std::time::Duration::from_millis(15))? {
            use crossterm::event::{Event, KeyEventKind};
            match crossterm::event::read()? {
                // a key press skips the animation (mouse moves and key
                // releases must not, or the splash vanishes on its own)
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if start.elapsed() > std::time::Duration::from_millis(300) {
                        return Ok(());
                    }
                }
                // the window changed size: repaint every cell
                Event::Resize(..) => term.clear()?,
                _ => {}
            }
        }
        if start.elapsed() > std::time::Duration::from_secs(30) {
            return Ok(()); // safety valve
        }
    }
}
