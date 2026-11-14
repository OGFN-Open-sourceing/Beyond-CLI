//! Minimal but real MCP (Model Context Protocol) client.
//!
//! Discovers MCP servers dropped into `ExtraResources/MCPs/` (`.exe`, `.py`,
//! `.js`, `.cmd`/`.bat`), speaks JSON-RPC over stdio (initialize →
//! tools/list → tools/call) and exposes their tools to the model as
//! `mcp:<server>/<tool>` so they merge seamlessly with built-in tools.

use crate::agent::types::ToolDef;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex};

/// One connected MCP server (stdio transport).
pub struct McpServer {
    pub name: String,
    /// Shared so `McpManager::shutdown(&self)` can kill it while tools run.
    child: Arc<Mutex<Child>>,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    pub tools: Vec<McpTool>,
}

/// A tool advertised by a server.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// All running MCP servers + any startup warnings.
pub struct McpManager {
    pub servers: Vec<McpServer>,
    pub warnings: Vec<String>,
}

impl McpManager {
    /// Discover and start every MCP server under `dir` (non-existent dir → empty).
    pub async fn load_all(dir: &Path) -> McpManager {
        let mut servers = Vec::new();
        let mut warnings = Vec::new();

        let Ok(entries) = std::fs::read_dir(dir) else {
            return McpManager { servers, warnings };
        };

        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        files.sort();

        for path in files {
            if !is_server_file(&path) {
                continue;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase().replace([' ', '.'], "-"))
                .unwrap_or_else(|| "mcp".into());
            match McpServer::spawn(&name, &path).await {
                Ok(mut srv) => match srv.bootstrap().await {
                    Ok(()) => servers.push(srv),
                    Err(e) => warnings.push(format!("{name}: {e:#}")),
                },
                Err(e) => warnings.push(format!("{name}: {e:#}")),
            }
        }

        McpManager { servers, warnings }
    }

    /// Flatten every server's tools into API `ToolDef`s.
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        let mut out = Vec::new();
        for s in &self.servers {
            for t in &s.tools {
                out.push(ToolDef {
                    name: format!("mcp:{}/{}", s.name, t.name),
                    description: format!("[MCP:{}] {}", s.name, t.description),
                    input_schema: t.schema.clone(),
                });
            }
        }
        out
    }

    /// Call a tool on a specific server.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        args: Value,
    ) -> crate::agent::types::ToolOutcome {
        use crate::agent::types::ToolOutcome;
        let Some(s) = self.servers.iter().find(|s| s.name == server) else {
            return ToolOutcome::err(format!("MCP server '{server}' is not running"));
        };
        match s.call("tools/call", json!({ "name": tool, "arguments": args })).await {
            Ok(res) => {
                let is_err = res["isError"].as_bool().unwrap_or(false);
                let text = res["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_else(|| res.to_string());
                if is_err {
                    ToolOutcome::err(text)
                } else if text.is_empty() {
                    ToolOutcome::ok("(empty result)")
                } else {
                    ToolOutcome::ok(text)
                }
            }
            Err(e) => ToolOutcome::err(format!("MCP call failed: {e:#}")),
        }
    }

    /// Kill every child process (best-effort, called on app exit).
    pub async fn shutdown(&self) {
        for s in &self.servers {
            let mut c = s.child.lock().await;
            let _ = c.kill().await;
        }
    }
}

fn is_server_file(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("exe") | Some("py") | Some("js") | Some("mjs") | Some("cmd") | Some("bat")
    )
}

impl McpServer {
    /// Spawn the server process with piped stdio (does not speak protocol yet).
    async fn spawn(name: &str, path: &Path) -> Result<Self> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let display = path.display().to_string();

        let (program, args): (String, Vec<String>) = match ext.as_str() {
            "exe" => (display.clone(), vec![]),
            "cmd" | "bat" => (display.clone(), vec![]),
            "py" => ("python".into(), vec![display.clone()]),
            "js" | "mjs" => ("node".into(), vec![display.clone()]),
            other => return Err(anyhow!("unsupported MCP file type .{other}")),
        };

        let mut child = tokio::process::Command::new(&program)
            .args(&args)
            .env("RUST_LOG", "error")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("launching MCP '{program}'"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;

        // Reader task: forwards JSON-RPC responses to pending requests.
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(ln)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&ln) else { continue };
                let Some(id) = v["id"].as_u64() else { continue }; // notifications ignored
                if let Some(tx) = pending_reader.lock().await.remove(&id) {
                    let _ = tx.send(v);
                }
            }
        });

        Ok(Self {
            name: name.into(),
            child: Arc::new(Mutex::new(child)),
            stdin: Mutex::new(stdin),
            next_id: AtomicU64::new(1),
            pending,
            tools: vec![],
        })
    }

    /// MCP handshake + tool discovery.
    async fn bootstrap(&mut self) -> Result<()> {
        let init = self
            .call(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "beyond-cli", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await
            .context("initialize handshake")?;
        let _ = init; // capabilities are not used further

        // Notifications carry no id — fire and forget.
        self.notify("notifications/initialized", json!({})).await?;

        let list = self.call("tools/list", json!({})).await.context("tools/list")?;
        self.tools = list["tools"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| {
                        Some(McpTool {
                            name: t["name"].as_str()?.into(),
                            description: t["description"].as_str().unwrap_or_default().into(),
                            schema: t["inputSchema"].clone(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(())
    }

    /// Send a request and await the matching response (60s timeout).
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let line = format!("{msg}\n");
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;

        let resp = tokio::time::timeout(std::time::Duration::from_secs(60), rx)
            .await
            .map_err(|_| anyhow!("{method} timed out"))?
            .map_err(|_| anyhow!("{method} cancelled"))?;

        if let Some(err) = resp["error"].as_object() {
            return Err(anyhow!("{method}: {}", err["message"].as_str().unwrap_or("error")));
        }
        Ok(resp["result"].clone())
    }

    /// Fire-and-forget JSON-RPC notification.
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(format!("{msg}\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}
