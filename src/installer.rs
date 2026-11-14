//! First-run setup wizard — the "MSI-like" guided install experience.
//!
//! Runs as its own mini TUI before the main app when the config is missing
//! an API key. Steps: welcome → API key → model → theme → window size →
//! usage budget → confirm. Creates the ExtraResources folders with a sample
//! skill so first launch is immediately useful.

use crate::config::{Config, WindowConfig};
use crate::theme::{self, Theme};
use crate::ui;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    Welcome,
    AuthMethod,
    Login,
    ApiKey,
    Model,
    Theme,
    Window,
    Budget,
    Confirm,
    Done,
}

const MODELS: [&str; 4] = [
    "claude-sonnet-4-5",
    "claude-sonnet-4-5-20250929",
    "claude-opus-4-5",
    "claude-haiku-4-5",
];

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

pub struct Installer {
    step: Step,
    pub cfg: Config,
    input: String,
    model_sel: usize,
    theme_sel: usize,
    window_sel: usize,
    budget_sel: usize,
    auth_sel: usize,
    preview_themes: [Theme; 2],
    /// API key already available via the ANTHROPIC_API_KEY env var?
    api_key_from_env: bool,
    /// Claude sign-in: URL to authorize + the PKCE verifier (state).
    login_url: String,
    login_verifier: String,
    browser_opened: bool,
    /// (pasted code, verifier) — exchanged by main after the wizard closes,
    /// because the token endpoint call must run on the async runtime.
    pub pending_login: Option<(String, String)>,
}

impl Installer {
    pub fn new(cfg: Config) -> Self {
        let api_key_from_env = cfg.api_key.is_some();
        Self {
            step: Step::Welcome,
            cfg,
            input: String::new(),
            model_sel: 0,
            theme_sel: 0,
            window_sel: 2, // standard
            budget_sel: 0,
            auth_sel: 0,
            preview_themes: [theme::beyond(), theme::galaxy()],
            api_key_from_env,
            login_url: String::new(),
            login_verifier: String::new(),
            browser_opened: false,
            pending_login: None,
        }
    }

    /// Run the wizard in the given terminal.
    ///
    /// Returns `true` when the wizard finished (or was aborted) so the caller
    /// can take over: read `self.cfg` / `take_pending_login()` afterwards.
    pub fn run(&mut self, term: &mut ui::Tui) -> Result<bool> {
        loop {
            term.draw(|f| self.draw(f))?;
            if crossterm::event::poll(std::time::Duration::from_millis(50))?
                && let Event::Key(key) = crossterm::event::read()?
                    && self.on_key(key) {
                        return Ok(true);
                    }
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match self.step {
            Step::Welcome => match key.code {
                KeyCode::Enter => {
                    self.step = Step::AuthMethod;
                    false
                }
                KeyCode::Esc => true,
                _ => false,
            },
            Step::AuthMethod => match key.code {
                KeyCode::Up => {
                    self.auth_sel = self.auth_sel.saturating_sub(1);
                    false
                }
                KeyCode::Down => {
                    self.auth_sel = (self.auth_sel + 1).min(1);
                    false
                }
                KeyCode::Enter => match self.auth_sel {
                    0 => {
                        // Claude account: open the browser, collect the code
                        let pkce = crate::auth::generate_pkce();
                        self.login_url = crate::auth::authorize_url(&pkce);
                        self.login_verifier = pkce.verifier;
                        self.browser_opened = crate::auth::open_browser(&self.login_url).is_ok();
                        self.step = Step::Login;
                        false
                    }
                    _ => {
                        // API key (or the already-set env var)
                        self.step = if self.api_key_from_env { Step::Model } else { Step::ApiKey };
                        false
                    }
                },
                KeyCode::Esc => true,
                _ => false,
            },
            Step::Login => match key.code {
                KeyCode::Enter => {
                    if self.input.trim().is_empty() {
                        // convenience: re-open the browser if it never opened
                        if !self.browser_opened {
                            self.browser_opened = crate::auth::open_browser(&self.login_url).is_ok();
                        }
                    } else {
                        self.pending_login = Some((self.input.trim().to_string(), self.login_verifier.clone()));
                        self.input.clear();
                        self.step = Step::Model;
                    }
                    false
                }
                KeyCode::Backspace => {
                    self.input.pop();
                    false
                }
                KeyCode::Esc => {
                    // skip sign-in; the app offers /login later
                    self.input.clear();
                    self.step = Step::Model;
                    false
                }
                KeyCode::Char(c) => {
                    self.input.push(c);
                    false
                }
                _ => false,
            },
            Step::ApiKey => match key.code {
                KeyCode::Enter => {
                    if !self.input.trim().is_empty() {
                        self.cfg.api_key = Some(self.input.trim().to_string());
                        self.input.clear();
                        self.step = Step::Model;
                    }
                    false
                }
                KeyCode::Backspace => {
                    self.input.pop();
                    false
                }
                KeyCode::Esc => true,
                KeyCode::Char(c) => {
                    self.input.push(c);
                    false
                }
                _ => false,
            },
            Step::Model => match key.code {
                KeyCode::Up => {
                    self.model_sel = self.model_sel.saturating_sub(1);
                    false
                }
                KeyCode::Down => {
                    self.model_sel = (self.model_sel + 1).min(MODELS.len() - 1);
                    false
                }
                KeyCode::Enter => {
                    self.cfg.model = MODELS[self.model_sel].to_string();
                    self.step = Step::Theme;
                    false
                }
                KeyCode::Esc => true,
                _ => false,
            },
            Step::Theme => match key.code {
                KeyCode::Up => {
                    self.theme_sel = self.theme_sel.saturating_sub(1);
                    false
                }
                KeyCode::Down => {
                    self.theme_sel = (self.theme_sel + 1).min(1);
                    false
                }
                KeyCode::Enter => {
                    self.cfg.theme = self.preview_themes[self.theme_sel].id.clone();
                    self.step = Step::Window;
                    false
                }
                KeyCode::Esc => true,
                _ => false,
            },
            Step::Window => match key.code {
                KeyCode::Up => {
                    self.window_sel = self.window_sel.saturating_sub(1);
                    false
                }
                KeyCode::Down => {
                    self.window_sel = (self.window_sel + 1).min(WINDOW_PRESETS.len() - 1);
                    false
                }
                KeyCode::Enter => {
                    let (_, cols, rows) = WINDOW_PRESETS[self.window_sel];
                    self.cfg.window = if cols == 0 {
                        WindowConfig { enabled: false, ..WindowConfig::default() }
                    } else {
                        WindowConfig { enabled: true, cols, rows, center: true }
                    };
                    self.step = Step::Budget;
                    false
                }
                KeyCode::Esc => true,
                _ => false,
            },
            Step::Budget => match key.code {
                KeyCode::Up => {
                    self.budget_sel = self.budget_sel.saturating_sub(1);
                    false
                }
                KeyCode::Down => {
                    self.budget_sel = (self.budget_sel + 1).min(BUDGETS.len() - 1);
                    false
                }
                KeyCode::Enter => {
                    self.cfg.usage_budget_daily = BUDGETS[self.budget_sel].1;
                    self.step = Step::Confirm;
                    false
                }
                KeyCode::Esc => true,
                _ => false,
            },
            Step::Confirm => match key.code {
                KeyCode::Enter => {
                    self.cfg.wizard_done = true;
                    self.step = Step::Done;
                    false
                }
                KeyCode::Esc => true, // abort setup; app will prompt again next run
                _ => false,
            },
            Step::Done => matches!(key.code, KeyCode::Enter | KeyCode::Esc),
        }
    }

    fn draw(&self, f: &mut ratatui::Frame) {
        let area = f.area();
        let w = 64.min(area.width.saturating_sub(4));
        let h = 16.min(area.height.saturating_sub(4));
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
        let theme = &self.preview_themes[self.theme_sel];

        let (title, border) = match self.step {
            Step::Welcome => (" welcome ", theme.accent),
            Step::AuthMethod => (" how do you sign in? ", theme.accent2),
            Step::Login => (" claude sign-in ", theme.accent2),
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
            .title(Line::from(Span::styled(
                title,
                Style::new().fg(border).add_modifier(Modifier::BOLD),
            )));
        f.render_widget(block, r);
        let inner = Rect::new(r.x + 2, r.y + 1, r.width - 4, r.height - 2);

        match self.step {
            Step::Welcome => {
                let lines = vec![
                    Line::from(""),
                    Line::from(gradient_word(theme)),
                    Line::from(Span::styled(
                        "a galaxy-grade coding agent",
                        Style::new().fg(theme.accent2).add_modifier(Modifier::ITALIC),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "let's set you up in a few quick steps.",
                        Style::new().fg(theme.fg),
                    )),
                    Line::from(Span::styled(
                        "you can change everything later via /setup.",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  [Enter]", Style::new().fg(theme.success)),
                        Span::styled(" begin   ", Style::new().fg(theme.fg_dim)),
                        Span::styled("[Esc]", Style::new().fg(theme.error)),
                        Span::styled(" quit", Style::new().fg(theme.fg_dim)),
                    ]),
                ];
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::AuthMethod => {
                let opt = |sel: bool, label: &str| {
                    Line::from(Span::styled(
                        format!("  {} {label}", if sel { "◉" } else { "○" }),
                        if sel {
                            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::new().fg(theme.fg_dim)
                        },
                    ))
                };
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "how do you want to authenticate?",
                        Style::new().fg(theme.fg),
                    )),
                    Line::from(""),
                    opt(self.auth_sel == 0, "Claude account (browser sign-in)"),
                    Line::from(Span::styled(
                        "    uses your Pro/Max subscription, auto-refresh",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(""),
                    opt(self.auth_sel == 1, if self.api_key_from_env {
                        "ANTHROPIC_API_KEY from env (already set ✓)"
                    } else {
                        "Anthropic API key (console.anthropic.com)"
                    }),
                    Line::from(Span::styled(
                        "    metered pay-per-token billing",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  ↑/↓ to choose · Enter to continue",
                        Style::new().fg(theme.fg_dim),
                    )),
                ];
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Login => {
                let mut lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "sign in with your Claude account:",
                        Style::new().fg(theme.fg),
                    )),
                ];
                if self.browser_opened {
                    lines.push(Line::from(Span::styled(
                        "browser opened — after signing in, copy the code it shows,",
                        Style::new().fg(theme.fg_dim),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        "could not open the browser — visit this URL:",
                        Style::new().fg(theme.warning),
                    )));
                    lines.push(Line::from(Span::styled(
                        format!("  {}", crate::util::truncate_chars(&self.login_url, 58)),
                        Style::new().fg(theme.accent2),
                    )));
                }
                lines.push(Line::from(Span::styled(
                    "then paste the code here and press Enter:",
                    Style::new().fg(theme.fg_dim),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  ▸ {}", self.input),
                    Style::new().fg(theme.accent2),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "[Esc] skip — sign in later with /login",
                    Style::new().fg(theme.fg_dim),
                )));
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::ApiKey => {
                let masked: String = self.input.chars().map(|_| '•').collect();
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "paste your Anthropic API key (console.anthropic.com):",
                        Style::new().fg(theme.fg),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("  {masked}"),
                        Style::new().fg(theme.accent2),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "stored locally in your config file only.",
                        Style::new().fg(theme.fg_dim),
                    )),
                ];
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Model => {
                let mut lines = vec![Line::from("")];
                for (i, m) in MODELS.iter().enumerate() {
                    let sel = i == self.model_sel;
                    lines.push(Line::from(Span::styled(
                        format!("  {} {m}", if sel { "◉" } else { "○" }),
                        if sel {
                            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::new().fg(theme.fg_dim)
                        },
                    )));
                }
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Theme => {
                let mut lines = vec![Line::from("")];
                for (i, (name, t)) in [("Beyond", &self.preview_themes[0]), ("Galaxy", &self.preview_themes[1])]
                    .iter()
                    .enumerate()
                {
                    let sel = i == self.theme_sel;
                    let swatch: Vec<Span> = (0..10)
                        .map(|k| Span::styled("━", Style::new().fg(t.gradient_color(k as f32 / 9.0))))
                        .collect();
                    let mut spans = vec![Span::styled(
                        format!("  {} {name:<8} ", if sel { "◉" } else { "○" }),
                        if sel {
                            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::new().fg(theme.fg_dim)
                        },
                    )];
                    spans.extend(swatch);
                    lines.push(Line::from(spans));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  themes are fully customizable — /loadtheme <file.json>",
                    Style::new().fg(theme.fg_dim),
                )));
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Window => {
                let mut lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "resize the console window on launch:",
                        Style::new().fg(theme.fg),
                    )),
                    Line::from(""),
                ];
                for (i, (label, _, _)) in WINDOW_PRESETS.iter().enumerate() {
                    let sel = i == self.window_sel;
                    lines.push(Line::from(Span::styled(
                        format!("  {} {label}", if sel { "◉" } else { "○" }),
                        if sel {
                            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::new().fg(theme.fg_dim)
                        },
                    )));
                }
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Budget => {
                let mut lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "soft daily token budget (shown in the status bar",
                        Style::new().fg(theme.fg),
                    )),
                    Line::from(Span::styled(
                        "and included in /later reminders):",
                        Style::new().fg(theme.fg),
                    )),
                    Line::from(""),
                ];
                for (i, (label, _)) in BUDGETS.iter().enumerate() {
                    let sel = i == self.budget_sel;
                    lines.push(Line::from(Span::styled(
                        format!("  {} {label}", if sel { "◉" } else { "○" }),
                        if sel {
                            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::new().fg(theme.fg_dim)
                        },
                    )));
                }
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Confirm => {
                let key_state = if self.cfg.oauth.is_some() || self.pending_login.is_some() {
                    "claude account ✓"
                } else if self.cfg.api_key.is_some() {
                    "api key ✓"
                } else {
                    "missing!"
                };
                let (_, wcols, wrows) = WINDOW_PRESETS[self.window_sel];
                let win_state = if wcols == 0 {
                    "system default".to_string()
                } else {
                    format!("{wcols} × {wrows}, centered")
                };
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled("  sign-in:      ", Style::new().fg(theme.fg_dim))),
                    Line::from(Span::styled(format!("  {key_state}"), Style::new().fg(theme.success))),
                    Line::from(Span::styled(format!("  model:        {}", self.cfg.model), Style::new().fg(theme.fg))),
                    Line::from(Span::styled(format!("  theme:        {}", self.cfg.theme), Style::new().fg(theme.fg))),
                    Line::from(Span::styled(format!("  window:       {win_state}"), Style::new().fg(theme.fg))),
                    Line::from(Span::styled(
                        format!("  daily budget: {}", BUDGETS[self.budget_sel].0),
                        Style::new().fg(theme.fg),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  [Enter]", Style::new().fg(theme.success)),
                        Span::styled(" finish setup   ", Style::new().fg(theme.fg_dim)),
                        Span::styled("[Esc]", Style::new().fg(theme.error)),
                        Span::styled(" abort", Style::new().fg(theme.fg_dim)),
                    ]),
                ];
                f.render_widget(Paragraph::new(lines), inner);
            }
            Step::Done => {
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled("  ✓ setup complete", Style::new().fg(theme.success))),
                    Line::from(Span::styled(
                        "  ExtraResources created at:",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(Span::styled(
                        format!("    {}", crate::config::Paths::extra_resources_dir().display()),
                        Style::new().fg(theme.accent2),
                    )),
                    Line::from(Span::styled(
                        "    MCPs/ — drop MCP.exe / MCP.py servers here",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(Span::styled(
                        "    Skills/ — markdown skills the agent learns from",
                        Style::new().fg(theme.fg_dim),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  launching Beyond CLI…",
                        Style::new().fg(theme.fg),
                    )),
                ];
                f.render_widget(Paragraph::new(lines), inner);
            }
        }
    }
}

fn gradient_word(theme: &Theme) -> Vec<Span<'static>> {
    let word = "✦ BEYOND CLI";
    word.chars()
        .enumerate()
        .map(|(i, ch)| {
            Span::styled(
                ch.to_string(),
                Style::new().fg(theme.gradient_color(i as f32 / word.chars().count() as f32)),
            )
        })
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
