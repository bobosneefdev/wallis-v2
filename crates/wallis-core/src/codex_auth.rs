//! ChatGPT (Codex) sign-in and live model catalog. Same public OAuth client and endpoints the Codex CLI and Pi
//! use. Wallis runs the browser flow itself, then writes the tokens to Pi's `auth.json` so Pi refreshes them.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CALLBACK_PORT: u16 = 1455;
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CATALOG_URL: &str = "https://chatgpt.com/backend-api/codex/models";

pub struct Pending { pub url: String, verifier: String, state: String }

#[derive(Clone, Debug, PartialEq)]
pub struct Credentials { pub access: String, pub refresh: String, pub expires_ms: u64, pub account_id: String }

fn random_token(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|error| format!("No secure random source: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

/// Starts a PKCE authorization: open `url` in the browser, then call `wait_for_code`.
pub fn begin() -> Result<Pending, String> {
    let verifier = random_token(32)?;
    let state = random_token(16)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let query = [("response_type", "code"), ("client_id", CLIENT_ID), ("redirect_uri", REDIRECT_URI), ("scope", "openid profile email offline_access"),
        ("code_challenge", &challenge), ("code_challenge_method", "S256"), ("state", &state), ("id_token_add_organizations", "true"), ("codex_cli_simplified_flow", "true"), ("originator", "wallis")]
        .map(|(key, value)| format!("{key}={}", urlencoding::encode(value))).join("&");
    Ok(Pending { url: format!("{AUTHORIZE_URL}?{query}"), verifier, state })
}

/// Extracts the authorization code from a callback request target, rejecting a mismatched `state`.
pub fn code_from_target(target: &str, state: &str) -> Result<String, String> {
    let query = target.split_once('?').map(|(_, query)| query).ok_or("The sign-in callback carried no result.")?;
    let find = |name: &str| query.split('&').filter_map(|pair| pair.split_once('=')).find(|(key, _)| *key == name).map(|(_, value)| urlencoding::decode(value).map(|v| v.into_owned()).unwrap_or_default());
    if let Some(error) = find("error") { return Err(format!("ChatGPT sign-in failed: {error}")); }
    if find("state").as_deref() != Some(state) { return Err("The sign-in response did not match this request.".into()); }
    find("code").filter(|code| !code.is_empty()).ok_or_else(|| "The sign-in response had no code.".into())
}

/// Blocks (up to `timeout`) for the browser to hit the loopback callback, then answers it with a small page.
pub fn wait_for_code(pending: &Pending, timeout: Duration) -> Result<String, String> {
    let listener = TcpListener::bind(("127.0.0.1", CALLBACK_PORT)).map_err(|error| format!("Port {CALLBACK_PORT} is busy (is Codex or Pi signing in?): {error}"))?;
    listener.set_nonblocking(true).map_err(|error| error.to_string())?;
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).ok();
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let target = request.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("");
                if !target.starts_with("/auth/callback") { let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"); continue; }
                let result = code_from_target(target, &pending.state);
                let body = if result.is_ok() { "Signed in. You can close this tab and return to Wallis." } else { "Sign-in failed. Return to Wallis for details." };
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() > deadline { return Err("Timed out waiting for ChatGPT sign-in.".into()); }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse { access_token: String, refresh_token: String, expires_in: u64 }

pub fn account_id(access_token: &str) -> Option<String> {
    let payload = URL_SAFE_NO_PAD.decode(access_token.split('.').nth(1)?).ok()?;
    let claims: Value = serde_json::from_slice(&payload).ok()?;
    claims.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")?.as_str().filter(|id| !id.is_empty()).map(String::from)
}

fn credentials_from(response: TokenResponse, now_ms: u64) -> Result<Credentials, String> {
    let account_id = account_id(&response.access_token).ok_or("The ChatGPT token had no account id.")?;
    Ok(Credentials { access: response.access_token, refresh: response.refresh_token, expires_ms: now_ms + response.expires_in * 1000, account_id })
}

fn now_ms() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }

pub fn exchange(pending: &Pending, code: &str) -> Result<Credentials, String> {
    let response: TokenResponse = ureq::post(TOKEN_URL)
        .send_form([("grant_type", "authorization_code"), ("client_id", CLIENT_ID), ("code", code), ("code_verifier", &pending.verifier), ("redirect_uri", REDIRECT_URI)])
        .map_err(|error| format!("ChatGPT token exchange failed: {error}"))?
        .body_mut().read_json().map_err(|error| format!("ChatGPT returned an unreadable token: {error}"))?;
    credentials_from(response, now_ms())
}

/// Trades a refresh token for new credentials (used before catalog fetches when the access token is stale).
pub fn refresh(credentials: &Credentials) -> Result<Credentials, String> {
    let response: TokenResponse = ureq::post(TOKEN_URL)
        .send_form([("grant_type", "refresh_token"), ("client_id", CLIENT_ID), ("refresh_token", &credentials.refresh)])
        .map_err(|error| format!("ChatGPT sign-in expired, sign in again: {error}"))?
        .body_mut().read_json().map_err(|error| format!("ChatGPT returned an unreadable token: {error}"))?;
    credentials_from(response, now_ms())
}

/// The `auth.json` object Pi reads for the `openai-codex` provider.
pub fn auth_json(credentials: &Credentials) -> Value {
    json!({ "openai-codex": { "type": "oauth", "access": credentials.access, "refresh": credentials.refresh, "expires": credentials.expires_ms, "accountId": credentials.account_id } })
}

/// Merges the credential into an existing `auth.json` (other providers' keys are kept).
pub fn merge_auth(existing: &str, credentials: &Credentials) -> Result<String, String> {
    let mut root: Value = if existing.trim().is_empty() { json!({}) } else { serde_json::from_str(existing).map_err(|error| format!("Pi auth.json is invalid: {error}"))? };
    root.as_object_mut().ok_or("Pi auth.json is not an object.")?.insert("openai-codex".into(), auth_json(credentials)["openai-codex"].clone());
    serde_json::to_string_pretty(&root).map_err(|error| error.to_string())
}

/// Reads the signed-in account from Pi's `auth.json`, if any (used to show "Signed in" in Settings).
pub fn signed_in(auth_json: &str) -> bool {
    serde_json::from_str::<Value>(auth_json).ok().and_then(|root| root.pointer("/openai-codex/type").and_then(Value::as_str).map(|kind| kind == "oauth")).unwrap_or(false)
}

/// Live model list for this ChatGPT account, in the shape `catalog::parse_codex_catalog` reads.
pub fn fetch_catalog(credentials: &Credentials, client_version: &str) -> Result<String, String> {
    ureq::get(CATALOG_URL).query("client_version", client_version)
        .header("Authorization", format!("Bearer {}", credentials.access)).header("chatgpt-account-id", &credentials.account_id)
        .call().map_err(|error| format!("Unable to fetch the Codex model list: {error}"))?
        .body_mut().read_to_string().map_err(|error| format!("Unable to read the Codex model list: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(account: &str) -> String {
        let claims = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": account } });
        format!("h.{}.s", URL_SAFE_NO_PAD.encode(claims.to_string()))
    }

    #[test]
    fn authorize_url_uses_pkce_s256_and_the_codex_client() {
        let pending = begin().unwrap();
        assert!(pending.url.starts_with("https://auth.openai.com/oauth/authorize?"));
        for part in ["response_type=code", "code_challenge_method=S256", "client_id=app_EMoamEEZ73f0CkXaXp7hrann", "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback", "offline_access"] { assert!(pending.url.contains(part), "{part}"); }
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.as_bytes()));
        assert!(pending.url.contains(&format!("code_challenge={challenge}")));
        assert_ne!(begin().unwrap().state, pending.state, "state must be random per attempt");
    }

    #[test]
    fn callback_requires_matching_state_and_a_code() {
        assert_eq!(code_from_target("/auth/callback?code=abc%20d&state=s1", "s1").unwrap(), "abc d");
        assert!(code_from_target("/auth/callback?code=abc&state=other", "s1").is_err(), "CSRF state must match");
        assert!(code_from_target("/auth/callback?error=access_denied&state=s1", "s1").unwrap_err().contains("access_denied"));
        assert!(code_from_target("/auth/callback?state=s1", "s1").is_err());
    }

    #[test]
    fn reads_account_id_and_writes_pi_auth_without_dropping_other_providers() {
        let credentials = credentials_from(TokenResponse { access_token: jwt("acct-1"), refresh_token: "r".into(), expires_in: 3600 }, 1000).unwrap();
        assert_eq!((credentials.account_id.as_str(), credentials.expires_ms), ("acct-1", 3_601_000));
        assert!(credentials_from(TokenResponse { access_token: "no-jwt".into(), refresh_token: "r".into(), expires_in: 1 }, 0).is_err());
        let merged: Value = serde_json::from_str(&merge_auth(r#"{"openrouter":{"type":"api_key","key":"k"}}"#, &credentials).unwrap()).unwrap();
        assert_eq!(merged["openrouter"]["key"], "k");
        assert_eq!(merged["openai-codex"]["type"], "oauth");
        assert_eq!(merged["openai-codex"]["accountId"], "acct-1");
        assert!(signed_in(&merged.to_string()) && !signed_in("{}") && !signed_in("garbage"));
    }
}
