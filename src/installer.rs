//! First-run setup wizard — a short guided setup before the chat opens.
//!
//! Steps: welcome → sign-in → (API key) → model → theme → window size →
//! usage budget → confirm. Sign-in prefers the installed Claude Code CLI
//! (its own login, your Claude plan); an Anthropic API key is the fallback.
//! Also creates the ExtraResources folders with a sample skill.

use crate::agent::claude_code::{self, AuthStatus};
use crate::config::{Backend, Config, WindowConfig};
use crate::theme::{self, Theme};
use crate::ui;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use std::path::PathBuf;
use std::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    Welcome,
    SignIn,
    ApiKey,
    Model,
    Theme,
    Window,
    Budget,
    Confirm,
    Done,
}

/// The first four entries of `claude_code::MODELS` are the family aliases.
const MODEL_CHOICES: usize = 4;

const WINDOW_PRESETS: [(&str, u16, u16); 4] = [
    ("system default (don't resize)", 0, 0),
    ("compact — 120 × 32", 120, 32),
    ("standard — 140 × 40", 140, 40),
    ("wide — 180 × 48", 180, 48),
];

const BUDGETS: [(&str, Option<u64>); 5] = [
    ("no budget tracking", None),
    ("200k tokens / day", Some(200_000)),
    ("500k tokens / day", Some(500_000)),
    ("1M tokens / day", Some(1_000_000)),
    ("2M tokens / day", Some(2_000_000)),
];

/// Theme rows visible at once in the theme step.
const THEME_ROWS: usize = 9;

/// What the wizard knows about Claude Code on this PC.
#[derive(Clone, PartialEq)]
enum ClaudeState {
    NotInstalled,
    /// Installed; sign-in state still being checked.
    Checking,
    Ready(AuthStatus),
}

pub struct Installer {
    step: Step,
    pub cfg: Config,
    input: String,
    /// Error shown under the API-key box (e.g. not an `sk-ant-` key).
    input_error: Option<&'static str>,
    signin_sel: usize,
    model_sel: usize,
    theme_sel: usize,
    window_sel: usize,
    budget_sel: usize,
    themes: Vec<Theme>,
    /// API key already available via the ANTHROPIC_API_KEY env var?
    api_key_from_env: bool,
    claude_cli: Option<PathBuf>,
    claude: ClaudeState,
    claude_rx: mpsc::Receiver<AuthStatus>,
    claude_tx: mpsc::Sender<AuthStatus>,
    /// The Claude Code sign-in window was opened from here.
    login_opened: bool,
}

impl Installer {
    pub fn new(cfg: Config) -> Self {
        let api_key_from_env = cfg.has_auth();
        let themes = theme::builtins();
        let theme_sel = themes.iter().position(|t| t.id == cfg.theme).unwrap_or(0);
        let model_sel = claude_code::MODELS[..MODEL_CHOICES]
            .iter()
            .position(|(id, _)| *id == cfg.model)
            .unwrap_or(2); // sonnet
        // tests never look for (or run) a real Claude Code install
        let claude_cli = if cfg!(test) { None } else { claude_code::find_cli() };
        let (claude_tx, claude_rx) = mpsc::channel();
        let me = Self {
            step: Step::Welcome,
            cfg,
            input: String::new(),
            input_error: None,
            signin_sel: 0,
            model_sel,
            theme_sel,
            window_sel: 0, // leave the console alone unless asked
            budget_sel: 0,
            themes,
            api_key_from_env,
            claude: if claude_cli.is_some() { ClaudeState::Checking } else { ClaudeState::NotInstalled },
            claude_cli,
            claude_rx,
            claude_tx,
            login_opened: false,
        };
        me.check_claude(1);
        me
    }

    /// Ask Claude Code for its sign-in state off the UI thread; with
    /// `attempts > 1`, keep asking until it reports signed in.
    fn check_claude(&self, attempts: u32) {
        let Some(cli) = self.claude_cli.clone() else { return };
        let tx = self.claude_tx.clone();
        std::thread::spawn(move || {
            for i in 0..attempts {
                if i > 0 {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
                let Some(status) = claude_code::auth_status(&cli) else { continue };
                let done = status.logged_in;
                if tx.send(status).is_err() || done {
                    break;
                }
            }
        });
    }

    fn claude_signed_in(&self) -> bool {
        matches!(&self.claude, ClaudeState::Ready(s) if s.logged_in)
    }

    /// Run the wizard until it finishes or is aborted; read `self.cfg` after.
    pub fn run(&mut self, term: &mut ui::Tui) -> Result<()> {
        loop {
            while let Ok(status) = self.claude_rx.try_recv() {
                self.claude = ClaudeState::Ready(status);
            }
            term.draw(|f| self.draw(f))?;
            if crossterm::event::poll(std::time::Duration::from_millis(50))?
                // Windows reports key releases too; acting on them would
                // run every key twice (Enter would skip a whole step).
                && let Event::Key(key) = crossterm::event::read()?
                && key.kind != KeyEventKind::Release
                && self.on_key(key)
            {
                return Ok(());
            }
        }
    }

    /// Move a list selection up or down within `len` items.
    fn nudge(sel: &mut usize, key: KeyCode, len: usize) {
        match key {
            KeyCode::Up => *sel = sel.saturating_sub(1),
            KeyCode::Down => *sel = (*sel + 1).min(len.saturating_sub(1)),
            _ => {}
        }
    }

    /// Handle one key; `true` = the wizard is over.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        let code = key.code;
        match self.step {
            Step::Welcome => match code {
                KeyCode::Enter => self.step = Step::SignIn,
                KeyCode::Esc => return true,
                _ => {}
            },
            Step::SignIn => match code {
                KeyCode::Up | KeyCode::Down => Self::nudge(&mut self.signin_sel, code, 2),
                KeyCode::Enter if self.signin_sel == 0 => {
                    let Some(cli) = self.claude_cli.clone() else { return false };
                    self.cfg.backend = Backend::Auto;
                    if !self.claude_signed_in() && !self.login_opened {
                        // Claude Code signs in with its own window; the app
                        // picks the result up by itself later
                        self.login_opened = claude_code::open_login_window(&cli).is_ok();
                        self.check_claude(100);
                    }
                    self.step = Step::Model;
                }
                KeyCode::Enter => {
                    self.cfg.backend = Backend::Api;
                    self.step = if self.api_key_from_env { Step::Model } else { Step::ApiKey };
                }
                KeyCode::Esc => return true,
                _ => {}
            },
            Step::ApiKey => match code {
                KeyCode::Enter => {
                    let key = self.input.trim();
                    if key.is_empty() {
                        return false;
                    }
                    if !key.starts_with("sk-ant-") {
                        self.input_error = Some("that isn't an Anthropic API key (they start with sk-ant-)");
                        return false;
                    }
                    self.cfg.api_key = Some(key.to_string());
                    self.input.clear();
                    self.input_error = None;
                    self.step = Step::Model;
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Esc => {
                    self.input.clear();
                    self.input_error = None;
                    self.step = Step::SignIn;
                }
                KeyCode::Char(c) => self.input.push(c),
                _ => {}
            },
            Step::Model => match code {
                KeyCode::Up | KeyCode::Down => Self::nudge(&mut self.model_sel, code, MODEL_CHOICES),
                KeyCode::Enter => {
                    self.cfg.model = claude_code::MODELS[self.model_sel].0.to_string();
                    self.step = Step::Theme;
                }
                KeyCode::Esc => self.step = Step::SignIn,
                _ => {}
            },
            Step::Theme => match code {
                KeyCode::Up | KeyCode::Down => Self::nudge(&mut self.theme_sel, code, self.themes.len()),
                KeyCode::Enter => {
                    self.cfg.theme = self.themes[self.theme_sel].id.clone();
                    self.step = Step::Window;
                }
                KeyCode::Esc => self.step = Step::Model,
                _ => {}
            },
            Step::Window => match code {
                KeyCode::Up | KeyCode::Down => Self::nudge(&mut self.window_sel, code, WINDOW_PRESETS.len()),
                KeyCode::Enter => {
                    let (_, cols, rows) = WINDOW_PRESETS[self.window_sel];
                    self.cfg.window = if cols == 0 {
                        WindowConfig { enabled: false, ..WindowConfig::default() }
                    } else {
                        WindowConfig { enabled: true, cols, rows, center: true }
                    };
                    self.step = Step::Budget;
                }
                KeyCode::Esc => self.step = Step::Theme,
                _ => {}
            },
            Step::Budget => match code {
                KeyCode::Up | KeyCode::Down => Self::nudge(&mut self.budget_sel, code, BUDGETS.len()),
                KeyCode::Enter => {
                    self.cfg.usage_budget_daily = BUDGETS[self.budget_sel].1;
                    self.step = Step::Confirm;
                }
                KeyCode::Esc => self.step = Step::Window,
                _ => {}
            },
            Step::Confirm => match code {
                KeyCode::Enter => {
                    self.cfg.wizard_done = true;
                    self.step = Step::Done;
                }
                KeyCode::Esc => self.step = Step::Budget,
                _ => {}
            },
            Step::Done => return matches!(code, KeyCode::Enter | KeyCode::Esc),
        }
        false
    }

    // ------------------------------------------------------------- drawing

    fn draw(&self, f: &mut ratatui::Frame) {
        let area = f.area();
        let w = 66.min(area.width.saturating_sub(4));
        let h = 18.min(area.height.saturating_sub(2));
        let r = Rect::new(
            area.x + area.width.saturating_sub(w) / 2,
            area.y + area.height.saturating_sub(h) / 2,
            w,
            h,
        );
        if r.width < 8 || r.height < 4 {
            return; // a window this small can't show anything useful
        }
        // the theme step previews whatever is highlighted
        let theme = &self.themes[self.theme_sel];

        let (title, border) = match self.step {
            Step::Welcome => (" welcome ", theme.accent),
            Step::SignIn => (" how do you sign in? ", theme.accent2),
            Step::ApiKey => (" Anthropic API key ", theme.accent2),
            Step::Model => (" pick a model ", theme.accent),
            Step::Theme => (" pick a theme ", theme.accent),
            Step::Window => (" console window size ", theme.accent2),
            Step::Budget => (" daily usage budget ", theme.accent2),
            Step::Confirm => (" confirm ", theme.success),
            Step::Done => (" done ", theme.success),
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border))
            .title(Line::from(Span::styled(title, Style::new().fg(border).add_modifier(Modifier::BOLD))));
        f.render_widget(block, r);
        let inner = Rect::new(r.x + 2, r.y + 1, r.width - 4, r.height - 2);

        let text = |s: String| Line::from(Span::styled(s, Style::new().fg(theme.fg)));
        let dim = |s: String| Line::from(Span::styled(s, Style::new().fg(theme.fg_dim)));
        let opt = |sel: bool, label: String| {
            Line::from(Span::styled(
                format!("  {} {label}", if sel { "◉" } else { "○" }),
                if sel {
                    Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(theme.fg_dim)
                },
            ))
        };
        let keys = |enter: &str, esc: &str| {
            Line::from(vec![
                Span::styled("  [Enter]", Style::new().fg(theme.success)),
                Span::styled(format!(" {enter}   "), Style::new().fg(theme.fg_dim)),
                Span::styled("[Esc]", Style::new().fg(theme.error)),
                Span::styled(format!(" {esc}"), Style::new().fg(theme.fg_dim)),
            ])
        };

        let lines: Vec<Line> = match self.step {
            Step::Welcome => vec![
                Line::from(""),
                Line::from(gradient_word(theme)),
                Line::from(Span::styled(
                    "a galaxy-grade coding agent",
                    Style::new().fg(theme.accent2).add_modifier(Modifier::ITALIC),
                )),
                Line::from(""),
                text("let's set you up in a few quick steps.".into()),
                dim("you can change everything later (`beyond setup`).".into()),
                Line::from(""),
                keys("begin", "quit"),
            ],
            Step::SignIn => {
                let claude_note = match &self.claude {
                    ClaudeState::NotInstalled => "    not installed — npm install -g @anthropic-ai/claude-code".to_string(),
                    ClaudeState::Checking => "    checking your sign-in…".to_string(),
                    ClaudeState::Ready(s) if s.logged_in => match &s.plan {
                        Some(plan) => format!("    signed in ✓ ({plan} plan) — uses your Claude plan"),
                        None => "    signed in ✓".to_string(),
                    },
                    ClaudeState::Ready(_) => "    installed — Enter opens its sign-in window".to_string(),
                };
                let key_label = if self.api_key_from_env {
                    "Anthropic API key (already set ✓)"
                } else {
                    "Anthropic API key (console.anthropic.com)"
                };
                vec![
                    Line::from(""),
                    text("which engine should answer your messages?".into()),
                    Line::from(""),
                    opt(self.signin_sel == 0, "Claude Code (recommended)".into()),
                    dim(claude_note),
                    Line::from(""),
                    opt(self.signin_sel == 1, key_label.into()),
                    dim("    metered pay-per-token billing".into()),
                    Line::from(""),
                    dim("  ↑/↓ choose · Enter continue · Esc quit".into()),
                ]
            }
            Step::ApiKey => {
                let masked: String = self.input.chars().map(|_| '•').collect();
                let mut l = vec![
                    Line::from(""),
                    text("paste your Anthropic API key (console.anthropic.com):".into()),
                    Line::from(""),
                    Line::from(Span::styled(format!("  ▸ {masked}"), Style::new().fg(theme.accent2))),
                    Line::from(""),
                ];
                match self.input_error {
                    Some(e) => l.push(Line::from(Span::styled(e, Style::new().fg(theme.error)))),
                    None => l.push(dim("stored only in your local Beyond config.".into())),
                }
                l.push(Line::from(""));
                l.push(keys("save", "back"));
                l
            }
            Step::Model => {
                let mut l = vec![Line::from("")];
                for (i, (id, desc)) in claude_code::MODELS[..MODEL_CHOICES].iter().enumerate() {
                    l.push(opt(i == self.model_sel, format!("{id:<7} {desc}")));
                }
                l.push(Line::from(""));
                l.push(dim("  exact versions: /model inside the app".into()));
                l
            }
            Step::Theme => {
                // a window of rows that follows the selection
                let start = self
                    .theme_sel
                    .saturating_sub(THEME_ROWS / 2)
                    .min(self.themes.len().saturating_sub(THEME_ROWS));
                let mut l = vec![dim(format!("  {} of {}", self.theme_sel + 1, self.themes.len()))];
                for (i, t) in self.themes.iter().enumerate().skip(start).take(THEME_ROWS) {
                    let sel = i == self.theme_sel;
                    let mut spans = vec![Span::styled(
                        format!("  {} {:<16} ", if sel { "◉" } else { "○" }, t.name),
                        if sel {
                            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::new().fg(theme.fg_dim)
                        },
                    )];
                    spans.extend(
                        (0..12).map(|k| Span::styled("━", Style::new().fg(t.gradient_color(k as f32 / 11.0)))),
                    );
                    l.push(Line::from(spans));
                }
                l.push(Line::from(""));
                l.push(dim("  custom themes: /loadtheme <file.json>".into()));
                l
            }
            Step::Window => {
                let mut l = vec![Line::from(""), text("resize the console window on launch:".into()), Line::from("")];
                for (i, (label, _, _)) in WINDOW_PRESETS.iter().enumerate() {
                    l.push(opt(i == self.window_sel, label.to_string()));
                }
                l.push(Line::from(""));
                l.push(dim("  (Windows Terminal sizes itself; this is for the classic console)".into()));
                l
            }
            Step::Budget => {
                let mut l = vec![
                    Line::from(""),
                    text("soft daily token budget (shown in the status bar):".into()),
                    Line::from(""),
                ];
                for (i, (label, _)) in BUDGETS.iter().enumerate() {
                    l.push(opt(i == self.budget_sel, label.to_string()));
                }
                l
            }
            Step::Confirm => {
                let (signin, ok) = match self.cfg.backend {
                    Backend::Api if self.cfg.has_auth() => ("API key ✓".to_string(), true),
                    Backend::Api => ("no API key yet — `/login key` in the app".to_string(), false),
                    _ if self.claude_signed_in() => ("Claude Code ✓".to_string(), true),
                    _ => ("Claude Code — finish its sign-in window".to_string(), false),
                };
                let (_, wcols, wrows) = WINDOW_PRESETS[self.window_sel];
                let window = if wcols == 0 { "system default".to_string() } else { format!("{wcols} × {wrows}, centered") };
                vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  sign-in:      ", Style::new().fg(theme.fg_dim)),
                        Span::styled(signin, Style::new().fg(if ok { theme.success } else { theme.warning })),
                    ]),
                    text(format!("  model:        {}", claude_code::MODELS[self.model_sel].0)),
                    text(format!("  theme:        {}", self.themes[self.theme_sel].name)),
                    text(format!("  window:       {window}")),
                    text(format!("  daily budget: {}", BUDGETS[self.budget_sel].0)),
                    Line::from(""),
                    keys("finish setup", "back"),
                ]
            }
            Step::Done => vec![
                Line::from(""),
                Line::from(Span::styled("  ✓ setup complete", Style::new().fg(theme.success))),
                dim("  ExtraResources created at:".into()),
                Line::from(Span::styled(
                    format!("    {}", crate::config::Paths::extra_resources_dir().display()),
                    Style::new().fg(theme.accent2),
                )),
                dim("    MCPs/ — drop MCP servers (.exe, .py, .js) here".into()),
                dim("    Skills/ — markdown skills the agent learns from".into()),
                Line::from(""),
                text("  press Enter to launch Beyond CLI".into()),
            ],
        };
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn gradient_word(theme: &Theme) -> Vec<Span<'static>> {
    let word = "✦ BEYOND CLI";
    let n = word.chars().count() as f32;
    word.chars()
        .enumerate()
        .map(|(i, ch)| Span::styled(ch.to_string(), Style::new().fg(theme.gradient_color(i as f32 / n))))
        .collect()
}

/// Create the ExtraResources scaffold (sample skill + MCPs readme).
pub fn seed_extra_resources() -> Result<()> {
    std::fs::create_dir_all(crate::config::Paths::mcps_dir())?;
    std::fs::create_dir_all(crate::config::Paths::skills_dir())?;
    let readme = crate::config::Paths::extra_resources_dir().join("README.md");
    if !readme.exists() {
        std::fs::write(
            &readme,
            "# ExtraResources\n\n\
             - `MCPs/` — drop MCP servers here (`.exe`, `.py`, `.js`); they are\n\
               started automatically and their tools merge into the agent.\n\
             - `Skills/` — markdown skills (with `name:`/`description:` frontmatter)\n\
               injected into every session. The agent writes its own here too.\n\
             - `dumps/` — process/system dumps from the `process_dump` tool.\n",
        )?;
    }
    let sample = crate::config::Paths::skills_dir().join("beyond-basics.md");
    if !sample.exists() {
        std::fs::write(
            &sample,
            "---\nname: beyond-basics\ndescription: How the user likes Beyond CLI to behave\n---\n\n\
             # Beyond basics\n\n\
             - Be concise; prefer short answers with code over long prose.\n\
             - Check `process_list` before launching heavy builds or test suites.\n\
             - After discovering a reusable workflow, save it with `write_skill`.\n\
             - The user's timezone is local; use `schedule_reminder` for anything\n\
               deferred — never silently postpone.\n",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn press(inst: &mut Installer, code: KeyCode) -> bool {
        inst.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn wizard() -> Installer {
        Installer::new(Config { api_key: None, ..Config::default() })
    }

    #[test]
    fn api_key_path_validates_then_finishes() {
        let mut w = wizard();
        press(&mut w, KeyCode::Enter); // welcome
        // Claude Code is missing: Enter on it does nothing
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::SignIn);
        press(&mut w, KeyCode::Down);
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::ApiKey);
        for c in "nope".chars() {
            press(&mut w, KeyCode::Char(c));
        }
        press(&mut w, KeyCode::Enter);
        assert!(w.input_error.is_some() && w.cfg.api_key.is_none());
        w.input = "sk-ant-test".into();
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.cfg.api_key.as_deref(), Some("sk-ant-test"));
        for _ in 0..5 {
            press(&mut w, KeyCode::Enter); // model, theme, window, budget, confirm
        }
        assert!(w.cfg.wizard_done && w.cfg.backend == Backend::Api);
        assert!(press(&mut w, KeyCode::Enter));
    }

    #[test]
    fn esc_goes_back_and_every_theme_is_offered() {
        let mut w = wizard();
        assert_eq!(w.themes.len(), 25);
        w.step = Step::Theme;
        for _ in 0..40 {
            press(&mut w, KeyCode::Down);
        }
        assert_eq!(w.theme_sel, 24);
        press(&mut w, KeyCode::Esc);
        assert_eq!(w.step, Step::Model);
    }
}
