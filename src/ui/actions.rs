//! Slash-command execution: everything `/something` does to the app.
//!
//! Parsing lives in [`crate::commands`]; this module turns a parsed
//! [`Cmd`] into state changes and transcript messages.

use std::path::PathBuf;

use super::app::{App, Choice, Entry, EntryKind, Popup, Prompt, SessionItem, ToolState, LIMITED_LOGIN_SECS};
use crate::agent::claude_code;
use crate::config::Backend;
use super::view::project_tag;
use crate::agent::types::{ContentBlock, Role};
use crate::commands::{self, Cmd};
use crate::sessions::{self, Session};
use crate::ui::wizards::EnvWizard;
use crate::{notify, theme, util};

/// Claude Code sessions offered in the `/loadsession` picker (newest first).
const MAX_CLAUDE_SESSIONS: usize = 60;

impl App {
    /// Execute a parsed slash command.
    pub fn apply_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Help(topic) => self.cmd_help(topic),
            Cmd::Theme(None) => {
                let items = theme::available_themes();
                let sel = items.iter().position(|(id, _)| *id == self.theme.id).unwrap_or(0);
                self.popup = Popup::ThemePicker { items, sel, original: Box::new(self.theme.clone()) };
            }
            Cmd::Theme(Some(name)) => self.set_theme(&name.to_lowercase()),
            Cmd::LoadTheme(file) => self.cmd_load_theme(&file),
            Cmd::Project(None) => match util::browse_folder("Pick your project folder") {
                Ok(Some(dir)) => self.set_project_direct(dir),
                Ok(None) => self.push(EntryKind::Info, "no folder picked"),
                Err(e) => self.push(EntryKind::Error, format!("{e:#} — try `/project <path>`")),
            },
            Cmd::Project(Some(dir)) => self.set_project_direct(util::expand_path(dir.trim_matches('"'))),
            Cmd::Env => self.wizard = Some(EnvWizard::new(self.cfg.env.repos.clone())),
            Cmd::LoadSession(None) => self.open_session_picker(),
            Cmd::LoadSession(Some(id)) => self.load_session(&id),
            Cmd::Later(args) => self.cmd_later(args),
            Cmd::Model(None) => {
                let items = claude_code::MODELS.iter().map(|(id, d)| (id.to_string(), d.to_string())).collect();
                self.open_choice(Choice::Model, items, &self.cfg.model.clone());
            }
            Cmd::Model(Some(m)) => self.set_model(m.trim()),
            Cmd::Mode(None) => {
                let items =
                    claude_code::PERMISSION_MODES.iter().map(|(id, d)| (id.to_string(), d.to_string())).collect();
                self.open_choice(Choice::Mode, items, &self.cfg.permission_mode.clone());
            }
            Cmd::Mode(Some(m)) => self.set_mode(m.trim()),
            Cmd::Backend(arg) => self.cmd_backend(arg),
            Cmd::Login(false) => self.login_with_claude(),
            Cmd::Login(true) => self.begin_login(false),
            Cmd::LimitedLogin => self.begin_login(true),
            Cmd::Logout => {
                let had_key = self.cfg.has_auth();
                self.persist(|c| c.clear_auth());
                let mut note = String::from(if had_key { "stored API key removed" } else { "no API key was stored" });
                if self.claude_auth.as_ref().is_some_and(|a| a.logged_in) {
                    // that login belongs to Claude Code, not to Beyond
                    note.push_str(" — Claude Code is still signed in; `claude auth logout` in a terminal signs it out");
                }
                self.push(EntryKind::Success, note);
            }
            Cmd::Voice => self.start_voice(),
            Cmd::Usage => self.cmd_usage(),
            Cmd::Budget(arg) => self.cmd_budget(arg),
            Cmd::Window(arg) => self.cmd_window(arg),
            Cmd::Clear => {
                if self.agent.is_some() {
                    self.push(EntryKind::Warn, "wait for the current turn to finish (Esc interrupts it)");
                    return;
                }
                // the old conversation stays on disk; start a fresh one
                self.entries.clear();
                self.messages.clear();
                self.session = Session::new(&self.cfg.model);
                self.session.cwd = self.project.as_ref().map(|p| p.to_string_lossy().to_string());
                self.session_tokens = 0;
                self.session_cost = 0.0;
                self.scroll = 0;
            }
            Cmd::Setup => self.push(
                EntryKind::Info,
                "the setup wizard runs before the chat starts — quit and run `beyond setup`",
            ),
            Cmd::Quit => self.quit = true,
            Cmd::Unknown(what) => {
                self.push(EntryKind::Error, format!("{what} — unknown command, `/help` lists them all"))
            }
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------- /help

    fn cmd_help(&mut self, topic: Option<String>) {
        let all = commands::registry();
        let row = |c: &commands::CmdDef| {
            let args = if c.args.is_empty() { String::new() } else { format!(" {}", c.args) };
            format!("`/{}{args}` — {}", c.name, c.desc)
        };
        if let Some(topic) = topic {
            let name = topic.trim_start_matches('/').to_lowercase();
            match all.iter().find(|c| c.name == name) {
                Some(c) => self.push(EntryKind::Info, row(c)),
                None => self.push(EntryKind::Error, format!("no command named /{name}")),
            }
            return;
        }
        let mut text = String::from("**Commands**\n");
        for c in all {
            text.push_str(&row(c));
            text.push('\n');
        }
        text.push_str(
            "\n**Keys**\n\
             `Enter` send · `Shift+Enter` new line · `Esc` interrupt · `↑↓` history\n\
             `PgUp/PgDn` or mouse wheel scroll · `F9` dictate · `Ctrl+L` clear · `Ctrl+C` quit\n\
             \n**Extras**\n",
        );
        text.push_str(&format!(
            "drop MCP servers and skills into `{}`",
            crate::config::Paths::extra_resources_dir().display()
        ));
        self.push(EntryKind::Info, text);
    }

    // ------------------------------------------------------------ themes

    /// Switch to a theme by id and remember it.
    pub(super) fn set_theme(&mut self, id: &str) {
        match theme::resolve(id) {
            Ok(t) => {
                let id = t.id.clone();
                self.persist(|c| c.theme = id.clone());
                self.push(EntryKind::Success, format!("theme: {}", t.name));
                self.theme = t;
                self.restyle();
            }
            Err(e) => self.push(EntryKind::Error, format!("{e:#}")),
        }
    }

    /// `/loadtheme file.json`: validate, copy into the themes folder so it
    /// survives the original file moving, then apply it.
    fn cmd_load_theme(&mut self, file: &str) {
        let src = util::expand_path(file.trim_matches('"'));
        let loaded = match theme::load_json_theme(&src) {
            Ok(t) => t,
            Err(e) => {
                self.push(EntryKind::Error, format!("{e:#}"));
                return;
            }
        };
        let stem = loaded.id.trim_start_matches("custom:").to_string();
        let dest = crate::config::Paths::custom_themes_dir().join(format!("{stem}.json"));
        if src != dest
            && let Err(e) = std::fs::copy(&src, &dest)
        {
            self.push(EntryKind::Warn, format!("theme applied, but could not be saved for next time: {e}"));
        }
        let id = loaded.id.clone();
        self.persist(|c| c.theme = id.clone());
        self.push(EntryKind::Success, format!("theme loaded: {} — now in `/theme` too", loaded.name));
        self.theme = loaded;
        self.restyle();
    }

    // ----------------------------------------------------------- project

    /// Tag `dir` as the working project: tools run there from the next turn.
    pub fn set_project_direct(&mut self, dir: PathBuf) {
        if !dir.is_dir() {
            self.push(EntryKind::Error, format!("not a folder: {}", dir.display()));
            return;
        }
        self.push(EntryKind::Success, format!("project tagged {}", project_tag(&dir, 200)));
        self.session.cwd = Some(dir.to_string_lossy().to_string());
        self.project = Some(dir);
    }

    // ---------------------------------------------------------- sessions

    fn open_session_picker(&mut self) {
        let mut items: Vec<SessionItem> = sessions::list_beyond()
            .into_iter()
            .filter(|s| s.id != self.session.id)
            .map(|s| SessionItem { id: s.id, title: s.title, when: s.created, source: "beyond" })
            .collect();
        items.extend(sessions::scan_claude_sessions().into_iter().take(MAX_CLAUDE_SESSIONS).map(|s| {
            SessionItem { title: s.cwd, when: s.modified, id: s.id, source: "claude" }
        }));
        if items.is_empty() {
            self.push(EntryKind::Info, "no saved sessions yet — Beyond and Claude Code sessions show up here");
        } else {
            self.popup = Popup::SessionPicker { items, sel: 0 };
        }
    }

    /// Resume a Beyond session, or import a Claude Code one, by (partial) id.
    pub(super) fn load_session(&mut self, id: &str) {
        if self.agent.is_some() {
            self.push(EntryKind::Warn, "wait for the current turn to finish (Esc interrupts it)");
            return;
        }
        let loaded = sessions::load_beyond(id).or_else(|_| {
            let needle = id.strip_prefix("cc-").unwrap_or(id);
            sessions::scan_claude_sessions()
                .into_iter()
                .find(|s| s.id.starts_with(needle))
                .ok_or_else(|| anyhow::anyhow!("no Beyond or Claude Code session matches '{id}'"))
                .and_then(|s| sessions::load_claude_session(&s))
        });
        let session = match loaded {
            Ok(s) => s,
            Err(e) => {
                self.push(EntryKind::Error, format!("{e:#}"));
                return;
            }
        };

        // Rebuild the visible transcript from the stored messages.
        self.entries.clear();
        for m in &session.messages {
            for block in &m.content {
                match (m.role, block) {
                    (Role::User, ContentBlock::Text { text }) => {
                        self.entries.push(Entry::new(EntryKind::User, text.clone()))
                    }
                    (Role::Assistant, ContentBlock::Text { text }) => {
                        self.entries.push(Entry::new(EntryKind::Assistant, text.clone()))
                    }
                    (_, ContentBlock::ToolUse { name, .. }) => self.entries.push(Entry::new(
                        EntryKind::Tool { name: name.clone(), state: ToolState::Ok },
                        "",
                    )),
                    _ => {} // tool results stay out of the transcript
                }
            }
        }
        if let Some(dir) = session.cwd.as_deref().map(PathBuf::from).filter(|p| p.is_dir()) {
            self.project = Some(dir);
        }
        self.messages = session.messages.clone();
        self.session_tokens = 0;
        self.session_cost = 0.0;
        let note = format!("resumed {} — {} ({} messages)", session.id, session.title, session.messages.len());
        self.session = session;
        self.push(EntryKind::Success, note);
    }

    // ------------------------------------------------------------- /later

    /// `/later [when] <task>`: schedule a reminder and show what's left today.
    fn cmd_later(&mut self, args: Option<String>) {
        let usage = self.usage_line();
        let Some(args) = args else {
            let mut text = String::from("**Reminders**\n");
            let pending: Vec<_> = notify::list().into_iter().filter(|r| !r.fired).collect();
            if pending.is_empty() {
                text.push_str("nothing scheduled\n");
            }
            for r in pending {
                text.push_str(&format!("{} — {}\n", r.when.format("%a %d %b %H:%M"), r.task));
            }
            text.push_str(&format!(
                "\n{usage}\n\n`/later tomorrow 9am finish the parser` · `/later in 2h run the tests` · \
                 `/later next week …` · `/later clear`"
            ));
            self.push(EntryKind::Info, text);
            return;
        };
        if args.trim() == "clear" {
            let n = notify::clear();
            self.push(EntryKind::Success, format!("cleared {n} reminder(s)"));
            return;
        }

        // The longest leading run of words that parses as a time is the
        // "when"; the rest is the task. No time given means tomorrow morning.
        let words: Vec<&str> = args.split_whitespace().collect();
        let split = (1..=words.len().min(4))
            .rev()
            .find(|n| notify::parse_when(&words[..*n].join(" ")).is_ok())
            .unwrap_or(0);
        let when = if split == 0 { "tomorrow".to_string() } else { words[..split].join(" ") };
        let mut task = words[split..].join(" ");
        if task.is_empty() {
            task = format!("pick up where we left off: {}", self.session.title);
        }

        match notify::schedule(&when, &task) {
            Ok(at) => self.push(
                EntryKind::Success,
                format!(
                    "I'll remind you {} — {task}\n{usage}\n\
                     you get a Windows notification while Beyond is open; one missed while it was closed shows at next launch",
                    at.format("%A %d %b at %H:%M"),
                ),
            ),
            Err(e) => self.push(EntryKind::Error, format!("could not schedule that: {e:#}")),
        }
    }

    // -------------------------------------------------------------- usage

    /// One-line summary of today's token use against the daily budget.
    pub(super) fn usage_line(&self) -> String {
        let used = self.cfg.usage.tokens_today();
        match self.cfg.usage_budget_daily {
            Some(budget) => format!(
                "today: {} of {} tokens used — **{} left**",
                util::human_tokens(used),
                util::human_tokens(budget),
                util::human_tokens(budget.saturating_sub(used)),
            ),
            None => format!(
                "today: {} tokens used — no daily budget set (`/budget 500k` adds a 'left today' meter)",
                util::human_tokens(used)
            ),
        }
    }

    fn cmd_budget(&mut self, arg: Option<String>) {
        let Some(arg) = arg else {
            let line = self.usage_line();
            self.push(EntryKind::Info, line);
            return;
        };
        let arg = arg.trim().to_lowercase();
        let budget = if matches!(arg.as_str(), "off" | "none" | "0") {
            None
        } else {
            match parse_tokens(&arg) {
                Some(n) => Some(n),
                None => {
                    self.push(EntryKind::Error, "usage: `/budget 500k`, `/budget 2m` or `/budget off`");
                    return;
                }
            }
        };
        self.persist(|c| c.usage_budget_daily = budget);
        let line = self.usage_line();
        self.push(EntryKind::Success, line);
    }

    // ------------------------------------------------------------- window

    /// `/window [cols rows|off]`: console size applied at every launch.
    fn cmd_window(&mut self, arg: Option<String>) {
        let Some(arg) = arg else {
            let w = &self.cfg.window;
            let state = if w.enabled { format!("{}×{}", w.cols, w.rows) } else { "off (your terminal decides)".into() };
            self.push(EntryKind::Info, format!("launch window size: {state} — `/window 140 40` or `/window off`"));
            return;
        };
        if arg.trim().eq_ignore_ascii_case("off") {
            self.persist(|c| c.window.enabled = false);
            self.push(EntryKind::Success, "launch window sizing turned off");
            return;
        }
        let nums: Vec<u16> = arg
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse().ok())
            .collect();
        let [cols, rows] = nums[..] else {
            self.push(EntryKind::Error, "usage: `/window 140 40` (columns rows) or `/window off`");
            return;
        };
        if !(60..=400).contains(&cols) || !(16..=120).contains(&rows) {
            self.push(EntryKind::Error, "pick 60–400 columns and 16–120 rows");
            return;
        }
        self.persist(|c| {
            c.window.enabled = true;
            c.window.cols = cols;
            c.window.rows = rows;
        });
        let applied = crate::win::apply_window_config(&self.cfg.window);
        let note = if applied { "applied now" } else { "Windows Terminal can't be resized by an app, so this only applies in a classic console window" };
        self.push(EntryKind::Success, format!("launch window size set to {cols}×{rows} — {note}"));
    }

    // ------------------------------------------------- model, mode, backend

    /// Open a small list popup with `current` preselected.
    fn open_choice(&mut self, kind: Choice, items: Vec<(String, String)>, current: &str) {
        let sel = items.iter().position(|(id, _)| id.eq_ignore_ascii_case(current)).unwrap_or(0);
        self.popup = Popup::ChoicePicker { kind, items, sel };
    }

    pub(super) fn set_model(&mut self, model: &str) {
        let m = match claude_code::resolve_model(model) {
            Ok(m) => m,
            Err(why) => {
                self.push(EntryKind::Error, why);
                return;
            }
        };
        self.persist(|c| c.model = m.clone());
        self.session.model = m.clone();
        let label = claude_code::model_label(&m).map(|l| format!(" ({l})")).unwrap_or_default();
        self.push(EntryKind::Success, format!("model set to `{m}`{label} — used from your next message"));
    }

    pub(super) fn set_mode(&mut self, arg: &str) {
        let id = match arg.to_ascii_lowercase().as_str() {
            "plan" => "plan",
            "edits" | "acceptedits" | "edit" => "acceptEdits",
            "auto" => "auto",
            "bypass" | "bypasspermissions" | "yolo" => "bypassPermissions",
            _ => {
                self.push(EntryKind::Error, "usage: `/mode plan`, `/mode edits`, `/mode auto` or `/mode bypass`");
                return;
            }
        };
        self.persist(|c| c.permission_mode = id.to_string());
        let what = claude_code::PERMISSION_MODES.iter().find(|(m, _)| *m == id).map(|(_, d)| *d).unwrap_or("");
        let mut note = format!("mode: `{id}` — {what}");
        if !self.uses_claude() {
            note.push_str("\n(this applies when Beyond runs on Claude Code; with an API key Beyond asks before each risky action)");
        }
        self.push(EntryKind::Success, note);
    }

    /// One line on which engine answers messages right now, and why.
    fn backend_line(&self) -> String {
        if self.uses_claude() {
            let who = match &self.claude_auth {
                Some(a) if a.is_subscription() => match &a.plan {
                    Some(plan) => format!("signed in with your Claude plan ({plan})"),
                    None => "signed in with your Claude plan".to_string(),
                },
                Some(a) if a.logged_in => format!("signed in ({})", a.method),
                Some(_) => "not signed in yet — run `/login`".to_string(),
                None => "checking its sign-in…".to_string(),
            };
            format!("running on **Claude Code** — {who}")
        } else if self.cfg.has_auth() {
            "running on your **API key** (billed per token by Anthropic)".to_string()
        } else if self.claude_cli.is_none() {
            "not signed in — Claude Code isn't installed, so use `/login key`, or install it and run `/login`".to_string()
        } else {
            "not signed in — run `/login`".to_string()
        }
    }

    fn cmd_backend(&mut self, arg: Option<String>) {
        let Some(arg) = arg else {
            let line = self.backend_line();
            self.push(
                EntryKind::Info,
                format!("{line}\n`/backend auto` picks Claude Code when it is signed in · `/backend claude` · `/backend api`"),
            );
            return;
        };
        let backend = match arg.trim().to_ascii_lowercase().as_str() {
            "auto" => Backend::Auto,
            "claude" | "claudecode" | "cc" => Backend::Claude,
            "api" | "key" => Backend::Api,
            _ => {
                self.push(EntryKind::Error, "usage: `/backend auto`, `/backend claude` or `/backend api`");
                return;
            }
        };
        if backend == Backend::Claude && self.claude_cli.is_none() {
            self.push(EntryKind::Error, "Claude Code isn't installed (no `claude` on your PATH)");
            return;
        }
        self.persist(|c| c.backend = backend);
        let line = self.backend_line();
        self.push(EntryKind::Success, line);
    }

    // -------------------------------------------------------------- usage

    /// `/usage`: tokens and cost, and who is paying for them.
    fn cmd_usage(&mut self) {
        let mut text = self.usage_line();
        text.push_str(&format!("\nthis session: {} tokens", util::human_tokens(self.session_tokens)));
        let (today, session) = (self.cfg.usage.cost_today(), self.session_cost);
        if today > 0.0 || session > 0.0 {
            let on_plan = self.uses_claude() && self.claude_auth.as_ref().is_some_and(|a| a.is_subscription());
            if on_plan {
                text.push_str(&format!(
                    "\nvalue at API prices: ${session:.2} this session · ${today:.2} today — **covered by your Claude plan**, nothing extra is charged"
                ));
            } else {
                text.push_str(&format!("\ncost: **${session:.2}** this session · **${today:.2}** today (billed by Anthropic)"));
            }
        } else if !self.uses_claude() && self.cfg.has_auth() {
            text.push_str("\ncost isn't tracked on the API-key backend — see console.anthropic.com for your bill");
        }
        text.push('\n');
        text.push_str(&self.backend_line());
        self.push(EntryKind::Info, text);
    }

    // -------------------------------------------------------------- login

    /// `/login`: sign in through Claude Code. Anthropic's own client does the
    /// signing in, in its own window; Beyond only waits for it to finish.
    fn login_with_claude(&mut self) {
        let Some(cli) = self.claude_cli.clone() else {
            self.push(
                EntryKind::Info,
                "Claude Code isn't installed, so there is no Claude sign-in to share.\n\
                 Install it (`npm install -g @anthropic-ai/claude-code`), restart Beyond and run `/login` — \
                 or use an Anthropic API key with `/login key`.",
            );
            return;
        };
        if self.claude_auth.as_ref().is_some_and(|a| a.logged_in) {
            let line = self.backend_line();
            self.push(EntryKind::Success, format!("already signed in — {line}"));
            return;
        }
        match claude_code::open_login_window(&cli) {
            Ok(()) => {
                // remember "signed out" so the success note shows when it flips
                self.claude_auth.get_or_insert_with(Default::default);
                self.check_claude_auth(100); // keep asking for ~5 minutes
                self.push(
                    EntryKind::Info,
                    "Claude Code's sign-in opened in a new window — finish it there and Beyond picks it up by itself.\n\
                     Prefer an API key? `/login key`",
                );
            }
            Err(e) => self.push(
                EntryKind::Error,
                format!("could not open Claude Code's sign-in ({e}) — run `claude auth login` in a terminal"),
            ),
        }
    }

    /// Start signing in: open the Anthropic Console's key page in the browser
    /// (the user signs in and creates a key there), then ask for it hidden.
    fn begin_login(&mut self, limited: bool) {
        const KEYS_URL: &str = "https://console.anthropic.com/settings/keys";
        // tests must not pop a browser window
        let opened = !cfg!(test) && crate::auth::open_browser(KEYS_URL).is_ok();
        self.push(
            EntryKind::Info,
            if opened {
                format!("opened {KEYS_URL} in your browser — sign in, click **Create Key**, copy it, then paste it below")
            } else {
                format!("open {KEYS_URL}, sign in, click **Create Key**, copy it, then paste it below")
            },
        );
        self.prompt = Some(Prompt::ApiKey { limited });
    }

    /// The hidden input box was submitted.
    pub(super) fn finish_prompt(&mut self, prompt: Prompt, text: String) {
        let Prompt::ApiKey { limited } = prompt;
        if text.is_empty() {
            self.push(EntryKind::Info, "cancelled");
            return;
        }
        if !text.starts_with("sk-ant-") {
            self.push(
                EntryKind::Error,
                "that doesn't look like an Anthropic API key (they start with `sk-ant-`) — nothing was saved",
            );
            return;
        }
        let expires = limited.then(|| chrono::Utc::now().timestamp().max(0) as u64 + LIMITED_LOGIN_SECS);
        self.persist(|c| {
            c.api_key = Some(text.clone());
            c.oauth = None;
            c.auth_expires_at = expires;
        });
        self.push(
            EntryKind::Success,
            if limited {
                "signed in for 48 hours — the key is wiped from this PC after that (or with `/logout`)"
            } else {
                "signed in — the key is stored in your Beyond config (`/logout` removes it)"
            },
        );
    }
}

/// Parse `500k`, `2m`, `1.5m` or a plain number into a token count.
fn parse_tokens(s: &str) -> Option<u64> {
    let (num, mult) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1_000.0),
        'm' => (&s[..s.len() - 1], 1_000_000.0),
        _ => (s, 1.0),
    };
    let n: f64 = num.trim().replace([',', '_'], "").parse().ok()?;
    (n > 0.0).then_some((n * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::parse_tokens;

    #[test]
    fn token_amounts() {
        assert_eq!(parse_tokens("500k"), Some(500_000));
        assert_eq!(parse_tokens("1.5m"), Some(1_500_000));
        assert_eq!(parse_tokens("250000"), Some(250_000));
        assert_eq!(parse_tokens("lots"), None);
        assert_eq!(parse_tokens("0"), None);
    }
}
