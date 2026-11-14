//! `/env` wizard: tag repos, detect toolchains, install what's missing —
//! a guided, step-by-step overlay. The agent has the same powers via the
//! `env_detect` / `env_install` tools; this wizard is the human-friendly path.

use crate::agent::tools::install_command;
use crate::config::EnvProfile;
use crate::sysinfo::detect_structured;
use crate::theme::Theme;
use crate::util;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    Intro,
    Pick,
    Review,
    Installing,
    Done,
}

/// Supported, installable toolchains (order matters for display).
const INSTALLABLE: [&str; 7] = ["node", "python", "rust", "go", "dotnet", "java", "git"];

pub struct EnvWizard {
    step: Step,
    repos: Vec<PathBuf>,
    repo_sel: usize,
    detected: Vec<(String, bool, String)>,
    missing: Vec<String>,
    selected: Vec<bool>,
    sel: usize,
    progress: Vec<String>,
    progress_rx: Option<mpsc::UnboundedReceiver<String>>,
}

impl EnvWizard {
    pub fn new(existing_repos: Vec<String>) -> Self {
        Self {
            step: Step::Intro,
            repos: existing_repos.iter().map(PathBuf::from).collect(),
            repo_sel: 0,
            detected: vec![],
            missing: vec![],
            selected: vec![],
            sel: 0,
            progress: vec![],
            progress_rx: None,
        }
    }

    /// Drain install progress lines (called each frame).
    pub fn pump(&mut self) {
        let mut rx = match self.progress_rx.take() {
            Some(r) => r,
            None => return,
        };
        loop {
            match rx.try_recv() {
                Ok(line) => self.progress.push(line),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.progress.push("✓ env setup complete".into());
                    self.step = Step::Done;
                    return; // keep progress_rx = None
                }
            }
        }
        self.progress_rx = Some(rx);
    }

    /// Handle a key; returns `true` when the wizard should close.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        match self.step {
            Step::Intro => match key.code {
                KeyCode::Enter | KeyCode::Char('b') => {
                    self.step = Step::Pick;
                    self.add_repo_dialog();
                    false
                }
                KeyCode::Esc => true,
                _ => false,
            },
            Step::Pick => match key.code {
                KeyCode::Char('b') | KeyCode::Char('a') => {
                    self.add_repo_dialog();
                    false
                }
                KeyCode::Up => {
                    self.repo_sel = self.repo_sel.saturating_sub(1);
                    false
                }
                KeyCode::Down => {
                    if self.repo_sel + 1 < self.repos.len() {
                        self.repo_sel += 1;
                    }
                    false
                }
                KeyCode::Char('d') | KeyCode::Backspace => {
                    if self.repo_sel < self.repos.len() {
                        self.repos.remove(self.repo_sel);
                        self.repo_sel = self.repo_sel.saturating_sub(1);
                    }
                    false
                }
                KeyCode::Enter => {
                    self.start_detect();
                    false
                }
                KeyCode::Esc => true,
                _ => false,
            },
            Step::Review => match key.code {
                KeyCode::Up => {
                    self.sel = self.sel.saturating_sub(1);
                    false
                }
                KeyCode::Down => {
                    if self.sel + 1 < self.missing.len() {
                        self.sel += 1;
                    }
                    false
                }
                KeyCode::Char(' ') => {
                    if let Some(v) = self.selected.get_mut(self.sel) {
                        *v = !*v;
                    }
                    false
                }
                KeyCode::Enter => {
                    self.start_install();
                    false
                }
                KeyCode::Esc => true,
                _ => false,
            },
            Step::Installing => {
                self.pump();
                matches!(key.code, KeyCode::Esc)
            }
            Step::Done => matches!(key.code, KeyCode::Enter | KeyCode::Esc),
        }
    }

    /// Updated env profile when the wizard finishes, for the app to persist.
    pub fn take_result(&mut self) -> Option<EnvProfile> {
        if self.step != Step::Done {
            return None;
        }
        let installed: Vec<String> = self
            .missing
            .iter()
            .enumerate()
            .filter(|(i, _)| self.selected.get(*i).copied().unwrap_or(false))
            .map(|(_, m)| m.clone())
            .collect();
        Some(EnvProfile {
            repos: self.repos.iter().map(|p| p.to_string_lossy().to_string()).collect(),
            installed,
            last_setup: Some(chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()),
        })
    }

    fn add_repo_dialog(&mut self) {
        if let Ok(Some(p)) = util::browse_folder("Pick a project folder to add")
            && !self.repos.contains(&p) {
                self.repos.push(p);
            }
    }

    /// Detect toolchains (fast, all local `--version` probes) and move to review.
    fn start_detect(&mut self) {
        self.detected = detect_structured();
        self.missing = self
            .detected
            .iter()
            .filter(|(n, ok, _)| !ok && INSTALLABLE.contains(&n.as_str()))
            .map(|(n, _, _)| n.clone())
            .collect();
        self.selected = vec![true; self.missing.len()];
        self.sel = 0;
        self.step = if self.missing.is_empty() { Step::Done } else { Step::Review };
    }

    fn start_install(&mut self) {
        let picks: Vec<String> = self
            .missing
            .iter()
            .enumerate()
            .filter(|(i, _)| self.selected.get(*i).copied().unwrap_or(false))
            .map(|(_, m)| m.clone())
            .collect();
        if picks.is_empty() {
            self.step = Step::Done;
            return;
        }
        self.step = Step::Installing;
        let (tx, rx) = mpsc::unbounded_channel();
        self.progress_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            for name in picks {
                let (cmd, via) = install_command(&name);
                let _ = tx.send(format!(" installing {name} via {via} …"));
                if cmd.is_empty() {
                    let _ = tx.send(format!(" ✗ unknown toolchain {name}"));
                    continue;
                }
                let prog = if cfg!(windows) { "powershell" } else { "sh" };
                let args: Vec<&str> = if cfg!(windows) {
                    vec!["-NoProfile", "-Command", &cmd]
                } else {
                    vec!["-c", &cmd]
                };
                match std::process::Command::new(prog).args(args).output() {
                    Ok(o) if o.status.success() => {
                        let _ = tx.send(format!(" ✓ {name} installed"));
                    }
                    Ok(o) => {
                        let tail = String::from_utf8_lossy(&o.stderr);
                        let _ = tx.send(format!(" ✗ {name} failed: {}", util::one_line(&tail, 120)));
                    }
                    Err(e) => {
                        let _ = tx.send(format!(" ✗ {name}: {e}"));
                    }
                }
            }
        });
    }

    // ------------------------------------------------------------ drawing

    pub fn draw(&mut self, f: &mut Frame, theme: &Theme) {
        self.pump();
        let area = f.area();
        f.render_widget(Clear, area);

        // dim backdrop
        let block = Block::new().style(Style::new().bg(theme.bg.unwrap_or(ratatui::style::Color::Rgb(12, 10, 26))));
        f.render_widget(block, area);

        let w = (area.width - 8).min(78);
        let h = (area.height - 6).min(24);
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);

        let title = match self.step {
            Step::Intro => " environment setup ",
            Step::Pick => " pick your repos ",
            Step::Review => " install missing toolchains ",
            Step::Installing => " installing… ",
            Step::Done => " done ",
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme.border_focus))
            .title(Line::from(Span::styled(
                title,
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            )));
        f.render_widget(block, r);
        let inner = Rect::new(r.x + 2, r.y + 1, r.width - 4, r.height - 2);

        match self.step {
            Step::Intro => {
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Beyond can prepare your machine for your projects:",
                        Style::new().fg(theme.fg),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  1. pick the repo folders you work in",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(Span::styled(
                        "  2. it detects which toolchains are installed",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(Span::styled(
                        "  3. it pre-installs missing SDKs (with your approval)",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  [Enter]", Style::new().fg(theme.success)),
                        Span::styled(" start   ", Style::new().fg(theme.fg_dim)),
                        Span::styled("[Esc]", Style::new().fg(theme.error)),
                        Span::styled(" cancel", Style::new().fg(theme.fg_dim)),
                    ]),
                ];
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Pick => {
                let mut lines = vec![
                    Line::from(Span::styled(
                        "repos:",
                        Style::new().fg(theme.accent2).add_modifier(Modifier::BOLD),
                    )),
                ];
                if self.repos.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "  (none yet — press b to browse)",
                        Style::new().fg(theme.fg_dim),
                    )));
                }
                for (i, p) in self.repos.iter().enumerate() {
                    let style = if i == self.repo_sel {
                        Style::new().fg(theme.fg).bg(theme.selection_bg)
                    } else {
                        Style::new().fg(theme.fg_dim)
                    };
                    lines.push(Line::from(Span::styled(
                        format!("  ❯ {}", p.display()),
                        style,
                    )));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("  [b]", Style::new().fg(theme.success)),
                    Span::styled(" add folder   ", Style::new().fg(theme.fg_dim)),
                    Span::styled("[d]", Style::new().fg(theme.error)),
                    Span::styled(" remove   ", Style::new().fg(theme.fg_dim)),
                    Span::styled("[Enter]", Style::new().fg(theme.success)),
                    Span::styled(" detect   ", Style::new().fg(theme.fg_dim)),
                    Span::styled("[Esc]", Style::new().fg(theme.error)),
                    Span::styled(" cancel", Style::new().fg(theme.fg_dim)),
                ]));
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Review => {
                let mut lines = vec![Line::from("")];
                for (n, ok, v) in &self.detected {
                    let (icon, style) = if *ok {
                        ("✓", Style::new().fg(theme.success))
                    } else {
                        ("✗", Style::new().fg(theme.error))
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {icon} "), style),
                        Span::styled(format!("{n:<8}"), Style::new().fg(theme.fg)),
                        Span::styled(v.clone(), Style::new().fg(theme.fg_dim)),
                    ]));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "select what to install (space toggles, enter starts):",
                    Style::new().fg(theme.accent2),
                )));
                for (i, m) in self.missing.iter().enumerate() {
                    let check = if self.selected.get(i).copied().unwrap_or(false) { "◉" } else { "○" };
                    let style = if i == self.sel {
                        Style::new().fg(theme.fg).bg(theme.selection_bg)
                    } else {
                        Style::new().fg(theme.fg)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {check} "), Style::new().fg(theme.accent)),
                        Span::styled(m.clone(), style),
                    ]));
                }
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Installing | Step::Done => {
                let mut lines = vec![Line::from("")];
                for p in &self.progress {
                    lines.push(Line::from(Span::styled(
                        format!("  {p}"),
                        Style::new().fg(theme.fg),
                    )));
                }
                if self.step == Step::Done {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::styled("  [Enter]", Style::new().fg(theme.success)),
                        Span::styled(" finish", Style::new().fg(theme.fg_dim)),
                    ]));
                }
                f.render_widget(Paragraph::new(lines), inner);
            }
        }
    }
}
