//! Claude Code backend: Beyond as a front end for the official `claude` CLI.
//!
//! When Claude Code is installed and signed in, Beyond doesn't talk to the
//! API itself. Each turn runs the real CLI headless
//! (`claude -p --output-format stream-json`) and translates the JSON lines it
//! prints into the same [`AgentEvent`]s the built-in agent emits. Signing in,
//! tools, permissions and billing are all Claude Code's own — Beyond never
//! sees or stores a credential on this path.

use super::{AgentEvent, Usage};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Don't flash a console window when spawning helper processes.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Every model `/model` offers: (what is sent to Claude, description).
/// The four bare family names are aliases that always mean the newest
/// version; the rest pin one exact version.
pub const MODELS: [(&str, &str); 19] = [
    ("fable", "always the newest Fable — most capable"),
    ("opus", "always the newest Opus — deep reasoning"),
    ("sonnet", "always the newest Sonnet — fast and capable"),
    ("haiku", "always the newest Haiku — fastest, cheapest"),
    ("claude-fable-5-2", "Fable 5.2 — hardest problems, long tasks"),
    ("claude-fable-5-1", "Fable 5.1"),
    ("claude-fable-5", "Fable 5"),
    ("claude-opus-5-5", "Opus 5.5 — current Opus"),
    ("claude-opus-5", "Opus 5"),
    ("claude-opus-4-8", "Opus 4.8"),
    ("claude-opus-4-7", "Opus 4.7"),
    ("claude-opus-4-6", "Opus 4.6"),
    ("claude-opus-4-5", "Opus 4.5"),
    ("claude-sonnet-5-5", "Sonnet 5.5 — current Sonnet"),
    ("claude-sonnet-5", "Sonnet 5"),
    ("claude-sonnet-4-6", "Sonnet 4.6"),
    ("claude-sonnet-4-5", "Sonnet 4.5"),
    ("claude-haiku-5-5", "Haiku 5.5 — current Haiku"),
    ("claude-haiku-4-5", "Haiku 4.5"),
];

const FAMILIES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// Turn what the user typed after `/model` into a model name.
///
/// Forgiving on purpose: `opus 5.5`, `opus5.5`, `Opus-5-5`, `claude opus 5.5`
/// and `claude-opus-5-5` all mean the same thing. A full `claude-…` id that
/// isn't in the list is passed through untouched, so brand-new models work
/// without an update. Anything else is an error that says what is available.
pub fn resolve_model(input: &str) -> Result<String, String> {
    let raw = input.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return Err("give a model, e.g. `/model opus 5.5` — or just `/model` to pick from the list".into());
    }

    // Normalise to dash-separated words and numbers: "Opus5.5" -> "opus-5-5".
    let mut key = String::new();
    let mut prev: Option<char> = None;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            let boundary = prev.is_some_and(|p| p.is_ascii_alphabetic() != c.is_ascii_alphabetic());
            if boundary && !key.ends_with('-') {
                key.push('-');
            }
            key.push(c);
            prev = Some(c);
        } else {
            if !key.is_empty() && !key.ends_with('-') {
                key.push('-');
            }
            prev = None;
        }
    }
    let key = key.trim_matches('-');
    let key = key.strip_prefix("claude-").unwrap_or(key);

    if let Some((id, _)) = MODELS.iter().find(|(id, _)| id.strip_prefix("claude-").unwrap_or(id) == key) {
        return Ok(id.to_string());
    }

    // An exact id we don't list (a new release, a dated snapshot, `opus[1m]`).
    let safe = raw.len() <= 96 && raw.chars().all(|c| c.is_ascii_alphanumeric() || "-_.[]:".contains(c));
    if safe && (raw.starts_with("claude-") || raw.contains('[')) {
        return Ok(raw);
    }

    // Right family, wrong version: say which versions exist.
    if let Some(family) = FAMILIES.iter().find(|f| key.starts_with(**f)) {
        let prefix = format!("claude-{family}-");
        let versions: Vec<String> = MODELS
            .iter()
            .filter_map(|(id, _)| id.strip_prefix(prefix.as_str()))
            .map(|v| v.replace('-', "."))
            .collect();
        return Err(format!(
            "no such {family} version — there is {family} {} (or just `{family}` for the newest)",
            versions.join(", ")
        ));
    }
    Err("unknown model — `/model` opens the list (fable, opus, sonnet, haiku and their versions)".into())
}

/// Friendly name for a model id from the list ("Opus 5.5"), if it has one.
pub fn model_label(id: &str) -> Option<&'static str> {
    let (_, desc) = MODELS.iter().find(|(m, _)| *m == id)?;
    id.starts_with("claude-").then(|| desc.split(" — ").next().unwrap_or(desc))
}

/// Permission modes offered by `/mode` (id, what it means).
pub const PERMISSION_MODES: [(&str, &str); 4] = [
    ("plan", "read-only: Claude explores and proposes, changes nothing"),
    ("acceptEdits", "Claude may edit files in the project; other actions are denied"),
    ("auto", "Claude Code decides what is safe to run on its own"),
    ("bypassPermissions", "no checks at all — only for throwaway folders"),
];

// ------------------------------------------------------------- discovery

/// Locate the `claude` launcher on PATH (native exe first, then npm's shim).
pub fn find_cli() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: &[&str] = if cfg!(windows) { &["claude.exe", "claude.cmd"] } else { &["claude"] };
    for dir in std::env::split_paths(&path) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// What `claude auth status` reports.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthStatus {
    pub logged_in: bool,
    /// e.g. `claude.ai` (subscription) or an API-key method; `none` when out.
    pub method: String,
    /// Plan name when Claude Code reports one (Pro, Max, …).
    pub plan: Option<String>,
}

impl AuthStatus {
    /// Signed in with a Claude subscription (usage is covered by the plan,
    /// not billed per token).
    pub fn is_subscription(&self) -> bool {
        self.logged_in && self.method.to_ascii_lowercase().contains("claude")
    }
}

/// Ask Claude Code whether it is signed in. Blocking (runs a process);
/// `None` when the CLI can't be run or answers something unreadable.
pub fn auth_status(cli: &Path) -> Option<AuthStatus> {
    let mut cmd = std::process::Command::new(cli);
    cmd.args(["auth", "status"]).stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // exits non-zero when signed out, but still prints the JSON
    let out = cmd.output().ok()?;
    parse_auth_status(&String::from_utf8_lossy(&out.stdout))
}

fn parse_auth_status(raw: &str) -> Option<AuthStatus> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    Some(AuthStatus {
        logged_in: v["loggedIn"].as_bool().unwrap_or(false),
        method: v["authMethod"].as_str().unwrap_or("none").to_string(),
        plan: v["subscriptionType"].as_str().map(str::to_string),
    })
}

/// Open Claude Code's own sign-in in a separate console window, so the user
/// completes it with Anthropic's client rather than inside Beyond.
pub fn open_login_window(cli: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // `start` gives the login its own window; the title argument is required
        // because the first quoted string is otherwise taken as the title.
        std::process::Command::new("cmd")
            .args(["/C", "start", "Claude Code sign-in"])
            .arg(cli)
            .args(["auth", "login"])
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(cli).args(["auth", "login"]).spawn().map(|_| ())
    }
}

// ------------------------------------------------------------------ turn

/// Everything one headless run needs.
pub struct TurnOpts {
    pub cli: PathBuf,
    pub prompt: String,
    pub model: String,
    /// Claude Code session to continue (`None` starts a new one).
    pub resume: Option<String>,
    pub cwd: Option<PathBuf>,
    pub permission_mode: String,
    /// Beyond's own context (skills, project tag, user instructions),
    /// appended to Claude Code's system prompt.
    pub system_append: String,
}

/// What a finished run reports back.
#[derive(Debug, Default)]
pub struct TurnOutcome {
    /// Session id to pass as `resume` next turn.
    pub session_id: Option<String>,
    pub usage: Usage,
    /// What the run cost at API prices (0 when unknown).
    pub cost_usd: f64,
    /// The assistant's visible text, for Beyond's own transcript.
    pub text: String,
}

/// Run one turn through Claude Code, streaming events to the UI.
pub async fn run_turn(
    opts: TurnOpts,
    events: mpsc::UnboundedSender<AgentEvent>,
    interrupt: Arc<AtomicBool>,
) -> TurnOutcome {
    let mut outcome = TurnOutcome { session_id: opts.resume.clone(), ..Default::default() };
    let fail = |msg: String| {
        let _ = events.send(AgentEvent::Failed(msg));
    };

    // The extra system prompt travels in a file: it is long, multi-line and
    // partly user-written, none of which belongs on a command line.
    let prompt_file = crate::config::Paths::data_dir().join("claude-code-context.md");
    let have_context = !opts.system_append.trim().is_empty()
        && std::fs::write(&prompt_file, &opts.system_append).is_ok();

    let mut cmd = tokio::process::Command::new(&opts.cli);
    cmd.args(["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages"]);
    cmd.args(["--permission-mode", &opts.permission_mode]);
    if is_safe_arg(&opts.model) {
        cmd.args(["--model", &opts.model]);
    }
    if let Some(id) = opts.resume.as_deref().filter(|id| is_safe_arg(id)) {
        cmd.args(["--resume", id]);
    }
    if have_context {
        cmd.arg("--append-system-prompt-file").arg(&prompt_file);
    }
    if let Some(dir) = opts.cwd.as_deref().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            fail(format!("could not start Claude Code ({}): {e}", opts.cli.display()));
            return outcome;
        }
    };

    // The message goes in on stdin (never as an argument), then EOF.
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = opts.prompt.clone();
        tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    // Collect stderr on the side so a full pipe can never stall the run.
    let stderr_task = child.stderr.take().map(|mut err| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf).await;
            buf
        })
    });

    let Some(stdout) = child.stdout.take() else {
        fail("could not read Claude Code's output".into());
        return outcome;
    };
    let mut lines = BufReader::new(stdout).lines();
    let mut state = StreamState::default();
    let mut ticker = tokio::time::interval(Duration::from_millis(120));
    let mut stopped = false;

    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        state.handle(&v, &events, &mut outcome);
                    }
                }
                _ => break, // EOF or a broken pipe: the process is done
            },
            _ = ticker.tick() => {
                if interrupt.load(Ordering::Relaxed) {
                    stopped = true;
                    kill_tree(&mut child).await;
                    break;
                }
            }
        }
    }
    let _ = child.wait().await;

    outcome.text = state.text;
    if !stopped && !state.got_result {
        // died before reporting a result: surface whatever it complained about
        let err = match stderr_task {
            Some(t) => t.await.unwrap_or_default(),
            None => String::new(),
        };
        let err = err.trim();
        fail(if err.is_empty() {
            "Claude Code stopped without an answer".to_string()
        } else {
            format!("Claude Code: {}", crate::util::one_line(err, 300))
        });
    }
    outcome
}

/// Only plain identifiers go on the command line (model names, session ids).
fn is_safe_arg(s: &str) -> bool {
    !s.is_empty() && s.len() <= 96 && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.[]:".contains(c))
}

/// Stop the CLI and everything it started (it runs as a process tree).
async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let mut kill = tokio::process::Command::new("taskkill");
        kill.args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = kill.status().await;
    }
    let _ = child.kill().await;
}

// ---------------------------------------------------------------- parsing

/// Running state while translating one run's JSON lines.
#[derive(Default)]
struct StreamState {
    /// Visible assistant text so far.
    text: String,
    /// Text arrived as live deltas, so whole-message text is a repeat.
    saw_deltas: bool,
    /// tool_use id → tool name, for matching results to calls.
    tools: HashMap<String, String>,
    started: HashSet<String>,
    got_result: bool,
}

impl StreamState {
    fn handle(&mut self, v: &Value, events: &mpsc::UnboundedSender<AgentEvent>, out: &mut TurnOutcome) {
        if let Some(id) = v["session_id"].as_str() {
            out.session_id = Some(id.to_string());
        }
        // Sub-agents report through the same stream; only their tool activity
        // is shown, their private text is not part of the answer.
        let main = v["parent_tool_use_id"].is_null();

        match v["type"].as_str().unwrap_or("") {
            "stream_event" => {
                let ev = &v["event"];
                match ev["type"].as_str().unwrap_or("") {
                    "content_block_delta" if main => {
                        if let Some(t) = ev["delta"]["text"].as_str().filter(|_| ev["delta"]["type"] == "text_delta") {
                            self.saw_deltas = true;
                            self.push_text(t, events);
                        }
                    }
                    "content_block_start" if main && ev["content_block"]["type"] == "text" => {
                        // a new paragraph after earlier text or a tool call
                        if !self.text.is_empty() && !self.text.ends_with("\n\n") {
                            self.push_text("\n\n", events);
                        }
                    }
                    "content_block_start" if ev["content_block"]["type"] == "tool_use" => {
                        if let Some(name) = ev["content_block"]["name"].as_str() {
                            let _ = events.send(AgentEvent::ToolPending { name: name.to_string() });
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str().unwrap_or("") {
                        "text" if main && !self.saw_deltas => {
                            if let Some(t) = block["text"].as_str() {
                                if !self.text.is_empty() {
                                    self.push_text("\n\n", events);
                                }
                                self.push_text(t, events);
                            }
                        }
                        "tool_use" => {
                            let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) else {
                                continue;
                            };
                            if self.started.insert(id.to_string()) {
                                self.tools.insert(id.to_string(), name.to_string());
                                let _ = events.send(AgentEvent::ToolStart {
                                    name: name.to_string(),
                                    preview: tool_preview(name, &block["input"]),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result"
                        && let Some(name) = block["tool_use_id"].as_str().and_then(|id| self.tools.remove(id))
                    {
                        let ok = !block["is_error"].as_bool().unwrap_or(false);
                        let _ = events.send(AgentEvent::ToolDone { name, ok });
                    }
                }
            }
            "result" => {
                self.got_result = true;
                let u = &v["usage"];
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                out.usage.input_tokens += n("input_tokens") + n("cache_creation_input_tokens");
                out.usage.output_tokens += n("output_tokens");
                out.cost_usd += v["total_cost_usd"].as_f64().unwrap_or(0.0);
                if v["is_error"].as_bool().unwrap_or(false) {
                    let msg = v["result"].as_str().unwrap_or("Claude Code reported an error");
                    let _ = events.send(AgentEvent::Failed(friendly_error(msg)));
                    // the error text also arrived as a fake assistant message
                    self.text.clear();
                }
            }
            _ => {}
        }
    }

    fn push_text(&mut self, t: &str, events: &mpsc::UnboundedSender<AgentEvent>) {
        self.text.push_str(t);
        let _ = events.send(AgentEvent::TextDelta(t.to_string()));
    }
}

/// Turn Claude Code's own error strings into something actionable here.
fn friendly_error(msg: &str) -> String {
    if msg.to_ascii_lowercase().contains("not logged in") {
        "Claude Code isn't signed in — run `/login`".to_string()
    } else {
        format!("Claude Code: {msg}")
    }
}

/// One-line summary of a Claude Code tool call for the activity feed.
fn tool_preview(name: &str, input: &Value) -> String {
    let field = |k: &str| input[k].as_str().map(|s| crate::util::one_line(s, 90));
    let picked = match name {
        "Bash" | "PowerShell" => field("command"),
        "Read" | "Write" | "Edit" | "NotebookEdit" => field("file_path").or_else(|| field("notebook_path")),
        "Glob" | "Grep" => field("pattern"),
        "Task" | "Agent" => field("description"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        _ => None,
    };
    picked
        .or_else(|| {
            // unknown tool: show its first short text argument
            input.as_object()?.values().find_map(|v| v.as_str()).map(|s| crate::util::one_line(s, 90))
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_are_understood_however_they_are_typed() {
        for typed in ["opus 5.5", "opus5.5", "Opus-5-5", "claude opus 5.5", "claude-opus-5-5", " OPUS 5.5 "] {
            assert_eq!(resolve_model(typed).as_deref(), Ok("claude-opus-5-5"), "{typed}");
        }
        assert_eq!(resolve_model("sonnet").as_deref(), Ok("sonnet"));
        assert_eq!(resolve_model("fable 5").as_deref(), Ok("claude-fable-5"));
        assert_eq!(resolve_model("haiku4.5").as_deref(), Ok("claude-haiku-4-5"));
        // ids we don't list pass through untouched
        assert_eq!(resolve_model("claude-opus-4-1-20250805").as_deref(), Ok("claude-opus-4-1-20250805"));
        assert_eq!(resolve_model("opus[1m]").as_deref(), Ok("opus[1m]"));
        // wrong versions and nonsense explain themselves
        assert!(resolve_model("opus 9").unwrap_err().contains("5.5"));
        assert!(resolve_model("gpt").is_err());
        assert!(resolve_model("").is_err());
        assert_eq!(model_label("claude-opus-5-5"), Some("Opus 5.5"));
        assert_eq!(model_label("opus"), None);
    }

    fn run(lines: &[&str]) -> (Vec<AgentEvent>, TurnOutcome, StreamState) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut state = StreamState::default();
        let mut out = TurnOutcome::default();
        for l in lines {
            state.handle(&serde_json::from_str(l).unwrap(), &tx, &mut out);
        }
        drop(tx);
        let mut evs = Vec::new();
        while let Ok(e) = rx.try_recv() {
            evs.push(e);
        }
        (evs, out, state)
    }

    #[test]
    fn streams_text_tools_and_cost() {
        let (evs, out, state) = run(&[
            r#"{"type":"system","subtype":"init","session_id":"abc-1"}"#,
            r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Hello"}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":false}]}}"#,
            r#"{"type":"result","is_error":false,"total_cost_usd":0.25,"usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":2}}"#,
        ]);
        assert_eq!(state.text, "Hello"); // the whole-message repeat is dropped
        assert_eq!(out.session_id.as_deref(), Some("abc-1"));
        assert_eq!((out.usage.input_tokens, out.usage.output_tokens), (12, 5));
        assert!((out.cost_usd - 0.25).abs() < 1e-9 && state.got_result);
        let starts = evs.iter().filter(|e| matches!(e, AgentEvent::ToolStart { .. })).count();
        assert_eq!(starts, 1, "a repeated tool_use block starts the tool once");
        assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { preview, .. } if preview == "cargo test")));
        assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { ok: true, .. })));
    }

    #[test]
    fn signed_out_run_becomes_a_login_hint() {
        let (evs, _, state) = run(&[
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Not logged in · Please run /login"}]}}"#,
            r#"{"type":"result","is_error":true,"result":"Not logged in · Please run /login","total_cost_usd":0,"usage":{}}"#,
        ]);
        assert!(state.text.is_empty());
        assert!(evs.iter().any(|e| matches!(e, AgentEvent::Failed(m) if m.contains("`/login`"))));
    }

    #[test]
    fn auth_status_and_arg_checks() {
        let s = parse_auth_status(r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max"}"#).unwrap();
        assert!(s.is_subscription() && s.plan.as_deref() == Some("max"));
        let out = parse_auth_status(r#"{"loggedIn":false,"authMethod":"none"}"#).unwrap();
        assert!(!out.logged_in && !out.is_subscription());
        assert!(is_safe_arg("claude-opus-4-5") && is_safe_arg("sonnet[1m]"));
        assert!(!is_safe_arg("x & calc") && !is_safe_arg(""));
    }
}
