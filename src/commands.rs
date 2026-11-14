//! Slash-command registry + parsing.
//!
//! Commands are declarative (`CmdDef`) so `/help` and the autocomplete popup
//! stay in sync automatically. [`dispatch`] turns a raw input line into a
//! [`Cmd`] which `ui::app::App::apply_cmd` executes against app state.

/// A command definition (drives /help + autocomplete).
pub struct CmdDef {
    pub name: &'static str,
    pub args: &'static str,
    pub desc: &'static str,
}

/// All commands, in display order.
pub fn registry() -> &'static [CmdDef] {
    CMDS
}

static CMDS: &[CmdDef] = &[
    CmdDef { name: "help", args: "[command]", desc: "show this help or details for one command" },
    CmdDef { name: "theme", args: "[name]", desc: "switch theme (no arg opens the picker)" },
    CmdDef { name: "loadtheme", args: "<file.json>", desc: "load a custom color theme from a JSON file" },
    CmdDef { name: "project", args: "[directory]", desc: "tag a project as [@/path] (no arg opens the folder browser)" },
    CmdDef { name: "env", args: "", desc: "set up your environment: detect + install toolchains for your repos" },
    CmdDef { name: "loadsession", args: "[id]", desc: "resume a Beyond or Claude Code session (no arg lists them)" },
    CmdDef { name: "later", args: "<when> <task>", desc: "schedule a reminder, e.g. /later tomorrow 9am run the tests — shows your usage left today" },
    CmdDef { name: "model", args: "[name]", desc: "pick the Claude model (no arg opens the picker)" },
    CmdDef { name: "mode", args: "[plan|edits|auto|bypass]", desc: "what Claude Code may do without asking (no arg opens the picker)" },
    CmdDef { name: "backend", args: "[auto|claude|api]", desc: "run on your Claude Code sign-in or on an API key" },
    CmdDef { name: "login", args: "[key]", desc: "sign in through Claude Code; `/login key` uses an Anthropic API key instead" },
    CmdDef { name: "logout", args: "", desc: "remove the stored API key" },
    CmdDef { name: "voice", args: "", desc: "dictate a message with your microphone (also F9) — experimental" },
    CmdDef { name: "limitedlogin", args: "", desc: "sign in for 48 hours only (use for shared PCs)" },
    CmdDef { name: "usage", args: "", desc: "tokens and cost for today and this session, and who pays" },
    CmdDef { name: "budget", args: "[tokens|off]", desc: "set a daily token budget, e.g. /budget 500k (drives the 'left today' meter)" },
    CmdDef { name: "window", args: "[cols rows|off]", desc: "console window size applied at launch, e.g. /window 140 40" },
    CmdDef { name: "clear", args: "", desc: "clear the conversation view" },
    CmdDef { name: "setup", args: "", desc: "re-run the setup wizard (API key, model, theme, window)" },
    CmdDef { name: "quit", args: "", desc: "exit Beyond CLI" },
];

/// Parsed command result.
#[derive(Debug, Clone)]
pub enum Cmd {
    Help(Option<String>),
    Theme(Option<String>),
    LoadTheme(String),
    Project(Option<String>),
    Env,
    LoadSession(Option<String>),
    Later(Option<String>),
    Model(Option<String>),
    /// `true` = sign in with an API key instead of Claude Code.
    Login(bool),
    Logout,
    Mode(Option<String>),
    Backend(Option<String>),
    Voice,
    LimitedLogin,
    Usage,
    Budget(Option<String>),
    Window(Option<String>),
    Clear,
    Setup,
    Quit,
    Unknown(String),
}

/// Split "/name rest-of-line".
pub fn parse(line: &str) -> Option<(&str, &str)> {
    let t = line.trim();
    let t = t.strip_prefix('/')?;
    match t.find(char::is_whitespace) {
        Some(i) => Some((&t[..i], t[i..].trim())),
        None => Some((t, "")),
    }
}

/// Parse a full input line into a [`Cmd`]. Non-slash input never reaches here.
pub fn dispatch(line: &str) -> Cmd {
    let Some((name, args)) = parse(line) else {
        return Cmd::Unknown(line.to_string());
    };
    match name.to_ascii_lowercase().as_str() {
        "help" | "?" => Cmd::Help((!args.is_empty()).then(|| args.to_string())),
        "theme" => Cmd::Theme((!args.is_empty()).then(|| args.to_string())),
        "loadtheme" => {
            if args.is_empty() {
                Cmd::Unknown("usage: /loadtheme <file.json>".into())
            } else {
                Cmd::LoadTheme(args.to_string())
            }
        }
        "project" => Cmd::Project((!args.is_empty()).then(|| args.to_string())),
        "env" | "environment" => Cmd::Env,
        "loadsession" | "session" => Cmd::LoadSession((!args.is_empty()).then(|| args.to_string())),
        "later" | "remind" => Cmd::Later((!args.is_empty()).then(|| args.to_string())),
        "model" => Cmd::Model((!args.is_empty()).then(|| args.to_string())),
        "login" => Cmd::Login(matches!(args.to_ascii_lowercase().as_str(), "key" | "api" | "apikey")),
        "mode" | "permissions" => Cmd::Mode((!args.is_empty()).then(|| args.to_string())),
        "backend" => Cmd::Backend((!args.is_empty()).then(|| args.to_string())),
        "logout" => Cmd::Logout,
        "voice" | "dictate" => Cmd::Voice,
        "limitedlogin" => Cmd::LimitedLogin,
        "usage" => Cmd::Usage,
        "budget" => Cmd::Budget((!args.is_empty()).then(|| args.to_string())),
        "window" => Cmd::Window((!args.is_empty()).then(|| args.to_string())),
        "clear" => Cmd::Clear,
        "setup" => Cmd::Setup,
        "quit" | "exit" => Cmd::Quit,
        other => Cmd::Unknown(format!("/{other}")),
    }
}
