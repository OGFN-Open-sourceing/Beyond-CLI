//! Built-in agent tools: the registry that gets sent to the Anthropic API and
//! the executor that actually runs them locally.
//!
//! Design notes:
//! - Every tool returns a [`ToolOutcome`] (plain text, error flag) which is fed
//!   back to the model as a `tool_result`.
//! - Risky tools (`bash`, file writes) are gated by [`config::Permissions`]
//!   and, in `Ask` mode, by a confirmation popup in the TUI.
//! - MCP server tools are appended to the same registry and dispatched through
//!   [`crate::mcp::McpManager`], so the model sees one uniform tool surface.

use crate::config::PermMode;
use crate::mcp::McpManager;
use crate::util;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

// ----------------------------------------------------------------- context

/// Everything a tool needs at execution time.
#[derive(Clone)]
pub struct ToolContext {
    /// Working directory for bash / file tools (project dir when set).
    pub cwd: Option<PathBuf>,
    /// Permission snapshot (may be relaxed by confirmation popups).
    pub perms: crate::config::Permissions,
    /// Where ExtraResources/Skills lives (for `write_skill`).
    pub skills_dir: PathBuf,
    /// Where process dumps go.
    pub dumps_dir: PathBuf,
    /// Handle to running MCP servers (their tools are dispatched through it).
    pub mcps: Arc<McpManager>,
    /// Channel used to ask the user for confirmation in the TUI.
    pub confirm_tx: mpsc::UnboundedSender<ConfirmRequest>,
    /// Cooperative cancel flag shared with the agent loop.
    pub interrupt: Arc<AtomicBool>,
}

/// A permission request surfaced in the UI as a Yes/No popup.
pub struct ConfirmRequest {
    pub title: String,
    pub detail: String,
    pub respond: oneshot::Sender<bool>,
}

/// Ask the user via the TUI. Falls back to `false` (deny) on timeout.
async fn ask_user(ctx: &ToolContext, title: &str, detail: &str) -> bool {
    let (tx, rx) = oneshot::channel();
    let _ = ctx.confirm_tx.send(ConfirmRequest {
        title: title.into(),
        detail: detail.into(),
        respond: tx,
    });
    // 60s to decide, then deny (the agent keeps working on other things).
    match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
        Ok(Ok(v)) => v,
        _ => false,
    }
}

// ---------------------------------------------------------------- registry

pub struct ToolRegistry {
    defs: Vec<crate::agent::types::ToolDef>,
    ctx: ToolContext,
}

impl ToolRegistry {
    pub fn new(ctx: ToolContext) -> Self {
        Self::with_allow(ctx, None)
    }

    /// Build a registry restricted to `allow` (used for dedicated subagents).
    pub fn with_allow(ctx: ToolContext, allow: Option<&[&str]>) -> Self {
        let mut defs = builtin_defs();
        defs.extend(ctx.mcps.tool_defs());
        if let Some(allow) = allow {
            defs.retain(|d| allow.contains(&d.name.as_str()));
        }
        Self { defs, ctx }
    }

    pub fn defs(&self) -> &[crate::agent::types::ToolDef] {
        &self.defs
    }

    /// Execute a tool by name (built-ins first, then MCP servers).
    pub async fn exec(
        &self,
        name: &str,
        input: Value,
    ) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        if self.ctx.interrupt.load(Ordering::Relaxed) {
            return ToolOutcome::err("interrupted by user");
        }
        match name {
            "bash" => self.t_bash(input).await,
            "read_file" => self.t_read(input).await,
            "write_file" => self.t_write(input).await,
            "edit_file" => self.t_edit(input).await,
            "glob_files" => self.t_glob(input).await,
            "grep_files" => self.t_grep(input).await,
            "process_list" => self.t_process_list(input).await,
            "process_dump" => self.t_process_dump(input).await,
            "storage_scan" => self.t_storage_scan(input).await,
            "notify" => self.t_notify(input).await,
            "schedule_reminder" => self.t_schedule(input).await,
            "write_skill" => self.t_write_skill(input).await,
            "env_detect" => self.t_env_detect().await,
            "env_install" => self.t_env_install(input).await,
            other => {
                // MCP passthrough: "mcp:<server>/<tool>"
                if let Some(rest) = other.strip_prefix("mcp:") {
                    let (server, tool) = match rest.split_once('/') {
                        Some(p) => p,
                        None => return ToolOutcome::err("malformed MCP tool name"),
                    };
                    return self.ctx.mcps.call_tool(server, tool, input).await;
                }
                ToolOutcome::err(format!("unknown tool '{other}'"))
            }
        }
    }
}

// --------------------------------------------------------- tool definitions

/// JSON-schema helper for string/number/bool properties.
fn props(p: serde_json::Map<String, Value>) -> Value {
    json!({ "type": "object", "properties": p, "additionalProperties": false })
}
fn s(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn builtin_defs() -> Vec<crate::agent::types::ToolDef> {
    use crate::agent::types::ToolDef;
    vec![
        ToolDef::new(
            "bash",
            "Run a shell command (PowerShell on Windows) in the current project directory and return its combined output. Use for builds, tests, git, package managers. Prefer targeted commands; long-running servers will time out.",
            props(json!({
                "command": s("The command to run, e.g. 'cargo test' or 'npm run build'"),
                "timeout_ms": { "type": "integer", "description": "Timeout in ms (default 120000, max 600000)" }
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "read_file",
            "Read a text file with line numbers. Supports offset/limit for paging through big files.",
            props(json!({
                "path": s("File path (absolute, ~, or relative to the project directory)"),
                "offset_line": { "type": "integer", "description": "1-based line to start from (default 1)" },
                "max_lines": { "type": "integer", "description": "Max lines to return (default 400)" }
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "write_file",
            "Create or overwrite a text file with the given content.",
            props(json!({
                "path": s("File path"),
                "content": s("Full file content to write")
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "edit_file",
            "Replace the first exact occurrence of `old_string` in a file with `new_string`. The strings must match exactly, including whitespace, and be unique in the file.",
            props(json!({
                "path": s("File path"),
                "old_string": s("Exact existing text to replace"),
                "new_string": s("Replacement text")
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "glob_files",
            "Find files by wildcard pattern (supports ** recursion). Returns matching paths.",
            props(json!({
                "pattern": s("Glob pattern like 'src/**/*.rs' or '*.json'"),
                "path": s("Base directory (default: project directory)")
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "grep_files",
            "Search file contents with a regular expression. Returns file:line: matches.",
            props(json!({
                "pattern": s("Rust regex, e.g. 'fn main|TODO'"),
                "path": s("Directory to search (default: project directory)"),
                "file_glob": s("Only search files matching this glob, e.g. '*.ts'")
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "process_list",
            "List top processes by CPU or memory usage. Use this to check machine load before launching heavy builds/tests, and to investigate what is slowing the system.",
            props(json!({
                "limit": { "type": "integer", "description": "How many processes (default 15)" },
                "sort_by": { "type": "string", "enum": ["cpu", "memory"], "description": "Sort key (default cpu)" }
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "process_dump",
            "Dump full system + process info to a JSON file in ExtraResources/dumps for later analysis or sharing.",
            props(json!({
                "sort_by": { "type": "string", "enum": ["cpu", "memory"], "description": "Sort processes (default memory)" }
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "storage_scan",
            "Show disk usage and the biggest folders under a path. Use for storage management and cleanup planning.",
            props(json!({
                "path": s("Directory to analyze (default: user home)")
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "notify",
            "Send a Windows notification (toast) to the user right now.",
            props(json!({
                "title": s("Notification title"),
                "message": s("Notification body")
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "schedule_reminder",
            "Schedule a Windows notification for later. `when` accepts natural language: 'tomorrow 9am', 'next week', 'in 2h30m', 'friday 18:00', or ISO '2026-01-01 14:00'.",
            props(json!({
                "when": s("When the reminder should fire"),
                "task": s("What to remind about")
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "write_skill",
            "Create or update a Skill (.md) in ExtraResources/Skills so future sessions learn it. Write skills for recurring workflows, project conventions, and useful procedures you discover.",
            props(json!({
                "name": s("Short skill id, e.g. 'rust-testing'"),
                "description": s("One-line description shown in /help"),
                "markdown": s("Full skill content in markdown")
            }).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "env_detect",
            "Detect installed coding toolchains (node, python, rust, go, dotnet, java, git, pnpm, bun). Returns versions for installed ones and flags missing ones.",
            props(json!({}).as_object().unwrap().clone()),
        ),
        ToolDef::new(
            "env_install",
            "Install a missing toolchain via winget/npm (requires approval). Supported: node, python, rust, go, dotnet, java, git, pnpm, bun.",
            props(json!({
                "name": s("Toolchain name, e.g. 'node' or 'rust'")
            }).as_object().unwrap().clone()),
        ),
    ]
}

// ------------------------------------------------------------ permissions

// --------------------------------------------------------------- executors

impl ToolRegistry {
    /// Resolve a possibly-relative/~ path against the working directory.
    fn resolve(&self, p: &str) -> PathBuf {
        let path = util::expand_path(p);
        if path.is_absolute() {
            path
        } else if let Some(cwd) = &self.ctx.cwd {
            cwd.join(path)
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path)
        }
    }

    fn cwd(&self) -> PathBuf {
        self.ctx.cwd.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    }

    async fn t_bash(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let command = input["command"].as_str().unwrap_or_default().to_string();
        if command.trim().is_empty() {
            return ToolOutcome::err("missing 'command'");
        }
        // Permission gate
        let allowed = match self.ctx.perms.bash {
            PermMode::Always => true,
            PermMode::Deny => false,
            PermMode::Ask => {
                ask_user(&self.ctx, "Run command", &util::one_line(&command, 120)).await
            }
        };
        if !allowed {
            return ToolOutcome::err("command denied by user (permissions)");
        }

        let timeout = input["timeout_ms"].as_u64().unwrap_or(120_000).min(600_000);
        let cwd = self.cwd();

        let mut cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", &command]);
            c
        };
        cmd.current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());
        // kill the child when the timeout future is dropped, otherwise
        // runaway processes keep running in the background
        cmd.kill_on_drop(true);

        match tokio::time::timeout(
            std::time::Duration::from_millis(timeout),
            cmd.output(),
        )
        .await
        {
            Err(_) => ToolOutcome::err(format!("command timed out after {timeout}ms")),
            Ok(Err(e)) => ToolOutcome::err(format!("failed to spawn: {e}")),
            Ok(Ok(out)) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut text = String::new();
                if !stdout.is_empty() {
                    text.push_str(&util::truncate_chars(&stdout, 20_000));
                }
                if !stderr.is_empty() {
                    text.push_str("\n[stderr]\n");
                    text.push_str(&util::truncate_chars(&stderr, 8_000));
                }
                if text.trim().is_empty() {
                    text = format!("(no output, exit code {})", out.status.code().unwrap_or(0));
                }
                ToolOutcome::ok(text)
            }
        }
    }

    async fn t_read(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let Some(path) = input["path"].as_str() else {
            return ToolOutcome::err("missing 'path'");
        };
        let path = self.resolve(path);
        // refuse huge files up front — reading a multi-GB binary would
        // blow up the context window and freeze the UI
        let size = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
        const MAX_READ: u64 = 2 * 1024 * 1024;
        if size > MAX_READ {
            return ToolOutcome::err(format!(
                "{}: file is {} — over the {} read limit",
                path.display(),
                util::human_size(size),
                util::human_size(MAX_READ)
            ));
        }
        let data = match tokio::fs::read(&path).await {
            Ok(d) => d,
            Err(e) => return ToolOutcome::err(format!("{}: {e}", path.display())),
        };
        let text = String::from_utf8_lossy(&data);
        let offset = input["offset_line"].as_u64().unwrap_or(1).max(1) as usize;
        let max = input["max_lines"].as_u64().unwrap_or(400).min(2000) as usize;
        let lines: Vec<&str> = text.lines().collect();
        let slice: Vec<String> = lines
            .iter()
            .skip(offset - 1)
            .take(max)
            .enumerate()
            .map(|(i, l)| format!("{}\t{}", offset + i, l))
            .collect();
        if slice.is_empty() {
            return ToolOutcome::ok(format!("(file has {} lines; offset {offset} is past the end)", lines.len()));
        }
        let mut out = slice.join("\n");
        if lines.len() > offset - 1 + max {
            out.push_str(&format!("\n… ({} more lines, use offset_line {})", lines.len() - (offset - 1 + max), offset + max));
        }
        ToolOutcome::ok(out)
    }

    async fn t_write(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let (Some(path), Some(content)) = (input["path"].as_str(), input["content"].as_str()) else {
            return ToolOutcome::err("missing 'path' or 'content'");
        };
        let path = self.resolve(path);
        let allowed = match self.ctx.perms.write {
            PermMode::Always => true,
            PermMode::Deny => false,
            PermMode::Ask => {
                ask_user(&self.ctx, "Write file", &util::truncate_chars(path.to_string_lossy().as_ref(), 120)).await
            }
        };
        if !allowed {
            return ToolOutcome::err("write denied by user (permissions)");
        }
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(&path, content).await {
            Ok(_) => ToolOutcome::ok(format!("wrote {} ({} bytes)", path.display(), content.len())),
            Err(e) => ToolOutcome::err(format!("{}: {e}", path.display())),
        }
    }

    async fn t_edit(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let (Some(path), Some(old), Some(new)) = (
            input["path"].as_str(),
            input["old_string"].as_str(),
            input["new_string"].as_str(),
        ) else {
            return ToolOutcome::err("missing 'path', 'old_string' or 'new_string'");
        };
        let path = self.resolve(path);
        let data = match tokio::fs::read(&path).await {
            Ok(d) => d,
            Err(e) => return ToolOutcome::err(format!("{}: {e}", path.display())),
        };
        let text = String::from_utf8_lossy(&data).to_string();
        let count = text.matches(old).count();
        if count == 0 {
            return ToolOutcome::err("old_string not found in file");
        }
        if count > 1 {
            return ToolOutcome::err(format!(
                "old_string matches {count} times — include more surrounding lines to make it unique"
            ));
        }
        let allowed = match self.ctx.perms.write {
            PermMode::Always => true,
            PermMode::Deny => false,
            PermMode::Ask => {
                ask_user(&self.ctx, "Edit file", &util::truncate_chars(path.to_string_lossy().as_ref(), 120)).await
            }
        };
        if !allowed {
            return ToolOutcome::err("edit denied by user (permissions)");
        }
        let updated = text.replacen(old, new, 1);
        match tokio::fs::write(&path, updated).await {
            Ok(_) => ToolOutcome::ok(format!("edited {}", path.display())),
            Err(e) => ToolOutcome::err(format!("{}: {e}", path.display())),
        }
    }

    async fn t_glob(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let pattern = input["pattern"].as_str().unwrap_or_default();
        let base = match input["path"].as_str() {
            Some(p) => self.resolve(p),
            None => self.cwd(),
        };
        let re = match glob_to_regex(pattern) {
            Ok(r) => r,
            Err(e) => return ToolOutcome::err(e),
        };
        let mut out = Vec::new();
        walk_files(&base, &mut |p| {
            if out.len() >= 500 {
                return;
            }
            if let Ok(rel) = p.strip_prefix(&base) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if re.is_match(&rel) {
                    out.push(p.to_string_lossy().to_string());
                }
            }
        });
        if out.is_empty() {
            ToolOutcome::ok(format!("no files match '{pattern}' under {}", base.display()))
        } else {
            ToolOutcome::ok(out.join("\n"))
        }
    }

    async fn t_grep(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let Some(pattern) = input["pattern"].as_str() else {
            return ToolOutcome::err("missing 'pattern'");
        };
        let base = match input["path"].as_str() {
            Some(p) => self.resolve(p),
            None => self.cwd(),
        };
        let file_glob = input["file_glob"].as_str().and_then(|g| glob_to_regex(g).ok());
        let re = match regex::RegexBuilder::new(pattern)
            .size_limit(1_000_000)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutcome::err(format!("bad regex: {e}")),
        };
        let mut out = Vec::new();
        walk_files(&base, &mut |p| {
            if out.len() >= 100 {
                return;
            }
            if let Some(fg) = &file_glob {
                let rel = p.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
                if !fg.is_match(rel.as_ref()) {
                    return;
                }
            }
            // skip obvious binaries
            let Ok(data) = std::fs::read(p) else { return };
            if data.len() > 1_500_000 || data.contains(&0u8) {
                return;
            }
            let text = String::from_utf8_lossy(&data);
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    out.push(format!(
                        "{}:{}:{}",
                        p.display(),
                        i + 1,
                        util::one_line(line, 200)
                    ));
                    if out.len() >= 100 {
                        return;
                    }
                }
            }
        });
        if out.is_empty() {
            ToolOutcome::ok(format!("no matches for '{pattern}'"))
        } else {
            if out.len() >= 100 {
                out.push("(100 match limit reached)".into());
            }
            ToolOutcome::ok(out.join("\n"))
        }
    }

    async fn t_process_list(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let limit = input["limit"].as_u64().unwrap_or(15).min(50) as usize;
        let by_mem = input["sort_by"].as_str() == Some("memory");
        let snap = crate::sysinfo::snapshot_processes(limit, by_mem);
        ToolOutcome::ok(snap)
    }

    async fn t_process_dump(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let by_mem = input["sort_by"].as_str() != Some("cpu");
        let json = crate::sysinfo::dump_json(by_mem);
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let path = self.ctx.dumps_dir.join(format!("proc-{ts}.json"));
        match tokio::fs::write(&path, json).await {
            Ok(_) => ToolOutcome::ok(format!("dumped process info to {}", path.display())),
            Err(e) => ToolOutcome::err(format!("dump failed: {e}")),
        }
    }

    async fn t_storage_scan(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let path = match input["path"].as_str() {
            Some(p) => self.resolve(p),
            None => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
        };
        if !path.exists() {
            return crate::agent::types::ToolOutcome::err(format!("{} does not exist", path.display()));
        }
        ToolOutcome::ok(crate::sysinfo::storage_scan(&path))
    }

    async fn t_notify(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let title = input["title"].as_str().unwrap_or("Beyond CLI");
        let message = input["message"].as_str().unwrap_or("");
        match util::send_toast(title, message) {
            Ok(_) => ToolOutcome::ok("notification sent"),
            Err(e) => ToolOutcome::err(format!("notification failed: {e:#}")),
        }
    }

    async fn t_schedule(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let (Some(when), Some(task)) = (input["when"].as_str(), input["task"].as_str()) else {
            return ToolOutcome::err("missing 'when' or 'task'");
        };
        match crate::notify::schedule(when, task) {
            Ok(dt) => ToolOutcome::ok(format!(
                "reminder set for {} — task: {task}",
                dt.format("%a %Y-%m-%d %H:%M %Z")
            )),
            Err(e) => ToolOutcome::err(format!("could not parse when '{when}': {e:#}")),
        }
    }

    async fn t_write_skill(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let (Some(name), Some(desc), Some(md)) = (
            input["name"].as_str(),
            input["description"].as_str(),
            input["markdown"].as_str(),
        ) else {
            return ToolOutcome::err("missing 'name', 'description' or 'markdown'");
        };
        let slug: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
            .collect();
        // a name made entirely of symbols would produce an empty filename
        let slug = if slug.is_empty() { "skill".to_string() } else { slug };
        let path = self.ctx.skills_dir.join(format!("{slug}.md"));
        let content = format!("---\nname: {slug}\ndescription: {}\n---\n\n{md}\n", util::one_line(desc, 120));
        match tokio::fs::write(&path, content).await {
            Ok(_) => ToolOutcome::ok(format!(
                "skill saved to {} — it will be loaded in every future session",
                path.display()
            )),
            Err(e) => ToolOutcome::err(format!("skill write failed: {e}")),
        }
    }

    async fn t_env_detect(&self) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        ToolOutcome::ok(crate::sysinfo::detect_toolchains())
    }

    async fn t_env_install(&self, input: Value) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let Some(name) = input["name"].as_str() else {
            return ToolOutcome::err("missing 'name'");
        };
        let allowed = match self.ctx.perms.bash {
            PermMode::Always => true,
            PermMode::Deny => false,
            PermMode::Ask => {
                ask_user(&self.ctx, "Install toolchain", &format!("Install '{name}' on this machine")).await
            }
        };
        if !allowed {
            return ToolOutcome::err("install denied by user");
        }
        let (cmd, via) = install_command(name);
        if cmd.is_empty() {
            return ToolOutcome::err(format!("unknown toolchain '{name}'"));
        }
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(600),
            tokio::process::Command::new(if cfg!(windows) { "powershell" } else { "sh" })
                .args(if cfg!(windows) {
                    vec!["-NoProfile", "-Command", &cmd]
                } else {
                    vec!["-c", &cmd]
                })
                // large SDK downloads can hang — kill with the timeout
                .kill_on_drop(true)
                .output(),
        )
        .await;
        match out {
            Err(_) => ToolOutcome::err("install timed out after 10 minutes"),
            Ok(Ok(o)) => {
                let ok = o.status.success();
                let tail = String::from_utf8_lossy(&o.stderr);
                ToolOutcome {
                    text: if ok {
                        format!("'{name}' installed via {via}. Verify with env_detect.")
                    } else {
                        format!("install failed: {}", util::one_line(&tail, 300))
                    },
                    is_error: !ok,
                }
            }
            Ok(Err(e)) => ToolOutcome::err(format!("spawn failed: {e}")),
        }
    }
}

/// winget/npm command per supported toolchain (also used by the /env wizard).
pub fn install_command(name: &str) -> (String, &'static str) {
    match name.to_ascii_lowercase().as_str() {
        "node" => ("winget install --id OpenJS.NodeJS.LTS -e --accept-source-agreements --accept-package-agreements".into(), "winget"),
        "python" => ("winget install --id Python.Python.3.12 -e --accept-source-agreements --accept-package-agreements".into(), "winget"),
        "rust" => ("winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements".into(), "winget"),
        "go" => ("winget install --id GoLang.Go -e --accept-source-agreements --accept-package-agreements".into(), "winget"),
        "dotnet" => ("winget install --id Microsoft.DotNet.SDK.8 -e --accept-source-agreements --accept-package-agreements".into(), "winget"),
        "java" => ("winget install --id EclipseAdoptium.Temurin.21.JDK -e --accept-source-agreements --accept-package-agreements".into(), "winget"),
        "git" => ("winget install --id Git.Git -e --accept-source-agreements --accept-package-agreements".into(), "winget"),
        "pnpm" => ("npm install -g pnpm".into(), "npm"),
        "bun" => ("powershell -c \"irm bun.sh/install.ps1 | iex\"".into(), "bun installer"),
        _ => (String::new(), ""),
    }
}

// ----------------------------------------------------------------- helpers

/// Convert a glob pattern into a `Regex` over `/`-separated relative paths.
pub fn glob_to_regex(pattern: &str) -> Result<regex::Regex, String> {
    let mut re = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // `**/` should also match zero directories
                    if i + 2 < chars.len() && chars[i + 2] == '/' {
                        re.push_str("(?:.*/)?");
                        i += 3;
                        continue;
                    }
                    re.push_str(".*");
                    i += 2;
                } else {
                    re.push_str("[^/\\\\]*");
                    i += 1;
                }
            }
            '?' => {
                re.push_str("[^/\\\\]");
                i += 1;
            }
            c => {
                if "\\.^$+()[]{}|".contains(c) {
                    re.push('\\');
                }
                re.push(c);
                i += 1;
            }
        }
    }
    re.push('$');
    regex::Regex::new(&re).map_err(|e| format!("bad glob '{pattern}': {e}"))
}

/// Recursive file walk that skips heavy/noise dirs. Blocking, but capped:
/// a hard file budget (20k) and a depth limit keep a hostile tree from
/// freezing the agent, and symlinks are never followed so cycles are
/// impossible.
fn walk_files(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path)) {
    let mut budget: usize = 20_000;
    walk_inner(dir, f, &mut budget, 0);
}

fn walk_inner(
    dir: &std::path::Path,
    f: &mut impl FnMut(&std::path::Path),
    budget: &mut usize,
    depth: u32,
) {
    const SKIP: [&str; 6] = [".git", "node_modules", "target", ".venv", "__pycache__", ".next"];
    const MAX_DEPTH: u32 = 32;
    if *budget == 0 || depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        // never follow links (junctions/symlinks) — avoids cycles
        if ft.is_symlink() {
            continue;
        }
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if ft.is_dir() {
            if SKIP.contains(&name.as_str()) {
                continue;
            }
            walk_inner(&p, f, budget, depth + 1);
            if *budget == 0 {
                return;
            }
        } else if *budget > 0 {
            *budget -= 1;
            f(&p);
        }
    }
}
