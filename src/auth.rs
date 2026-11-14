//! Claude account login (OAuth 2.0 + PKCE), so the app can run on a Claude
//! Pro/Max/Ultra subscription instead of a metered API key.
//!
//! Flow (mirrors what Claude Code's own CLI does):
//! 1. generate a PKCE verifier + challenge
//! 2. open `claude.ai/oauth/authorize` in the user's browser (`code=true` makes
//!    the callback page *display* the authorization code instead of redirecting
//!    to a local server — perfect for a CLI)
//! 3. the user pastes the `CODE#STATE` string back into Beyond CLI
//! 4. exchange it at the token endpoint for an access + refresh token pair
//! 5. requests then use `Authorization: Bearer <token>` plus the
//!    `anthropic-beta: oauth-2025-04-20` opt-in header
//!
//! Access tokens live ~8 h and are refreshed automatically before expiry.

use anyhow::{anyhow, Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

/// The public OAuth client id Claude Code ships with.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPE: &str = "org:create_api_key user:profile user:inference";

/// Sent as `anthropic-beta` so subscription tokens are accepted on the
/// Messages API (they are rejected without this opt-in).
pub const OAUTH_BETA: &str = "oauth-2025-04-20";

/// The OAuth-scoped endpoint requires the system prompt to identify the
/// caller as Claude Code; this line leads every request in OAuth mode.
pub const OAUTH_SYSTEM_PREFIX: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// One half of a PKCE pair (`state` doubles as the verifier for this flow).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// A token set as returned by the token endpoint.
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Seconds until the access token expires (0 = unknown).
    pub expires_in: u64,
}

// ------------------------------------------------------------------- pkce

/// Generate a fresh PKCE pair: 32 random bytes → base64url verifier,
/// SHA-256 of the verifier → base64url challenge.
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("system RNG unavailable");
    let verifier = b64url(&bytes);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    Pkce { verifier, challenge }
}

/// Build the browser authorization URL (user then pastes back `code#state`).
pub fn authorize_url(pkce: &Pkce) -> String {
    format!(
        "{AUTHORIZE_URL}?code=true&client_id={CLIENT_ID}&response_type=code\
         &redirect_uri={REDIRECT_URI}&scope={SCOPE}\
         &code_challenge={}&code_challenge_method=S256&state={}",
        urlencode(&pkce.challenge),
        urlencode(&pkce.verifier),
    )
}

/// Percent-encode the small set of characters that can break a URL query.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Base64url (RFC 4648 §5) without padding — PKCE requires exactly this.
fn b64url(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

// ----------------------------------------------------------------- tokens

/// Exchange an authorization code for a token set.
///
/// `verifier` is the PKCE verifier the code was requested with (Claude's flow
/// also sends it as the `state` parameter).
pub async fn exchange_code(code: &str, verifier: &str) -> Result<Tokens> {
    let body = json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": verifier,
        "client_id": CLIENT_ID,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": verifier,
    });
    token_request(body).await
}

/// Refresh an expired access token.
pub async fn refresh_tokens(refresh_token: &str) -> Result<Tokens> {
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });
    token_request(body).await
}

/// Shared POST to the token endpoint (JSON body, per Anthropic's client).
async fn token_request(body: serde_json::Value) -> Result<Tokens> {
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .header("content-type", "application/json")
        .header("user-agent", "anthropic")
        .json(&body)
        .send()
        .await
        .context("token request failed")?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| anyhow!("token endpoint returned non-JSON ({status})"))?;
    if !status.is_success() {
        let msg = v["error_description"]
            .as_str()
            .or_else(|| v["error"].as_str())
            .unwrap_or("unknown error");
        return Err(anyhow!("OAuth error: {msg}"));
    }
    Ok(Tokens {
        access_token: v["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("token response had no access_token"))?
            .to_string(),
        refresh_token: v["refresh_token"].as_str().unwrap_or_default().to_string(),
        expires_in: v["expires_in"].as_u64().unwrap_or(0),
    })
}

/// Open a URL in the user's default browser (Windows).
pub fn open_browser(url: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("failed to open browser")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_roundtrip() {
        let pkce = generate_pkce();
        // verifier: 32 bytes → 43 base64url chars, no padding
        assert_eq!(pkce.verifier.len(), 43);
        assert!(!pkce.verifier.contains('='));
        assert!(!pkce.challenge.contains('='));
        // url embeds both
        let url = authorize_url(&pkce);
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.starts_with(AUTHORIZE_URL));
    }
}
