//! The agent loop: streams a model turn, executes tool calls, feeds results
//! back until the model stops calling tools or the user interrupts.
//!
//! Also builds the system prompt from config, skills, MCP servers and the
//! current project context — this is where Beyond CLI's identity lives.

pub mod claude_code;
pub mod client;
pub mod tools;
pub mod types;

use crate::config::Config;
use crate::mcp::McpManager;
use crate::skills::{self, Skill};
use client::{AnthropicClient, Auth, StreamEvent};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

pub use types::{Message, Usage};

/// Events the UI consumes while a turn runs.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    /// The model started writing a tool call (arguments still streaming).
    ToolPending { name: String },
    /// A tool call is about to execute; `preview` is a one-line summary.
    ToolStart { name: String, preview: String },
    ToolDone { name: String, ok: bool },
    Failed(String),
    /// Final event of a turn: full (updated) conversation + usage.
    TurnComplete {
        messages: Vec<Message>,
        usage: Usage,
        /// What the turn cost at API prices (0 when unknown).
        cost_usd: f64,
        /// Claude Code session to continue next turn (Claude Code backend only).
        claude_session: Option<String>,
    },
}

pub struct Agent {
    pub client: AnthropicClient,
    pub registry: tools::ToolRegistry,
}

impl Agent {
    pub fn new(
        cfg: &Config,
        cwd: Option<std::path::PathBuf>,
        mcps: Arc<McpManager>,
        confirm_tx: mpsc::UnboundedSender<tools::ConfirmRequest>,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        let auth = if let Some(o) = &cfg.oauth {
            // Claude account login wins over the (metered) API key.
            Auth::OAuth {
                access_token: o.access_token.clone(),
                refresh_token: o.refresh_token.clone(),
                expires_at: o.expires_at,
            }
        } else {
            Auth::ApiKey(cfg.api_key.clone().unwrap_or_default())
        };
        let client = AnthropicClient::new(auth, cfg.model.clone(), cfg.max_tokens);
        let ctx = tools::ToolContext {
            cwd,
            perms: cfg.permissions.clone(),
            skills_dir: crate::config::Paths::skills_dir(),
            dumps_dir: crate::config::Paths::dumps_dir(),
            mcps,
            confirm_tx,
            interrupt: interrupt.clone(),
        };
        Self { client, registry: tools::ToolRegistry::new(ctx) }
    }

    /// Run a full turn (possibly many model calls while tools are used).
    ///
    /// Returns the updated conversation (including tool exchanges) so the
    /// caller can persist it.
    pub async fn run_turn(
        &self,
        system: &str,
        mut messages: Vec<Message>,
        events: mpsc::UnboundedSender<AgentEvent>,
        interrupt: Arc<AtomicBool>,
    ) -> (Vec<Message>, Usage) {
        let mut total_usage = Usage::default();
        let mut interrupted = false;

        // Hard cap on tool cycles so a buggy loop can't run forever.
        for _cycle in 0..40 {
            if interrupt.load(Ordering::Relaxed) {
                break;
            }

            // ---- stream one model call -------------------------------
            let mut rx = match self.client.stream(system, &messages, self.registry.defs()).await {
                Ok(rx) => rx,
                Err(e) => {
                    let _ = events.send(AgentEvent::Failed(format!("{e:#}")));
                    return (messages, total_usage);
                }
            };

            // Reassemble this call's assistant message from stream events.
            let mut text = String::new();
            // tool blocks keyed by stream index
            let mut tool_order: Vec<u64> = Vec::new();
            let mut tool_blocks: std::collections::HashMap<u64, (String, String, String)> =
                std::collections::HashMap::new(); // index -> (id, name, json)

            while let Some(ev) = rx.recv().await {
                if interrupt.load(Ordering::Relaxed) {
                    interrupted = true;
                    break;
                }
                match ev {
                    StreamEvent::TextDelta(t) => {
                        text.push_str(&t);
                        let _ = events.send(AgentEvent::TextDelta(t));
                    }
                    StreamEvent::ToolUseStart { index, id, name } => {
                        tool_order.push(index);
                        tool_blocks.insert(index, (id, name.clone(), String::new()));
                        let _ = events.send(AgentEvent::ToolPending { name: name.clone() });
                    }
                    StreamEvent::ToolUseDelta { index, json } => {
                        if let Some(entry) = tool_blocks.get_mut(&index) {
                            entry.2.push_str(&json);
                        }
                    }
                    StreamEvent::MessageDone { usage } => {
                        total_usage.input_tokens += usage.input_tokens;
                        total_usage.output_tokens += usage.output_tokens;
                    }
                    StreamEvent::Failed(e) => {
                        let _ = events.send(AgentEvent::Failed(e));
                        return (messages, total_usage);
                    }
                }
            }

            if interrupted {
                // keep whatever partial text arrived
                if !text.is_empty() {
                    messages.push(Message::assistant_text(text));
                }
                break;
            }

            // ---- finalize the assistant message ----------------------
            let mut content = Vec::new();
            if !text.is_empty() {
                content.push(types::ContentBlock::Text { text });
            }
            for idx in &tool_order {
                if let Some((id, name, json_raw)) = tool_blocks.get(idx) {
                    let input = if json_raw.trim().is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(json_raw).unwrap_or_else(|_| serde_json::json!({}))
                    };
                    content.push(types::ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input,
                    });
                }
            }
            if content.is_empty() {
                break; // nothing at all — treat as end of turn
            }
            let assistant = Message { role: types::Role::Assistant, content };
            let has_tools = assistant.has_tool_use();
            messages.push(assistant);

            if !has_tools {
                break; // plain answer — turn complete
            }

            // ---- execute the tool calls ------------------------------
            let assistant = messages.last().unwrap().clone();
            let mut executed: Vec<String> = Vec::new();
            for block in &assistant.content {
                if let types::ContentBlock::ToolUse { id, name, input } = block {
                    if interrupt.load(Ordering::Relaxed) {
                        interrupted = true;
                        break;
                    }
                    let preview = tool_preview(name, input);
                    let _ = events.send(AgentEvent::ToolStart { name: name.clone(), preview });
                    let outcome = self.registry.exec(name, input.clone()).await;
                    let _ = events.send(AgentEvent::ToolDone { name: name.clone(), ok: !outcome.is_error });
                    messages.push(Message::tool_result(id.clone(), outcome.text, outcome.is_error));
                    executed.push(id.clone());
                }
            }
            if interrupted {
                // Every tool_use the assistant emitted needs a tool_result
                // before the next request, or the API rejects the turn
                // ("tool_use ids were provided without tool_result blocks").
                for block in &assistant.content {
                    if let types::ContentBlock::ToolUse { id, .. } = block
                        && !executed.contains(id) {
                            messages.push(Message::tool_result(
                                id.clone(),
                                "(interrupted by user)".into(),
                                true,
                            ));
                        }
                }
                break;
            }
        }

        (messages, total_usage)
    }
}

/// Short one-line preview of a tool call for the activity feed.
fn tool_preview(name: &str, input: &serde_json::Value) -> String {
    match name {
        "bash" => input["command"].as_str().map(|c| crate::util::one_line(c, 90)).unwrap_or_default(),
        "read_file" | "write_file" | "edit_file" => {
            input["path"].as_str().map(|p| p.to_string()).unwrap_or_default()
        }
        "glob_files" | "grep_files" => {
            format!(
                "{} in {}",
                input["pattern"].as_str().unwrap_or("?"),
                input["path"].as_str().unwrap_or("project")
            )
        }
        "process_list" => format!("top {}", input["limit"].as_u64().unwrap_or(15)),
        "storage_scan" => input["path"].as_str().unwrap_or("home").to_string(),
        "schedule_reminder" => format!(
            "{} → {}",
            input["when"].as_str().unwrap_or("?"),
            crate::util::one_line(input["task"].as_str().unwrap_or(""), 60)
        ),
        "write_skill" => input["name"].as_str().unwrap_or("?").to_string(),
        "env_install" => input["name"].as_str().unwrap_or("?").to_string(),
        _ => String::new(),
    }
}

// -------------------------------------------------------------- system prompt

/// What Beyond adds to Claude Code's own system prompt on the Claude Code
/// backend: who is hosting it, the user's skills and their standing
/// instructions. Tools and environment details are Claude Code's business.
pub fn build_claude_context(cfg: &Config, skills: &[Skill], project: Option<&std::path::Path>) -> String {
    let mut s = String::from(
        "You are running inside Beyond CLI, a terminal front end. Your output is shown in a\n\
         narrow chat view: keep answers concise and use markdown lightly.\n",
    );
    if let Some(p) = project {
        s.push_str(&format!("\nThe user tagged this project folder: {}\n", p.display()));
    }
    s.push_str(&skills::system_prompt_section(skills, 3500));
    let extras = crate::config::system_extras_lines(cfg);
    if !extras.is_empty() {
        s.push_str(&format!("\n## User instructions\n{extras}\n"));
    }
    s
}

/// Compose the full system prompt from config + skills + MCP context.
pub fn build_system_prompt(cfg: &Config, skills: &[Skill], mcps: &McpManager, project: Option<&std::path::Path>) -> String {
    let mut s = String::from(
        "You are Beyond — the agent inside Beyond CLI, a fast, lightweight terminal coding assistant.\n\
         You help with software engineering: writing and reviewing code, running commands, managing files,\n\
         and keeping the user's machine healthy. Be concise, direct and practical. Use markdown lightly\n\
         (short paragraphs, code fences with language tags). Never invent tool results.\n",
    );

    // Environment / project context
    if let Some(p) = project {
        s.push_str(&format!("\n## Current project\nWorking directory: {}\nAll relative paths in file tools resolve against this directory.\n", p.display()));
    } else {
        s.push_str("\n## Current project\nNo project directory is tagged yet. Suggest `/project` if the user references a codebase.\n");
    }

    // Machine awareness — the user explicitly wants load-aware behavior
    let (used, total) = crate::sysinfo::memory_totals();
    s.push_str(&format!(
        "\n## Machine awareness\nYou can inspect running processes (`process_list`, `process_dump`) and disk usage\n\
         (`storage_scan`). Before launching heavy builds or test suites, check load and pick efficient\n\
         commands. RAM in use right now: {} / {}.\n",
        crate::util::human_size(used),
        crate::util::human_size(total)
    ));

    // Environment setup — one job: detect, then install exactly what's missing
    s.push_str(
        "\n## Environment setup\nUse `env_detect` to see which toolchains are installed and `env_install` to add\n\
         missing SDKs (node, python, rust, go, dotnet, java, git). Never install anything the user\n\
         did not ask for; if a package manager fails, report it and move on.\n",
    );

    // Skills
    let sk = skills::system_prompt_section(skills, 3500);
    s.push_str(&sk);

    // MCP servers
    if !mcps.servers.is_empty() {
        s.push_str("\n## MCP servers\nConnected: ");
        let names: Vec<String> = mcps.servers.iter().map(|x| format!("{} ({} tools)", x.name, x.tools.len())).collect();
        s.push_str(&names.join(", "));
        s.push_str("\nTheir tools are exposed with `mcp:<server>/<tool>` names.\n");
    }

    // Self-learning directive
    s.push_str(
        "\n## Learning\nWhen you discover a reusable workflow, convention or fix, save it as a skill with\n\
         `write_skill` so future sessions benefit automatically.\n",
    );

    // User extras
    let extras = crate::config::system_extras_lines(cfg);
    if !extras.is_empty() {
        s.push_str(&format!("\n## User instructions\n{extras}\n"));
    }

    s
}
