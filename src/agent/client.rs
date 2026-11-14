//! Streaming client for the Anthropic Messages API.
//!
//! Supports two auth modes (see [`Auth`]): a classic API key, or a Claude
//! account OAuth token pair (Pro/Max subscription) which is transparently
//! refreshed before expiry and persisted back into `config.json`.
//!
//! Emits [`StreamEvent`]s over a channel while the response streams in, so the
//! TUI can render text and tool calls live. Tool_use blocks are reassembled
//! from `input_json_delta` chunks before being exposed to the agent loop.

use super::types::{Message, ToolDef, Usage};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2023-06-01";
/// Give up on connecting after this long (streams themselves have no cap).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Kill the stream when no chunk arrives for this long (stalled network).
const CHUNK_TIMEOUT: Duration = Duration::from_secs(120);
/// Refresh OAuth tokens this many seconds before they actually expire.
const REFRESH_SLACK: u64 = 120;

/// How the client authenticates against the Anthropic API.
#[derive(Debug, Clone)]
pub enum Auth {
    /// A metered console API key (`x-api-key` header).
    ApiKey(String),
    /// Claude account login (subscription usage). Uses `Authorization: Bearer`
    /// plus the `anthropic-beta: oauth-2025-04-20` opt-in header.
    OAuth {
        access_token: String,
        refresh_token: String,
        /// Unix seconds when `access_token` expires (0 = unknown).
        expires_at: u64,
    },
}

/// Events produced while streaming a model turn.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Plain assistant text chunk.
    TextDelta(String),
    /// A tool_use block started at `index` (id + tool name).
    ToolUseStart { index: u64, id: String, name: String },
    /// Partial JSON argument chunk for the tool block at `index`.
    ToolUseDelta { index: u64, json: String },
    /// Turn fully finished; carries accumulated token usage.
    MessageDone { usage: Usage },
    /// Request/stream failed.
    Failed(String),
}

#[derive(Debug)]
pub struct AnthropicClient {
    /// Wrapped in a mutex so an in-flight refresh is visible to later turns
    /// (each turn constructs a fresh `Agent`, but tokens outlive turns).
    pub auth: Mutex<Auth>,
    pub model: String,
    pub max_tokens: u32,
}

impl AnthropicClient {
    pub fn new(auth: Auth, model: String, max_tokens: u32) -> Self {
        Self { auth: Mutex::new(auth), model, max_tokens }
    }

    /// Ensure OAuth tokens are fresh; refresh + persist them if not.
    /// Returns `Err` only when the refresh itself fails.
    async fn ensure_fresh(&self) -> Result<()> {
        let mut guard = self.auth.lock().await;
        let Auth::OAuth { access_token, refresh_token, expires_at } = &*guard else {
            return Ok(()); // API-key mode: nothing to refresh
        };
        let creds = crate::config::OAuthCreds {
            access_token: access_token.clone(),
            refresh_token: refresh_token.clone(),
            expires_at: *expires_at,
        };
        if !creds.needs_refresh(REFRESH_SLACK) {
            return Ok(());
        }
        let fresh = crate::auth::refresh_tokens(&creds.refresh_token)
            .await
            .context("refreshing Claude login")?;
        let new_creds = crate::config::OAuthCreds {
            access_token: fresh.access_token.clone(),
            refresh_token: if fresh.refresh_token.is_empty() {
                creds.refresh_token.clone()
            } else {
                fresh.refresh_token.clone()
            },
            expires_at: chrono::Utc::now().timestamp() as u64 + fresh.expires_in,
        };
        // Persist so restarts (and other turns) see the new tokens.
        let _ = crate::config::update(|c| c.oauth = Some(new_creds.clone()));
        *guard = Auth::OAuth {
            access_token: new_creds.access_token,
            refresh_token: new_creds.refresh_token,
            expires_at: new_creds.expires_at,
        };
        Ok(())
    }

    /// Start a streaming turn. Returns a receiver of events; the HTTP/SSE work
    /// runs on a background task so the caller never blocks the UI thread.
    ///
    /// In OAuth mode the system prompt is prefixed with the Claude Code
    /// identity line the subscription endpoint requires.
    pub async fn stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
        self.ensure_fresh().await?;
        let (auth_snapshot, is_oauth) = {
            let guard = self.auth.lock().await;
            (guard.clone(), matches!(&*guard, Auth::OAuth { .. }))
        };
        let system = if is_oauth {
            format!("{}. {system}", crate::auth::OAUTH_SYSTEM_PREFIX)
        } else {
            system.to_string()
        };

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": messages,
            "tools": tools,
            "stream": true,
        });

        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            if let Err(e) = stream_once(auth_snapshot, body, tx.clone()).await {
                let _ = tx.send(StreamEvent::Failed(format!("{e:#}")));
            }
        });
        Ok(rx)
    }
}

impl Auth {
    /// Apply this auth mode to an outgoing request builder.
    fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Auth::ApiKey(key) => req
                .header("x-api-key", key)
                .header("anthropic-version", API_VERSION),
            Auth::OAuth { access_token, .. } => req
                .header("Authorization", format!("Bearer {access_token}"))
                .header("anthropic-version", API_VERSION)
                .header("anthropic-beta", crate::auth::OAUTH_BETA),
        }
    }
}

/// Perform one streamed request, forwarding parsed SSE events into `tx`.
async fn stream_once(
    auth: Auth,
    body: serde_json::Value,
    tx: mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .context("building HTTP client")?;
    let resp = auth
        .apply(client.post(API_URL))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("network request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        // try to surface the human-readable error field
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| crate::util::truncate_chars(&text, 300));
        anyhow::bail!("API {status}: {msg}");
    }

    // Stream raw bytes and split on SSE event boundaries ("\n\n").
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    // Usage accumulates across message_start / message_delta events.
    let mut usage = Usage::default();

    loop {
        // Per-chunk idle timeout: a silent connection for over two minutes
        // is treated as a stalled stream instead of hanging forever.
        let chunk = match tokio::time::timeout(CHUNK_TIMEOUT, stream.next()).await {
            Err(_) => anyhow::bail!(
                "stream stalled (no data for {}s)",
                CHUNK_TIMEOUT.as_secs()
            ),
            Ok(None) => break, // stream ended
            Ok(Some(c)) => c,
        };
        let chunk = chunk.context("stream interrupted")?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // Process all complete events currently buffered.
        while let Some(pos) = buf.find("\n\n") {
            let event: String = buf.drain(..pos + 2).collect();
            match parse_sse_event(&event) {
                Some(SseEvent::Stream(ev)) => {
                    if tx.send(ev).is_err() {
                        return Ok(()); // receiver dropped → user left or interrupted
                    }
                }
                Some(SseEvent::Usage { input, output }) => {
                    // SSE updates are partial: merge into the running usage.
                    if let Some(i) = input {
                        usage.input_tokens = i;
                    }
                    if let Some(o) = output {
                        usage.output_tokens = o;
                    }
                }
                Some(SseEvent::Done) => {
                    // Stream complete: emit the accumulated usage exactly once.
                    let _ = tx.send(StreamEvent::MessageDone { usage });
                    return Ok(());
                }
                None => {}
            }
        }
    }
    Ok(())
}

/// What one SSE block yields: a UI event, a partial usage update, or end.
enum SseEvent {
    Stream(StreamEvent),
    Usage { input: Option<u64>, output: Option<u64> },
    Done,
}

/// Parse one SSE event block into an [`SseEvent`], if applicable.
fn parse_sse_event(block: &str) -> Option<SseEvent> {
    // Find the "data:" payload line(s).
    let data = block
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| l.trim_start())
        .collect::<Vec<_>>()
        .join("");
    if data.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;

    let typ = v["type"].as_str()?;
    match typ {
        "message_start" => {
            Some(SseEvent::Usage {
                input: v["message"]["usage"]["input_tokens"].as_u64(),
                output: v["message"]["usage"]["output_tokens"].as_u64(),
            })
        }
        "content_block_start" => {
            let cb = &v["content_block"];
            if cb["type"] == "tool_use" {
                Some(SseEvent::Stream(StreamEvent::ToolUseStart {
                    index: v["index"].as_u64().unwrap_or(0),
                    id: cb["id"].as_str().unwrap_or_default().into(),
                    name: cb["name"].as_str().unwrap_or_default().into(),
                }))
            } else {
                None
            }
        }
        "content_block_delta" => {
            let d = &v["delta"];
            match d["type"].as_str() {
                Some("text_delta") => Some(SseEvent::Stream(StreamEvent::TextDelta(
                    d["text"].as_str().unwrap_or_default().into(),
                ))),
                Some("input_json_delta") => Some(SseEvent::Stream(StreamEvent::ToolUseDelta {
                    index: v["index"].as_u64().unwrap_or(0),
                    json: d["partial_json"].as_str().unwrap_or_default().into(),
                })),
                _ => None,
            }
        }
        "message_delta" => {
            // output_tokens here is cumulative for the turn.
            Some(SseEvent::Usage { input: None, output: v["usage"]["output_tokens"].as_u64() })
        }
        "message_stop" => Some(SseEvent::Done),
        "error" => {
            let msg = v["error"]["message"].as_str().unwrap_or("unknown error");
            Some(SseEvent::Stream(StreamEvent::Failed(msg.to_string())))
        }
        _ => None,
    }
}
