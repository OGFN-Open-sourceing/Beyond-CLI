//! Streaming client for the Anthropic Messages API, authenticated with the
//! user's own API key. (Subscription use goes through the official Claude
//! Code CLI instead — see [`super::claude_code`].)
//!
//! Emits [`StreamEvent`]s over a channel while the response streams in, so the
//! TUI can render text and tool calls live. Tool_use blocks are reassembled
//! from `input_json_delta` chunks before being exposed to the agent loop.

use super::types::{Message, ToolDef, Usage};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2023-06-01";
/// Give up on connecting after this long (streams themselves have no cap).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Kill the stream when no chunk arrives for this long (stalled network).
const CHUNK_TIMEOUT: Duration = Duration::from_secs(120);

/// One HTTP client for the whole process, so TLS sessions and connections
/// are reused across turns instead of renegotiated for every model call.
fn http() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let built = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .context("building HTTP client")?;
    Ok(CLIENT.get_or_init(|| built))
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

pub struct AnthropicClient {
    api_key: String,
    pub model: String,
    pub max_tokens: u32,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String, max_tokens: u32) -> Self {
        Self { api_key, model, max_tokens }
    }

    /// Start a streaming turn. Returns a receiver of events; the HTTP/SSE work
    /// runs on a background task so the caller never blocks the UI thread.
    pub async fn stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
        if self.api_key.trim().is_empty() {
            anyhow::bail!("no API key — run `/login key`");
        }
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": messages,
            "tools": tools,
            "stream": true,
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let key = self.api_key.clone();
        tokio::spawn(async move {
            if let Err(e) = stream_once(&key, body, tx.clone()).await {
                let _ = tx.send(StreamEvent::Failed(format!("{e:#}")));
            }
        });
        Ok(rx)
    }
}

/// Perform one streamed request, forwarding parsed SSE events into `tx`.
async fn stream_once(api_key: &str, body: serde_json::Value, tx: mpsc::UnboundedSender<StreamEvent>) -> Result<()> {
    let resp = http()?
        .post(API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", API_VERSION)
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

    // Stream raw bytes and split on SSE event boundaries ("\n\n"). The buffer
    // stays bytes until a whole event is in: a network chunk can end in the
    // middle of a multi-byte character (é, ő, emoji), and decoding chunks one
    // by one would turn those into U+FFFD.
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
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
        buf.extend_from_slice(&chunk);

        // Process all complete events currently buffered.
        while let Some(event) = take_event(&mut buf) {
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

/// Cut the first complete SSE event (up to and including its blank line)
/// off the front of `buf`, decoded only now that all its bytes are here.
fn take_event(buf: &mut Vec<u8>) -> Option<String> {
    let pos = buf.windows(2).position(|w| w == b"\n\n")?;
    let raw: Vec<u8> = buf.drain(..pos + 2).collect();
    Some(String::from_utf8_lossy(&raw).into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_split_across_chunks_survive() {
        let event = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"szőlő 🍇\"}}\n\n";
        let bytes = event.as_bytes();
        // cut inside the 2-byte "ő" and again inside the 4-byte emoji
        let cut1 = event.find('ő').unwrap() + 1;
        let cut2 = event.find('🍇').unwrap() + 2;
        let mut buf = Vec::new();
        buf.extend_from_slice(&bytes[..cut1]);
        assert!(take_event(&mut buf).is_none());
        buf.extend_from_slice(&bytes[cut1..cut2]);
        assert!(take_event(&mut buf).is_none());
        buf.extend_from_slice(&bytes[cut2..]);
        let got = take_event(&mut buf).unwrap();
        assert!(buf.is_empty());
        match parse_sse_event(&got) {
            Some(SseEvent::Stream(StreamEvent::TextDelta(t))) => assert_eq!(t, "szőlő 🍇"),
            _ => panic!("expected a text delta"),
        }
    }
}
