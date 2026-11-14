//! Session persistence + Claude Code import.
//!
//! Beyond sessions live in `<config>/sessions/<id>.json`. Claude Code CLI /
//! desktop sessions (`~/.claude/projects/**/*.jsonl`) are scanned and imported
//! read-only so `/loadsession <id>` can resume them seamlessly.

use crate::agent::types::{ContentBlock, Message};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created: String,
    pub cwd: Option<String>,
    pub model: String,
    pub messages: Vec<Message>,
    /// Claude Code session this conversation continues in, when it runs on
    /// the Claude Code backend (so resuming picks up Claude's full context).
    #[serde(default)]
    pub claude_session: Option<String>,
}

impl Session {
    pub fn new(model: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string()[..8].to_string(),
            title: "new session".into(),
            created: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            cwd: std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string()),
            model: model.into(),
            messages: vec![],
            claude_session: None,
        }
    }

    /// Derive a display title from the first user message.
    pub fn refresh_title(&mut self) {
        if self.messages.iter().any(|m| m.role == crate::agent::types::Role::User)
            && let Some(m) = self.messages.iter().find(|m| m.role == crate::agent::types::Role::User) {
                self.title = m.title();
            }
    }

    fn file(&self) -> PathBuf {
        crate::config::Paths::sessions_dir().join(format!("{}.json", self.id))
    }

    pub fn save(&self) -> Result<()> {
        let p = self.file();
        std::fs::write(p, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// A session id + title + created, for the picker.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub created: String,
}

/// List saved Beyond sessions, newest first.
pub fn list_beyond() -> Vec<SessionMeta> {
    let dir = crate::config::Paths::sessions_dir();
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "json").unwrap_or(false)
            && let Ok(raw) = std::fs::read_to_string(&p)
                && let Ok(s) = serde_json::from_str::<Session>(&raw) {
                    out.push(SessionMeta { id: s.id, title: s.title, created: s.created });
                }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

/// Load a Beyond session by (possibly partial) id.
pub fn load_beyond(id_prefix: &str) -> Result<Session> {
    let dir = crate::config::Paths::sessions_dir();
    let full = dir.join(format!("{id_prefix}.json"));
    if full.exists() {
        let raw = std::fs::read_to_string(&full)?;
        return serde_json::from_str(&raw).context("parsing session file");
    }
    // partial-id search
    for meta in list_beyond() {
        if meta.id.starts_with(id_prefix) {
            let raw = std::fs::read_to_string(dir.join(format!("{}.json", meta.id)))?;
            return serde_json::from_str(&raw).context("parsing session file");
        }
    }
    anyhow::bail!("no session matches id '{id_prefix}' (use /loadsession with no args to list)")
}

// --------------------------------------------------------- claude import

/// A Claude Code session discovered on disk.
#[derive(Debug, Clone)]
pub struct ClaudeSession {
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub modified: String,
}

/// Scan `~/.claude/projects/**/*.jsonl` for importable sessions.
pub fn scan_claude_sessions() -> Vec<ClaudeSession> {
    let mut out = Vec::new();
    let Some(home) = dirs::home_dir() else { return out };
    let projects = home.join(".claude").join("projects");
    let Ok(rd) = std::fs::read_dir(&projects) else { return out };
    for d in rd.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
                let id = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let modified = f
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| {
                        chrono::DateTime::<chrono::Local>::from(t)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                let cwd = first_cwd_in(&p).unwrap_or_else(|| d.file_name().to_string_lossy().to_string());
                out.push(ClaudeSession { id, path: p, cwd, modified });
            }
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// Peek at the `cwd` field of the first entries in a .jsonl session file.
/// Reads only the first ~20 lines — session files can be hundreds of MB.
fn first_cwd_in(p: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(p).ok()?;
    let mut reader = std::io::BufReader::new(file);
    for _ in 0..20 {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF or I/O error
            Ok(_) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
                    && let Some(cwd) = v["cwd"].as_str() {
                        return Some(cwd.to_string());
                    }
            }
        }
    }
    None
}

/// Import a Claude Code .jsonl session into plain text messages.
/// Tool call/result noise is skipped — only human + assistant text is kept.
pub fn load_claude_session(s: &ClaudeSession) -> Result<Session> {
    let raw = std::fs::read_to_string(&s.path).context("reading .jsonl")?;
    let mut messages = Vec::new();

    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        // Claude Code jsonl rows: { type: "user"|"assistant", message: {role, content} }
        let typ = v["type"].as_str().unwrap_or("");
        if typ != "user" && typ != "assistant" {
            continue;
        }
        let msg = &v["message"];
        let content = &msg["content"];
        let text = extract_text(content);
        if text.trim().is_empty() {
            continue;
        }
        // skip synthetic rows (tool results come as user rows with tool_result blocks)
        if typ == "user" && content.as_array().map(|a| a.iter().any(|b| b["type"] == "tool_result")).unwrap_or(false) {
            continue;
        }
        let role = if typ == "user" {
            crate::agent::types::Role::User
        } else {
            crate::agent::types::Role::Assistant
        };
        messages.push(Message { role, content: vec![ContentBlock::Text { text }] });
    }

    if messages.is_empty() {
        anyhow::bail!("no readable messages in {}", s.path.display());
    }

    let mut sess = Session {
        id: format!("cc-{}", &s.id[..s.id.len().min(8)]),
        title: messages.first().map(|m| m.title()).unwrap_or_else(|| "claude session".into()),
        created: s.modified.clone(),
        cwd: Some(s.cwd.clone()),
        model: "imported".into(),
        messages,
        // the real thing: on the Claude Code backend this continues the
        // original session with all of its context, not just the visible text
        claude_session: Some(s.id.clone()),
    };
    sess.refresh_title();
    Ok(sess)
}

/// Pull human-readable text out of a Claude Code content field
/// (string, or array of {type:"text"|"tool_use"|"tool_result", ...}).
fn extract_text(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut out = Vec::new();
        for b in arr {
            if b["type"] == "text"
                && let Some(t) = b["text"].as_str() {
                    out.push(t.to_string());
                }
        }
        return out.join("\n");
    }
    String::new()
}
