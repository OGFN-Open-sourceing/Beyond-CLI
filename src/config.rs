//! Persistent user configuration and filesystem layout.
//!
//! Everything lives in two roots:
//! - `<config>/beyond-cli/` — `config.json`, `sessions/`, `themes/`, `reminders.json`
//! - `<local data>/beyond-cli/ExtraResources/` — `MCPs/`, `Skills/`, `dumps/`
//!
//! All `Config` fields use `#[serde(default)]` so an old or hand-edited
//! config always deserializes; a corrupt file silently falls back to
//! defaults rather than crashing the app.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// Console window sizing applied at launch (the "window size" setting).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowConfig {
    /// Disabled by default so the user's console profile wins on first run.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// Center the console window on the primary monitor.
    #[serde(default = "default_true")]
    pub center: bool,
}

fn default_cols() -> u16 {
    140
}
fn default_rows() -> u16 {
    40
}
fn default_true() -> bool {
    true
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self { enabled: false, cols: default_cols(), rows: default_rows(), center: true }
    }
}

/// How a tool category is permissioned when the agent wants to use it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PermMode {
    /// Prompt the user every time (TUI confirm popup).
    Ask,
    /// Run without asking; remembered for the session.
    Always,
    /// Refuse automatically.
    Deny,
}

/// Per-tool-category permission modes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permissions {
    /// Applies to the `bash` tool (shell commands).
    pub bash: PermMode,
    /// Applies to the `write_file` / `edit_file` tools.
    pub write: PermMode,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { bash: PermMode::Ask, write: PermMode::Ask }
    }
}

/// Daily token accounting for the usage budget (persisted in config.json).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageState {
    /// Local date (YYYY-MM-DD) the counter belongs to.
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub tokens_today: u64,
    /// What today's turns cost at API prices, in US dollars.
    #[serde(default)]
    pub cost_today: f64,
}

/// Which engine answers messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// Claude Code when it is installed and signed in, otherwise the API key.
    #[default]
    Auto,
    /// Always run through the installed Claude Code CLI.
    Claude,
    /// Always call the API directly with the stored key.
    Api,
}

impl UsageState {
    /// Record `tokens` against today, rolling the counter over at midnight.
    pub fn add(&mut self, tokens: u64) {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        if self.date != today {
            self.date = today;
            self.tokens_today = 0;
            self.cost_today = 0.0;
        }
        self.tokens_today += tokens;
    }
    /// Record a finished turn: tokens plus what it cost at API prices.
    pub fn add_turn(&mut self, tokens: u64, cost_usd: f64) {
        self.add(tokens);
        self.cost_today += cost_usd.max(0.0);
    }
    /// Today's cost; 0 if the stored date is stale (different day).
    pub fn cost_today(&self) -> f64 {
        if self.tokens_today() == 0 { 0.0 } else { self.cost_today }
    }
    /// Tokens left in today's budget (`None` when no budget is configured).
    pub fn left_today(&self, budget: Option<u64>) -> Option<u64> {
        budget.map(|b| b.saturating_sub(self.tokens_today()))
    }
    /// Today's counter; 0 if the stored date is stale (different day).
    pub fn tokens_today(&self) -> u64 {
        if self.date == chrono::Local::now().format("%Y-%m-%d").to_string() {
            self.tokens_today
        } else {
            0
        }
    }
}

/// What the `/env` wizard learned about the user's machine setup.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvProfile {
    /// Project folders the user tagged as "work here".
    #[serde(default)]
    pub repos: Vec<String>,
    /// Toolchains the wizard (or agent) installed on the user's behalf.
    #[serde(default)]
    pub installed: Vec<String>,
    /// Human-readable timestamp of the last completed setup run.
    #[serde(default)]
    pub last_setup: Option<String>,
}

/// The whole persisted configuration (`config.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Anthropic API key; falls back to `ANTHROPIC_API_KEY` when `None`.
    #[serde(default)]
    pub api_key: Option<String>,
    // (Older builds stored Claude account tokens under `oauth`; serde skips
    // that field now and the next save drops it from the file.)
    /// Unix seconds after which stored credentials are wiped (`/limitedlogin`
    /// sets this to now + 48 h; `None` = the login never expires).
    #[serde(default)]
    pub auth_expires_at: Option<u64>,
    #[serde(default = "default_model")]
    pub model: String,
    /// Theme id: a builtin ("beyond" / "galaxy") or a JSON file stem.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Per-request `max_tokens` for the Messages API.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub window: WindowConfig,
    #[serde(default)]
    pub permissions: Permissions,
    /// Optional daily token budget for the status bar meter.
    #[serde(default)]
    pub usage_budget_daily: Option<u64>,
    #[serde(default)]
    pub usage: UsageState,
    /// Set once the first-run setup wizard has completed.
    #[serde(default)]
    pub wizard_done: bool,
    #[serde(default)]
    pub env: EnvProfile,
    /// Free-form extra instructions injected into the system prompt.
    #[serde(default)]
    pub system_extras: Vec<String>,
    /// Splash + UI animation master switch.
    #[serde(default = "default_true")]
    pub animations: bool,
    /// Which engine answers messages (`/backend`).
    #[serde(default)]
    pub backend: Backend,
    /// Claude Code permission mode used for its tools (`/mode`).
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
}

fn default_permission_mode() -> String {
    "acceptEdits".into()
}
fn default_model() -> String {
    // an alias: always the newest Sonnet (mapped to a full id on the API backend)
    "sonnet".into()
}
fn default_theme() -> String {
    "beyond".into()
}
fn default_max_tokens() -> u32 {
    16384
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            auth_expires_at: None,
            model: default_model(),
            theme: default_theme(),
            max_tokens: default_max_tokens(),
            window: WindowConfig::default(),
            permissions: Permissions::default(),
            usage_budget_daily: None,
            usage: UsageState::default(),
            wizard_done: false,
            env: EnvProfile::default(),
            system_extras: vec![],
            animations: true,
            backend: Backend::default(),
            permission_mode: default_permission_mode(),
        }
    }
}

impl Config {
    /// Is an API key stored (Beyond's only credential of its own)?
    pub fn has_auth(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
    }

    /// True once a `/limitedlogin` window has run out.
    pub fn auth_expired(&self) -> bool {
        self.auth_expires_at
            .is_some_and(|t| chrono::Utc::now().timestamp().max(0) as u64 >= t)
    }

    /// Forget every stored credential (used by `/logout` and login expiry).
    pub fn clear_auth(&mut self) {
        self.api_key = None;
        self.auth_expires_at = None;
    }
}

/// Filesystem layout: every path the app touches comes from here.
pub struct Paths;

impl Paths {
    /// e.g. `C:\Users\me\AppData\Roaming\beyond-cli`
    pub fn config_dir() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("beyond-cli")
    }
    pub fn config_file() -> PathBuf {
        Self::config_dir().join("config.json")
    }
    /// e.g. `C:\Users\me\AppData\Local\beyond-cli`
    pub fn data_dir() -> PathBuf {
        dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("beyond-cli")
    }
    pub fn sessions_dir() -> PathBuf {
        Self::config_dir().join("sessions")
    }
    pub fn reminders_file() -> PathBuf {
        Self::config_dir().join("reminders.json")
    }
    /// Where `/loadtheme <file>` looks (file name without extension).
    pub fn custom_themes_dir() -> PathBuf {
        Self::config_dir().join("themes")
    }
    /// Root of the `MCPs/` + `Skills/` + `dumps/` scaffold.
    pub fn extra_resources_dir() -> PathBuf {
        Self::data_dir().join("ExtraResources")
    }
    pub fn mcps_dir() -> PathBuf {
        Self::extra_resources_dir().join("MCPs")
    }
    pub fn skills_dir() -> PathBuf {
        Self::extra_resources_dir().join("Skills")
    }
    pub fn dumps_dir() -> PathBuf {
        Self::extra_resources_dir().join("dumps")
    }

    /// Create every directory the app expects (idempotent, run at boot).
    pub fn ensure_all() -> Result<()> {
        for d in [
            Self::config_dir(),
            Self::sessions_dir(),
            Self::custom_themes_dir(),
            Self::extra_resources_dir(),
            Self::mcps_dir(),
            Self::skills_dir(),
            Self::dumps_dir(),
        ] {
            std::fs::create_dir_all(&d)
                .with_context(|| format!("creating dir {}", d.display()))?;
        }
        Ok(())
    }
}

/// Process-wide copy of the current config, for tools that need a field
/// without plumbing `Config` through (kept in sync by `load`/`save`).
static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

/// Load `config.json`, creating a fresh one on first run.
pub fn load() -> Result<Config> {
    Paths::ensure_all()?;
    let file = Paths::config_file();
    let cfg = if file.exists() {
        let raw = std::fs::read_to_string(&file).context("reading config.json")?;
        match serde_json::from_str(&raw) {
            Ok(cfg) => cfg,
            Err(_) => {
                // corrupt file → defaults instead of a crash, but keep the
                // original so the next save doesn't destroy the user's settings
                let _ = std::fs::copy(&file, file.with_extension("json.broken"));
                Config::default()
            }
        }
    } else {
        let cfg = Config::default();
        write_atomic(&file, &serde_json::to_string_pretty(&cfg)?)?;
        cfg
    };
    *CONFIG.lock().unwrap() = Some(cfg.clone());
    Ok(cfg)
}

/// Persist the config and refresh the process-wide copy.
pub fn save(cfg: &Config) -> Result<()> {
    let file = Paths::config_file();
    std::fs::create_dir_all(file.parent().unwrap())?;
    write_atomic(&file, &serde_json::to_string_pretty(cfg)?)?;
    *CONFIG.lock().unwrap() = Some(cfg.clone());
    Ok(())
}

/// Write to a temp file, then rename over the target: a crash or full disk
/// mid-write leaves the old config intact instead of a truncated one.
fn write_atomic(file: &std::path::Path, contents: &str) -> Result<()> {
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("replacing {}", file.display()))?;
    Ok(())
}

/// Apply an in-place change to the current config and persist it.
///
/// Mutates the *process-wide* copy rather than a caller's possibly stale
/// clone, so concurrent changes to other fields aren't clobbered.
pub fn update(f: impl FnOnce(&mut Config)) -> Result<()> {
    let mut guard = CONFIG.lock().unwrap();
    let cfg = guard.get_or_insert_with(Config::default);
    f(cfg);
    let cfg = cfg.clone();
    drop(guard);
    save(&cfg)
}

/// Extra system prompt lines a user can set for the agent.
pub fn system_extras_lines(cfg: &Config) -> String {
    cfg.system_extras.join("\n")
}
