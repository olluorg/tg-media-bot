//! Google WebSub (PubSubHubbub) subscriber client and webhook server.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use tokio::sync::mpsc::UnboundedSender;

use super::resolver;
use super::types::NewVideoNotification;

pub struct WebSubState {
    pub secret: Option<String>,
    pub notify_tx: UnboundedSender<NewVideoNotification>,
}

pub fn router(state: Arc<WebSubState>) -> axum::Router {
    axum::Router::new().route("/websub", axum::routing::get(handle_challenge).post(handle_feed)).with_state(state)
}

/// Handles Google Hub verification GET request:
/// Hub sends hub.mode, hub.topic, hub.challenge, hub.lease_seconds.
/// Must echo hub.challenge with 200 OK.
async fn handle_challenge(Query(params): Query<HashMap<String, String>>) -> impl IntoResponse {
    let mode = params.get("hub.mode").map(String::as_str);
    let challenge = params.get("hub.challenge");

    if let (Some("subscribe" | "unsubscribe"), Some(chal)) = (mode, challenge) {
        log::info!("WebSub challenge received for mode: {:?}, topic: {:?}", mode, params.get("hub.topic"));
        (StatusCode::OK, chal.clone())
    } else {
        log::warn!("WebSub challenge invalid params: {:?}", params);
        (StatusCode::NOT_FOUND, "invalid challenge".to_string())
    }
}

/// Handles Google Hub Atom XML push POST request.
async fn handle_feed(State(state): State<Arc<WebSubState>>, headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    // 1. Verify HMAC signature if secret is configured
    if let Some(secret) = &state.secret {
        let sig = headers.get("x-hub-signature").and_then(|h| h.to_str().ok()).unwrap_or_default();
        if !verify_signature(secret, &body, sig) {
            log::warn!("WebSub invalid signature from Google Hub: {sig}");
            return (StatusCode::FORBIDDEN, "invalid signature");
        }
    }

    // 2. Parse Atom feed
    let xml = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid utf-8"),
    };

    let (channel_title, entries) = match resolver::parse_feed(xml) {
        Ok(res) => res,
        Err(e) => {
            log::warn!("WebSub XML parse error: {e}");
            return (StatusCode::OK, "parsed");
        }
    };

    for entry in entries {
        let notif = NewVideoNotification {
            channel_id: entry.channel_id,
            channel_title: if !channel_title.is_empty() { channel_title.clone() } else { entry.author_name },
            video_id: entry.video_id,
            video_title: entry.title,
            published_at: entry.published_at,
        };
        let _ = state.notify_tx.send(notif);
    }

    (StatusCode::OK, "received")
}

/// Sends subscribe or unsubscribe request to Google WebSub Hub.
pub async fn send_hub_request(
    http: &reqwest::Client,
    callback_url: &str,
    channel_id: &str,
    mode: &str, // "subscribe" or "unsubscribe"
    secret: Option<&str>,
    lease_seconds: u64,
) -> Result<(), String> {
    let topic = format!("https://www.youtube.com/xml/feeds/videos.xml?channel_id={channel_id}");
    let mut form = HashMap::new();
    form.insert("hub.callback", callback_url);
    form.insert("hub.mode", mode);
    form.insert("hub.topic", &topic);

    let lease_str = lease_seconds.to_string();
    if mode == "subscribe" {
        form.insert("hub.lease_seconds", &lease_str);
    }
    if let Some(sec) = secret {
        form.insert("hub.secret", sec);
    }

    let resp = http
        .post("https://pubsubhubbub.appspot.com/subscribe")
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("hub request failed: {e}"))?;

    let status = resp.status();
    if status == reqwest::StatusCode::ACCEPTED || status == reqwest::StatusCode::OK {
        log::info!("WebSub hub request {mode} accepted for channel {channel_id}");
        Ok(())
    } else {
        let err_text = resp.text().await.unwrap_or_default();
        log::warn!("WebSub hub request {mode} for channel {channel_id} returned {status}: {err_text}");
        Err(format!("hub returned status {status}: {err_text}"))
    }
}

pub fn verify_signature(secret: &str, body: &[u8], signature_header: &str) -> bool {
    let Some(hex_sig) = signature_header.strip_prefix("sha1=") else {
        return false;
    };
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret.as_bytes());
    let tag = ring::hmac::sign(&key, body);
    let expected_hex = hex_encode(tag.as_ref());
    expected_hex.eq_ignore_ascii_case(hex_sig)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_verification() {
        let secret = "supersecret123";
        let body = b"hello world atom feed";
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret.as_bytes());
        let tag = ring::hmac::sign(&key, body);
        let valid_header = format!("sha1={}", hex_encode(tag.as_ref()));

        assert!(verify_signature(secret, body, &valid_header));
        assert!(!verify_signature("wrong_secret", body, &valid_header));
        assert!(!verify_signature(secret, b"tampered", &valid_header));
        assert!(!verify_signature(secret, body, "md5=12345"));
    }
}
