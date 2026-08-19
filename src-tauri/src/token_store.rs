//! Scoped-token store for the enterprise LDAP sign-in flow (D0).
//!
//! The scoped token is the credential the desktop shell uses to act on a
//! server with the identity of the signed-in user. It is stored as a JSON
//! record in the OS keychain through the keyring abstraction, keyed by
//! `<instance_url>/scoped-token`; it never touches plain files or the
//! webview. The server-side token endpoint lands with persea#227, so
//! [`cmd_token_acquire`] is a stub: the login page and the integration
//! tickets have a stable command to call and get a clear error until then.
//!
//! The store helpers and the error classifier are consumed by the
//! integration tickets that land after this module; the not-yet-consumed
//! items carry an allow until their consumers wire in.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::keyring::{self, SERVICE_NAME};

/// A stored scoped token and the moment it was issued (unix seconds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRecord {
    pub token: String,
    pub issued_at: u64,
}

/// Scoped tokens expire 12 hours after they are issued.
pub const TOKEN_TTL_SECS: u64 = 12 * 3600;

fn keyring_user(instance_url: &str) -> String {
    format!("{instance_url}/scoped-token")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// True when `now_secs` is past the record's TTL. Clock skew (now before
/// `issued_at`) never counts as expired.
pub fn is_expired(record: &TokenRecord, now_secs: u64) -> bool {
    now_secs.saturating_sub(record.issued_at) >= TOKEN_TTL_SECS
}

/// Stores the token record for `instance_url` in the OS keychain.
pub async fn set_token(
    app: tauri::AppHandle,
    instance_url: &str,
    token: &str,
) -> Result<(), String> {
    let record = TokenRecord {
        token: token.to_string(),
        issued_at: now_secs(),
    };
    let json = serde_json::to_string(&record)
        .map_err(|e| format!("cannot serialize token record: {e}"))?;
    keyring::keyring_set(
        SERVICE_NAME.to_string(),
        keyring_user(instance_url),
        json,
        app,
    )
    .await
}

/// Reads the stored record for `instance_url`; `None` when nothing is
/// stored for the pair.
pub async fn get_token(
    app: tauri::AppHandle,
    instance_url: &str,
) -> Result<Option<TokenRecord>, String> {
    let Some(raw) =
        keyring::keyring_get(SERVICE_NAME.to_string(), keyring_user(instance_url), app).await?
    else {
        return Ok(None);
    };
    let record =
        serde_json::from_str(&raw).map_err(|e| format!("cannot parse stored token record: {e}"))?;
    Ok(Some(record))
}

/// Deletes the stored record for `instance_url`; `false` when none existed.
pub async fn delete_token(app: tauri::AppHandle, instance_url: &str) -> Result<bool, String> {
    keyring::keyring_delete(SERVICE_NAME.to_string(), keyring_user(instance_url), app).await
}

/// Class of a failed token-acquisition attempt, so the shell can pick the
/// right message and recovery path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    /// The stored token was rejected by the server.
    TokenInvalidated,
    /// The server could not be reached.
    Network,
    /// The username or password was wrong.
    Credentials,
    /// Anything not recognized; fail closed.
    Other,
}

/// Classifies a raw error string from the server/HTTP layer. Checks the
/// token-invalidated markers first, then the network markers, then the
/// credentials markers; anything else is [`AuthFailure::Other`].
pub fn classify_auth_error(raw: &str) -> AuthFailure {
    let lower = raw.to_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|marker| lower.contains(marker));
    if has(&["401", "expired", "invalidated", "token"]) {
        AuthFailure::TokenInvalidated
    } else if has(&["refused", "timed out", "timeout", "resolve", "lookup"]) {
        AuthFailure::Network
    } else if has(&["login failed", "wrong password", "403"]) {
        AuthFailure::Credentials
    } else {
        AuthFailure::Other
    }
}

/// A freshly acquired scoped token and how long it stays valid.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenView {
    pub token: String,
    pub ttl_secs: u64,
}

/// Acquires a scoped token for `url` on behalf of `username`.
///
/// Stub until the server endpoint lands (persea#227): the login page and
/// the integration tickets call this seam now, and it fails with a clear
/// error that names the pending server-side work.
#[tauri::command]
pub async fn cmd_token_acquire(
    _app: tauri::AppHandle,
    _url: String,
    _username: String,
    _password: String,
) -> Result<TokenView, String> {
    Err(
        "Scoped-token acquisition is not available yet: the server endpoint ships with \
         the server-side integration (persea#227)."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(token: &str, issued_at: u64) -> TokenRecord {
        TokenRecord {
            token: token.to_string(),
            issued_at,
        }
    }

    #[test]
    fn token_record_serializes_with_camel_case_fields() {
        let json = serde_json::to_string(&record("tok-1", 42)).expect("serialize");
        assert_eq!(json, r#"{"token":"tok-1","issuedAt":42}"#);
        let back: TokenRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, record("tok-1", 42));
    }

    #[test]
    fn token_record_deserializes_issued_at_as_camel_case() {
        let parsed: TokenRecord =
            serde_json::from_str(r#"{"token":"abc","issuedAt":123}"#).expect("parse");
        assert_eq!(parsed.token, "abc");
        assert_eq!(parsed.issued_at, 123);
    }

    #[test]
    fn token_expires_at_the_ttl_boundary() {
        let rec = record("t", 1_000);
        assert!(!is_expired(&rec, 1_000 + TOKEN_TTL_SECS - 1));
        assert!(is_expired(&rec, 1_000 + TOKEN_TTL_SECS));
        assert!(is_expired(&rec, 1_000 + TOKEN_TTL_SECS + 1));
    }

    #[test]
    fn token_never_expires_before_issue_time() {
        // Clock skew: now before issued_at must not count as expired.
        assert!(!is_expired(&record("t", 2_000), 1_000));
    }

    #[test]
    fn classify_token_invalidated_markers() {
        for raw in [
            "401 Unauthorized",
            "HTTP 401",
            "token expired",
            "token invalidated",
            "invalidated credential",
        ] {
            assert_eq!(
                classify_auth_error(raw),
                AuthFailure::TokenInvalidated,
                "{raw}"
            );
        }
    }

    #[test]
    fn classify_network_markers() {
        for raw in [
            "connection refused",
            "request timed out",
            "request timeout",
            "could not resolve host",
            "dns lookup failed",
        ] {
            assert_eq!(classify_auth_error(raw), AuthFailure::Network, "{raw}");
        }
    }

    #[test]
    fn classify_credentials_markers() {
        for raw in ["login failed", "wrong password", "403 Forbidden"] {
            assert_eq!(classify_auth_error(raw), AuthFailure::Credentials, "{raw}");
        }
    }

    #[test]
    fn classify_unknown_fails_closed_to_other() {
        assert_eq!(classify_auth_error(""), AuthFailure::Other);
        assert_eq!(classify_auth_error("boom"), AuthFailure::Other);
        assert_eq!(
            classify_auth_error("some unexpected server hiccup"),
            AuthFailure::Other
        );
    }
}
