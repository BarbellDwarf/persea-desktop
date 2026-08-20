//! Scoped-token store for the desktop sign-in flow (D0 plumbing, D1
//! acquisition, D2 expiry awareness).
//!
//! Expiry awareness (D2): reads meant for presentation to a server go
//! through [`load_valid`], which never hands out a record past
//! [`TOKEN_TTL_SECS`]; [`freshness`] classifies how far a stored token
//! is from its expiry so the shell can surface an interactive renew
//! sign-in (there is no silent refresh: the app never stores the
//! password).
//!
//! The scoped token is the credential the desktop shell uses to act on a
//! server with the identity of the signed-in user. It is stored as a JSON
//! record in the OS keychain through the keyring abstraction, keyed by
//! `<instance_url>/scoped-token`; it never touches plain files or the
//! webview, and neither the token nor the signing-in user's password is
//! ever logged.
//!
//! [`cmd_token_acquire`] performs the desktop handshake against the
//! server's own login endpoints: an anonymous `GET /` bootstraps the
//! readable `csrf_token` cookie, a form-encoded `POST /auth/login` with
//! `desktop=true` exchanges the credentials for the token page, and the
//! scoped token plus its expiry are parsed straight out of that HTML.
//! Redirects are classified into user-actionable failures (wrong
//! credentials, locked account, MFA required) instead of being followed.
//!
//! The error classifier stays contract surface for the integration
//! tickets that consume it; those not-yet-consumed items carry an allow
//! until their consumers wire in.
#![allow(dead_code)]

use std::time::Duration;

use reqwest::header;
use serde::{Deserialize, Serialize};

use crate::http::CSRF_COOKIE;
use crate::keyring::{self, SERVICE_NAME};

/// Timeouts for the login handshake, mirroring the shell HTTP client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

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

/// How long before the TTL a stored token counts as "expiring": inside
/// this window the shell surfaces its renew offer (Settings banner).
pub const TOKEN_RENEWAL_WINDOW_SECS: u64 = 30 * 60;

/// How far a stored token is from its expiry, as the renewal surfacing
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenFreshness {
    /// Valid with more than [`TOKEN_RENEWAL_WINDOW_SECS`] left.
    Fresh,
    /// Valid but inside the renewal window; the shell offers an
    /// interactive renew sign-in (no silent refresh exists: the app
    /// never stores the password).
    Expiring,
    /// Past [`TOKEN_TTL_SECS`]; the server rejects it.
    Expired,
}

/// [`TokenFreshness`] of `record` at `now_secs`. Clock skew (now before
/// `issued_at`) counts as fresh, matching [`is_expired`].
pub fn freshness(record: &TokenRecord, now_secs: u64) -> TokenFreshness {
    if is_expired(record, now_secs) {
        TokenFreshness::Expired
    } else if now_secs.saturating_sub(record.issued_at)
        >= TOKEN_TTL_SECS - TOKEN_RENEWAL_WINDOW_SECS
    {
        TokenFreshness::Expiring
    } else {
        TokenFreshness::Fresh
    }
}

/// The expiry-aware read decision, pure for tests: a record only comes
/// back when it exists and is still inside its TTL.
fn unexpired(record: Option<TokenRecord>, now_secs: u64) -> Option<TokenRecord> {
    record.filter(|r| !is_expired(r, now_secs))
}

/// Reads the stored record for `instance_url`, but never hands out an
/// expired one: `Ok(None)` when nothing is stored or the record is past
/// [`TOKEN_TTL_SECS`]. Callers that present the token to a server must
/// use this instead of [`get_token`].
pub async fn load_valid(
    app: tauri::AppHandle,
    instance_url: &str,
) -> Result<Option<TokenRecord>, String> {
    let record = get_token(app, instance_url).await?;
    Ok(unexpired(record, now_secs()))
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
    /// The server reports the account as locked.
    AccountLocked,
    /// The account wants a multi-factor check the desktop prompt cannot
    /// perform; the user completes it in the server's web page first.
    MfaRequired,
    /// Anything not recognized; fail closed.
    Other,
}

/// Classifies a raw error string from the server/HTTP layer. Checks the
/// token-invalidated markers first, then the network markers, then the
/// credentials markers, then lockout and MFA; anything else is
/// [`AuthFailure::Other`].
pub fn classify_auth_error(raw: &str) -> AuthFailure {
    let lower = raw.to_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|marker| lower.contains(marker));
    if has(&["401", "expired", "invalidated", "token"]) {
        AuthFailure::TokenInvalidated
    } else if has(&["refused", "timed out", "timeout", "resolve", "lookup"]) {
        AuthFailure::Network
    } else if has(&["login failed", "wrong password", "403"]) {
        AuthFailure::Credentials
    } else if has(&["account locked", "locked"]) {
        AuthFailure::AccountLocked
    } else if has(&["mfa"]) {
        AuthFailure::MfaRequired
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

// ---------------------------------------------------------------------------
// Desktop login handshake (D1)
// ---------------------------------------------------------------------------

/// Acquires a scoped token for `url` on behalf of `username` and stores
/// it in the OS keychain for the instance.
///
/// The handshake never logs the password or the token; errors carry only
/// status codes and redirect targets. Returns the acquired token as a
/// [`TokenView`] so the login page can show the remaining validity.
#[tauri::command]
pub async fn cmd_token_acquire(
    app: tauri::AppHandle,
    url: String,
    username: String,
    password: String,
) -> Result<TokenView, String> {
    let base = url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err("a server URL is required".to_string());
    }
    let username = username.trim();
    if username.is_empty() {
        return Err("a username is required".to_string());
    }
    if password.is_empty() {
        return Err("a password is required".to_string());
    }
    let client = login_client(allow_insecure_tls_for(&base))?;
    let (token, ttl_secs) = fetch_scoped_token(&client, &base, username, &password).await?;
    set_token(app, &base, &token).await?;
    Ok(TokenView { token, ttl_secs })
}

/// The untrusted-TLS override for the login handshake: the instance's
/// effective probe flag when configured, else the global shell toggle
/// (same rule as the connection probe).
fn allow_insecure_tls_for(instance_url: &str) -> bool {
    crate::instances::instance(instance_url)
        .map(|i| i.allow_insecure_tls_effective())
        .unwrap_or_else(crate::shell_config::allow_insecure_tls)
}

/// One-shot client for the login handshake. Redirects are disabled so
/// the server's 303 answers can be classified instead of followed; every
/// other knob mirrors the shared shell HTTP client.
fn login_client(allow_insecure_tls: bool) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("persea-desktop/", env!("CARGO_PKG_VERSION")))
        .danger_accept_invalid_certs(allow_insecure_tls)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("could not build the login client: {e}"))
}

/// The wire half of the acquisition: returns `(token, ttl_secs)` without
/// touching storage, so tests can drive it against the mock server.
async fn fetch_scoped_token(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<(String, u64), String> {
    // 1. Bootstrap: the anonymous GET to `/` serves the login form and
    //    sets the readable csrf_token cookie.
    let bootstrap = client
        .get(format!("{base_url}/"))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !bootstrap.status().is_success() {
        return Err(format!(
            "the server login page could not be loaded (status {})",
            bootstrap.status()
        ));
    }
    let csrf = extract_cookie(&bootstrap, CSRF_COOKIE)
        .ok_or_else(|| "the server did not provide a login form token".to_string())?;

    // 2. Sign in: form-encoded credentials, browser-style. The cookie is
    //    echoed and the csrf value repeats in the form body; desktop=true
    //    asks for the scoped-token page instead of a browser session.
    let form = [
        ("csrf_token", csrf.as_str()),
        ("username", username),
        ("password", password),
        ("desktop", "true"),
    ];
    let login = client
        .post(format!("{base_url}/auth/login"))
        .header(header::COOKIE, format!("{CSRF_COOKIE}={csrf}"))
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    // 3. Classify the answer. A redirect means the server refused the
    //    sign-in; the Location target says why.
    let status = login.status();
    if status.is_redirection() {
        let location = login
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        return Err(classify_login_redirect(&location));
    }
    if !status.is_success() {
        return Err(format!(
            "the server could not complete the sign-in (status {status})"
        ));
    }
    let html = login
        .text()
        .await
        .map_err(|e| format!("could not read the sign-in response: {e}"))?;
    let token = parse_scoped_token(&html)
        .ok_or_else(|| "the sign-in response did not contain a scoped token".to_string())?;
    Ok((token, parse_expires_ttl(&html).unwrap_or(TOKEN_TTL_SECS)))
}

/// Maps the server's post-login redirect target to a user-actionable
/// error. `/?error=invalid_credentials` and `/?error=account_locked` are
/// the server's credential outcomes; `/auth/mfa` means the account wants
/// a second factor the desktop prompt cannot ask for yet.
fn classify_login_redirect(location: &str) -> String {
    let lower = location.to_ascii_lowercase();
    if lower.contains("/auth/mfa") {
        "mfa required: finish the multi-factor check on the server's web \
         page first, then sign in here again"
            .to_string()
    } else if lower.contains("error=account_locked") {
        "account locked: the server reports this account is locked".to_string()
    } else if lower.contains("error=") {
        "login failed: the server rejected the username or password".to_string()
    } else {
        "the server sent the sign-in to an unexpected location".to_string()
    }
}

/// Pulls the scoped token out of the success page: the value of the
/// `<input id="scoped-token">` field. Plain string parsing; the server
/// renders the input with `id` before `value`.
fn parse_scoped_token(html: &str) -> Option<String> {
    const ID_MARKER: &str = r#"id="scoped-token""#;
    const VALUE_MARKER: &str = r#"value=""#;
    let after_id = html.find(ID_MARKER)? + ID_MARKER.len();
    let rest = &html[after_id..];
    let start = rest.find(VALUE_MARKER)? + VALUE_MARKER.len();
    let end = rest[start..].find('"')?;
    let value = rest[start..start + end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Remaining validity of the scoped token, parsed from the page's
/// `Expires: <RFC3339>` line and clamped to the known TTL. `None` when
/// the line is missing or malformed (the caller falls back to the TTL).
fn parse_expires_ttl(html: &str) -> Option<u64> {
    const MARKER: &str = "Expires:";
    let idx = html.find(MARKER)?;
    let rest = html[idx + MARKER.len()..].trim_start();
    let end = rest.find(['\n', '\r', '<']).unwrap_or(rest.len());
    let expiry = parse_rfc3339(rest[..end].trim())?;
    Some(expiry.saturating_sub(now_secs()).min(TOKEN_TTL_SECS))
}

/// Minimal RFC3339 parser (`YYYY-MM-DDTHH:MM:SS[.fff]` with an optional
/// `Z`/`z` or `±HH:MM` offset), returning unix seconds. Written by hand
/// so one timestamp costs no new dependency; the days-from-civil
/// arithmetic is proleptic-Gregorian exact.
fn parse_rfc3339(raw: &str) -> Option<u64> {
    let s = raw.trim();
    let (date, time) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let time = time.trim_end_matches(|c| c == 'Z' || c == 'z');
    let (hms, offset) = match time.find(['+', '-']) {
        Some(idx) => (&time[..idx], Some(&time[idx..])),
        None => (time, None),
    };
    let mut clock = hms.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock
        .next()
        .unwrap_or("0")
        .split('.')
        .next()
        .unwrap_or("0")
        .parse()
        .ok()?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let offset_secs: i64 = match offset {
        None => 0,
        Some(off) => {
            let (sign, digits) = off.split_at(1);
            let mut parts = digits.split(':');
            let hours: i64 = parts.next()?.parse().ok()?;
            let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
            let magnitude = hours * 3600 + minutes * 60;
            if sign == "-" {
                -magnitude
            } else {
                magnitude
            }
        }
    };

    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second
        - offset_secs;
    u64::try_from(secs).ok()
}

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard
/// Hinnant's days_from_civil algorithm).
fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Pulls `name` from a `Set-Cookie` response header, if present. Cookie
/// attributes beyond the first `;` are dropped. (Same contract as the
/// shell HTTP client's private helper.)
fn extract_cookie(resp: &reqwest::Response, name: &str) -> Option<String> {
    for value in resp.headers().get_all(header::SET_COOKIE) {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        let pair = raw.split_once(';').map(|(pair, _)| pair).unwrap_or(raw);
        let (key, val) = pair.split_once('=')?;
        if key.trim() == name {
            return Some(val.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_mock::{MockResponse, MockScript, MockServer};

    fn record(token: &str, issued_at: u64) -> TokenRecord {
        TokenRecord {
            token: token.to_string(),
            issued_at,
        }
    }

    fn html_response(
        status: u16,
        reason: &'static str,
        headers: Vec<(String, String)>,
        body: &str,
    ) -> MockResponse {
        MockResponse {
            status,
            reason,
            headers,
            body: body.to_string(),
        }
    }

    fn cookie_header(value: &str) -> (String, String) {
        (
            "Set-Cookie".to_string(),
            format!("{CSRF_COOKIE}={value}; Path=/"),
        )
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
    fn freshness_marks_the_renewal_window() {
        let rec = record("t", 1_000);
        let window_start = 1_000 + TOKEN_TTL_SECS - TOKEN_RENEWAL_WINDOW_SECS;
        assert_eq!(freshness(&rec, 1_000), TokenFreshness::Fresh);
        assert_eq!(freshness(&rec, window_start - 1), TokenFreshness::Fresh);
        // The window start itself is already "expiring".
        assert_eq!(freshness(&rec, window_start), TokenFreshness::Expiring);
        assert_eq!(
            freshness(&rec, 1_000 + TOKEN_TTL_SECS - 1),
            TokenFreshness::Expiring
        );
        // The TTL boundary flips straight to expired.
        assert_eq!(
            freshness(&rec, 1_000 + TOKEN_TTL_SECS),
            TokenFreshness::Expired
        );
    }

    #[test]
    fn freshness_treats_clock_skew_as_fresh() {
        assert_eq!(freshness(&record("t", 2_000), 1_000), TokenFreshness::Fresh);
    }

    #[test]
    fn unexpired_drops_missing_and_expired_records() {
        let rec = record("t", 1_000);
        assert_eq!(unexpired(None, 2_000), None);
        assert_eq!(unexpired(Some(rec.clone()), 1_000 + TOKEN_TTL_SECS), None);
        assert!(unexpired(Some(rec), 1_000 + TOKEN_TTL_SECS - 1).is_some());
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
    fn classify_lockout_and_mfa_markers() {
        for raw in [
            "account locked: the server reports this account is locked",
            "the account is locked",
        ] {
            assert_eq!(
                classify_auth_error(raw),
                AuthFailure::AccountLocked,
                "{raw}"
            );
        }
        assert_eq!(
            classify_auth_error(
                "mfa required: finish the multi-factor check on the server's web \
                 page first, then sign in here again"
            ),
            AuthFailure::MfaRequired
        );
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

    #[test]
    fn login_redirects_classify_into_actionable_errors() {
        for (location, marker, class) in [
            (
                "/?error=invalid_credentials",
                "rejected the username or password",
                AuthFailure::Credentials,
            ),
            (
                "/?error=account_locked",
                "account locked",
                AuthFailure::AccountLocked,
            ),
            (
                "/auth/mfa?state=abc",
                "mfa required",
                AuthFailure::MfaRequired,
            ),
            ("/somewhere-else", "unexpected location", AuthFailure::Other),
        ] {
            let err = classify_login_redirect(location);
            assert!(err.contains(marker), "{location}: {err}");
            assert_eq!(classify_auth_error(&err), class, "{location}: {err}");
        }
    }

    #[test]
    fn scoped_token_parser_reads_the_contract_page() {
        let html = r#"<input type="text" id="scoped-token" value="rgu_abc">"#;
        assert_eq!(parse_scoped_token(html).as_deref(), Some("rgu_abc"));
        assert_eq!(
            parse_scoped_token(r#"id="scoped-token" value="  rgu_x  ""#).as_deref(),
            Some("rgu_x")
        );
        // Value rendered before the id (contract renders id first),
        // empty value, absent field: no token.
        assert_eq!(
            parse_scoped_token(r#"value="earlier" id="scoped-token""#),
            None
        );
        assert_eq!(parse_scoped_token(r#"id="scoped-token" value="""#), None);
        assert_eq!(parse_scoped_token("no token here"), None);
    }

    #[test]
    fn rfc3339_parser_handles_utc_offsets_and_fractions() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-08-20T00:00:00Z"), Some(1_787_184_000));
        assert_eq!(
            parse_rfc3339("2026-08-20T12:00:00+02:00"),
            Some(1_787_220_000)
        );
        assert_eq!(
            parse_rfc3339("2026-08-20T00:00:00.500Z"),
            Some(1_787_184_000)
        );
        assert_eq!(parse_rfc3339("2026-08-20 06:30:00Z"), Some(1_787_207_400));
        assert_eq!(parse_rfc3339("not-a-date"), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("2026-08-20T99:00:00Z"), None);
    }

    #[test]
    fn expires_line_yields_clamped_ttl() {
        assert_eq!(
            parse_expires_ttl("Desktop Connected\nExpires: 2099-01-01T00:00:00Z"),
            Some(TOKEN_TTL_SECS)
        );
        // Long-past expiry clamps to zero, never wraps.
        assert_eq!(parse_expires_ttl("Expires: 1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_expires_ttl("no expiry line"), None);
        assert_eq!(parse_expires_ttl("Expires: garbage"), None);
    }

    #[test]
    fn login_handshake_posts_form_and_parses_token_page() {
        let server = MockServer::start(MockScript::new(vec![
            html_response(
                200,
                "OK",
                vec![
                    ("Content-Type".to_string(), "text/html".to_string()),
                    cookie_header("csrf-login-1"),
                ],
                "<html>sign in</html>",
            ),
            html_response(
                200,
                "OK",
                vec![("Content-Type".to_string(), "text/html".to_string())],
                "<html><body>Desktop Connected \
                 <input type=\"text\" id=\"scoped-token\" value=\"rgu-test-value\">\
                 Expires: 2099-01-01T00:00:00Z</body></html>",
            ),
        ]));
        let client = login_client(false).expect("client");
        let (token, ttl) = tauri::async_runtime::block_on(fetch_scoped_token(
            &client,
            &server.url(),
            "alice",
            "correct-horse",
        ))
        .expect("acquire");
        assert_eq!(token, "rgu-test-value");
        assert_eq!(ttl, TOKEN_TTL_SECS);

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/");
        let posted = &requests[1];
        assert_eq!(posted.method, "POST");
        assert_eq!(posted.path, "/auth/login");
        assert_eq!(
            posted.headers.get("cookie").map(String::as_str),
            Some("csrf_token=csrf-login-1")
        );
        assert!(posted.body.contains("csrf_token=csrf-login-1"));
        assert!(posted.body.contains("username=alice"));
        assert!(posted.body.contains("desktop=true"));
    }

    #[test]
    fn login_handshake_surfaces_invalid_credentials() {
        let server = MockServer::start(MockScript::new(vec![
            html_response(
                200,
                "OK",
                vec![cookie_header("csrf-login-2")],
                "<html>form</html>",
            ),
            MockResponse {
                status: 303,
                reason: "See Other",
                headers: vec![(
                    "Location".to_string(),
                    "/?error=invalid_credentials".to_string(),
                )],
                body: String::new(),
            },
        ]));
        let client = login_client(false).expect("client");
        let err = tauri::async_runtime::block_on(fetch_scoped_token(
            &client,
            &server.url(),
            "alice",
            "nope",
        ))
        .expect_err("redirect must fail");
        assert_eq!(classify_auth_error(&err), AuthFailure::Credentials);
    }

    #[test]
    fn login_handshake_surfaces_mfa_requirement() {
        let server = MockServer::start(MockScript::new(vec![
            html_response(
                200,
                "OK",
                vec![cookie_header("csrf-login-3")],
                "<html>form</html>",
            ),
            MockResponse {
                status: 303,
                reason: "See Other",
                headers: vec![("Location".to_string(), "/auth/mfa?next=%2F".to_string())],
                body: String::new(),
            },
        ]));
        let client = login_client(false).expect("client");
        let err = tauri::async_runtime::block_on(fetch_scoped_token(
            &client,
            &server.url(),
            "alice",
            "nope",
        ))
        .expect_err("redirect must fail");
        assert_eq!(classify_auth_error(&err), AuthFailure::MfaRequired);
    }

    #[test]
    fn login_handshake_fails_without_csrf_cookie() {
        let mut script = MockScript::new(vec![html_response(
            200,
            "OK",
            Vec::new(),
            "<html>no cookie set</html>",
        )]);
        script.repeat_last = true;
        let server = MockServer::start(script);
        let client = login_client(false).expect("client");
        let err = tauri::async_runtime::block_on(fetch_scoped_token(
            &client,
            &server.url(),
            "alice",
            "nope",
        ))
        .expect_err("missing cookie must fail");
        assert!(err.contains("did not provide a login form token"));
    }

    #[test]
    fn login_handshake_reports_unusable_login_page() {
        let mut script = MockScript::new(vec![MockResponse {
            status: 500,
            reason: "Internal Server Error",
            headers: Vec::new(),
            body: String::new(),
        }]);
        script.repeat_last = true;
        let server = MockServer::start(script);
        let client = login_client(false).expect("client");
        let err = tauri::async_runtime::block_on(fetch_scoped_token(
            &client,
            &server.url(),
            "alice",
            "nope",
        ))
        .expect_err("500 bootstrap must fail");
        assert!(err.contains("login page could not be loaded"));
    }
}
