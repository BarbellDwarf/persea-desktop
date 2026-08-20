//! Shell HTTP client: one shared reqwest client for every native shell
//! call, with Bearer auth and the server's CSRF double-submit contract.
//!
//! CSRF contract (server side): the `csrf_token` cookie is set on every
//! response and reused once present (the middleware re-sets the incoming
//! token instead of rotating it). State-changing methods
//! (POST/PUT/DELETE/PATCH) must echo the cookie and an `X-CSRF-Token`
//! header with the same value; GET/HEAD/OPTIONS are exempt. The client
//! bootstraps the cookie with one anonymous GET per instance
//! (`/api/auth/status`) and refreshes the stored value from every
//! response's `Set-Cookie`, so the bootstrap is invisible to the user
//! and survives server-side token changes.
//!
//! The anonymous device-code endpoints (`POST /api/desktop/pair`,
//! `GET /api/desktop/pair/status`) live outside the CSRF layer
//! server-side (no session to bind): they ignore the CSRF headers, so
//! routing them through the same state-changing path is harmless and
//! keeps the client uniform.
//!
//! Credential resolution (D3): every outgoing call carries exactly one
//! Authorization credential, decided per request. A caller-supplied
//! credential (the paired device token) always wins, so pairing stays
//! the default identity. When the caller has none, the instance's
//! stored scoped token fills in (`token_store::load_valid`,
//! expiry-aware): the desktop then acts as the signed-in user, which is
//! the only working identity on compliance-mode servers. The
//! deliberately anonymous endpoints (the CSRF bootstrap and the
//! device-code pairing handshake) never receive the fill-in, so the
//! pairing flow behaves identically whether or not a scoped token is
//! stored. A 401 answered to a call that went out with the scoped token
//! is routed once into the D2 invalidation path
//! (`bridge::scoped_token_rejected`): the keychain record clears and
//! the shell offers re-login. There is no retry inside the failing
//! request; the next caller-driven request re-resolves the cleared
//! credential and goes out unauthenticated. Token material never
//! appears in logs or errors.
//!
//! Consumption points:
//! - device pairing: `post` for `/api/desktop/pair`, `get` for
//!   `/api/desktop/pair/status`, `delete` (Bearer + CSRF) for
//!   `/api/me/tokens/{id}` revocation.
//! - session poller: `get` with the paired Bearer token.
//! - transfers: `put` with Bearer + CSRF for drive uploads.
//!
//! Every method takes the instance URL as configured in the instance
//! store, so one client serves every instance with per-instance CSRF
//! state. The reqwest client uses the default redirect policy (up to 10
//! hops), matching the server's setup-wizard redirect detection.

//! The `put` helper and the response body accessors are contract surface
//! for consumers that land after this module; they carry an allow until
//! wired in (each consumer removes what it consumes).
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use reqwest::{header, Method, StatusCode};
use serde_json::Value;

/// CSRF cookie name (server: `src/csrf.rs`).
pub const CSRF_COOKIE: &str = "csrf_token";
/// CSRF double-submit header (server: `src/csrf.rs`).
pub const CSRF_HEADER: &str = "x-csrf-token";
/// Anonymous bootstrap endpoint: any anonymous GET sets the cookie.
const BOOTSTRAP_PATH: &str = "/api/auth/status";
/// Device-code pairing start; anonymous by server contract.
const PAIR_PATH: &str = "/api/desktop/pair";
/// Device-code pairing status path (the code travels as the query).
const PAIR_STATUS_PATH: &str = "/api/desktop/pair/status";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const BOOTSTRAP_ATTEMPTS: u32 = 3;
const BOOTSTRAP_RETRY_DELAY: Duration = Duration::from_millis(750);

/// Parsed result of a shell HTTP call: status plus the JSON body.
#[derive(Debug, Clone)]
pub struct HttpResult {
    pub status: StatusCode,
    pub body: Value,
}

impl HttpResult {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// The server's `error` field, when present (the server renders
    /// failures as `{"error": "..."}`).
    pub fn server_error(&self) -> Option<String> {
        self.body
            .get("error")
            .and_then(|e| e.as_str())
            .map(str::to_string)
    }
}

/// The Authorization credential one outgoing call carries, decided by
/// [`ShellHttp::resolve_credential`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Credential {
    /// The caller's own credential (the paired device token): attached
    /// as-is, unchanged behavior.
    Caller(String),
    /// The instance's stored scoped token, filled in because the caller
    /// had no credential: the desktop acts as the signed-in user.
    Scoped(String),
    /// Nothing to attach.
    Anonymous,
}

impl Credential {
    /// The header value for the credential; `None` when anonymous.
    fn authorization_header(&self) -> Option<String> {
        match self {
            Credential::Caller(token) | Credential::Scoped(token) => {
                Some(format!("Bearer {token}"))
            }
            Credential::Anonymous => None,
        }
    }

    /// Whether the credential came from the scoped-token store (drives
    /// the 401 routing).
    fn is_scoped(&self) -> bool {
        matches!(self, Credential::Scoped(_))
    }
}

/// Shared shell HTTP client with per-instance CSRF state.
#[derive(Debug)]
pub struct ShellHttp {
    client: reqwest::Client,
    csrf: Mutex<HashMap<String, Option<String>>>,
    /// Test seam: when set, replaces the keychain read behind
    /// [`ShellHttp::stored_scoped_token`] (`Some(None)` models "no
    /// valid record stored").
    #[cfg(test)]
    scoped_stub: Mutex<Option<Option<String>>>,
}

impl ShellHttp {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("persea-desktop/", env!("CARGO_PKG_VERSION")))
            .danger_accept_invalid_certs(crate::shell_config::allow_insecure_tls())
            .build()
            .expect("reqwest client build cannot fail");
        Self {
            client,
            csrf: Mutex::new(HashMap::new()),
            #[cfg(test)]
            scoped_stub: Mutex::new(None),
        }
    }

    fn base_url(&self, instance_url: &str) -> String {
        instance_url.trim_end_matches('/').to_string()
    }

    /// GET request. GETs are CSRF-exempt. `bearer` is the caller's own
    /// credential (the paired device token); without one, the instance's
    /// stored scoped token fills in when valid (see the module docs).
    pub async fn get(
        &self,
        instance_url: &str,
        path: &str,
        bearer: Option<&str>,
    ) -> Result<HttpResult, String> {
        let url = format!("{}{}", self.base_url(instance_url), path);
        let credential = self.resolve_credential(instance_url, path, bearer).await;
        let mut req = self.client.get(&url);
        if let Some(value) = credential.authorization_header() {
            req = req.header(header::AUTHORIZATION, value);
        }
        self.dispatch(req, instance_url, credential.is_scoped())
            .await
    }

    /// Generic state-changing call with the CSRF double-submit contract:
    /// the `csrf_token` cookie and the `X-CSRF-Token` header are echoed
    /// on every POST/PUT/DELETE/PATCH, plus the resolved Authorization
    /// credential (caller's own first, stored scoped token as fill-in).
    pub async fn send(
        &self,
        method: Method,
        instance_url: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> Result<HttpResult, String> {
        let csrf = self.ensure_csrf(instance_url).await?;
        let url = format!("{}{}", self.base_url(instance_url), path);
        let credential = self.resolve_credential(instance_url, path, bearer).await;
        let mut req = self.client.request(method, &url);
        if let Some(value) = credential.authorization_header() {
            req = req.header(header::AUTHORIZATION, value);
        }
        if let Some(tok) = csrf {
            req = req
                .header(header::COOKIE, format!("{CSRF_COOKIE}={tok}"))
                .header(CSRF_HEADER, tok);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        self.dispatch(req, instance_url, credential.is_scoped())
            .await
    }

    pub async fn post(
        &self,
        instance_url: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> Result<HttpResult, String> {
        self.send(Method::POST, instance_url, path, bearer, body)
            .await
    }

    pub async fn put(
        &self,
        instance_url: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> Result<HttpResult, String> {
        self.send(Method::PUT, instance_url, path, bearer, body)
            .await
    }

    pub async fn delete(
        &self,
        instance_url: &str,
        path: &str,
        bearer: Option<&str>,
    ) -> Result<HttpResult, String> {
        self.send(Method::DELETE, instance_url, path, bearer, None)
            .await
    }

    // -------------------------------------------------------------------
    // Credential resolution and scoped-token 401 routing (D3)
    // -------------------------------------------------------------------

    /// Decides the Authorization credential of one outgoing call: the
    /// caller's own credential first; otherwise the instance's stored
    /// scoped token, unless the path is anonymous by contract.
    async fn resolve_credential(
        &self,
        instance_url: &str,
        path: &str,
        caller: Option<&str>,
    ) -> Credential {
        match caller {
            Some(token) => Credential::Caller(token.to_string()),
            None if anonymous_path(path) => Credential::Anonymous,
            None => {
                let stored = self.stored_scoped_token(instance_url).await;
                credential_precedence(None, stored)
            }
        }
    }

    /// The instance's stored scoped token, expiry-aware via
    /// `token_store::load_valid`. `None` before the app handle exists
    /// (early setup, tests) or when no unexpired record is stored; both
    /// degrade to the unauthenticated call, exactly as before D3.
    async fn stored_scoped_token(&self, instance_url: &str) -> Option<String> {
        #[cfg(test)]
        {
            let stubbed = self
                .scoped_stub
                .lock()
                .map(|slot| (*slot).clone())
                .unwrap_or_default();
            if let Some(token) = stubbed {
                return token;
            }
        }
        let handle = crate::bridge::app_handle()?.clone();
        let record = crate::token_store::load_valid(handle, instance_url)
            .await
            .ok()??;
        Some(record.token)
    }

    /// Single-shot 401 handling for a call that went out with the
    /// scoped token: hands the failure to the D2 invalidation routing
    /// once. No retry happens here; the next caller-driven request
    /// re-resolves the cleared credential.
    async fn route_scoped_rejection(&self, instance_url: &str, status: StatusCode, body: &Value) {
        let Some(handle) = crate::bridge::app_handle() else {
            return;
        };
        let raw = scoped_rejection_raw_error(status, body);
        crate::bridge::scoped_token_rejected(handle, instance_url, &raw).await;
    }

    /// Installs the test override for [`ShellHttp::stored_scoped_token`].
    #[cfg(test)]
    fn stub_scoped_token(&self, token: Option<String>) {
        if let Ok(mut slot) = self.scoped_stub.lock() {
            *slot = Some(token);
        }
    }

    /// CSRF bootstrap: one anonymous GET per instance, retried up to
    /// [`BOOTSTRAP_ATTEMPTS`] times with a short delay between attempts.
    /// Repeated calls return immediately once a token is stored.
    pub async fn bootstrap(&self, instance_url: &str) -> Result<(), String> {
        if self.csrf_token(instance_url).is_some() {
            return Ok(());
        }
        let mut last_error = "server set no csrf cookie".to_string();
        for attempt in 0..BOOTSTRAP_ATTEMPTS {
            if attempt > 0 {
                sleep(BOOTSTRAP_RETRY_DELAY).await;
            }
            match self.bootstrap_once(instance_url).await {
                Ok(Some(token)) => {
                    self.store_token(instance_url, token);
                    return Ok(());
                }
                Ok(None) => {
                    last_error = "server set no csrf cookie".to_string();
                }
                Err(err) => last_error = err,
            }
        }
        Err(format!(
            "CSRF bootstrap failed for {instance_url}: {last_error}"
        ))
    }

    async fn ensure_csrf(&self, instance_url: &str) -> Result<Option<String>, String> {
        if let Some(tok) = self.csrf_token(instance_url) {
            return Ok(Some(tok));
        }
        self.bootstrap(instance_url).await?;
        Ok(self.csrf_token(instance_url))
    }

    async fn bootstrap_once(&self, instance_url: &str) -> Result<Option<String>, String> {
        let url = format!("{}{}", self.base_url(instance_url), BOOTSTRAP_PATH);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        Ok(extract_cookie(&resp, CSRF_COOKIE))
    }

    async fn dispatch(
        &self,
        req: reqwest::RequestBuilder,
        instance_url: &str,
        used_scoped: bool,
    ) -> Result<HttpResult, String> {
        let resp = req
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        // The server re-sets the CSRF cookie on every response; refresh
        // the stored value so a rotated token never leaves the client
        // stale.
        if let Some(tok) = extract_cookie(&resp, CSRF_COOKIE) {
            self.store_token(instance_url, tok);
        }
        let status = resp.status();
        let body = match resp.json().await {
            Ok(value) => value,
            Err(_) => Value::Null,
        };
        if routes_scoped_rejection(used_scoped, status) {
            self.route_scoped_rejection(instance_url, status, &body)
                .await;
        }
        Ok(HttpResult { status, body })
    }

    fn store_token(&self, instance_url: &str, token: String) {
        if let Ok(mut map) = self.csrf.lock() {
            map.insert(instance_url.trim_end_matches('/').to_string(), Some(token));
        }
    }

    fn csrf_token(&self, instance_url: &str) -> Option<String> {
        self.csrf
            .lock()
            .ok()
            .and_then(|map| map.get(instance_url.trim_end_matches('/')).cloned())
            .flatten()
    }
}

impl Default for ShellHttp {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide shell HTTP client, shared by every native caller.
pub fn shell_http() -> &'static ShellHttp {
    static SHELL_HTTP: OnceLock<ShellHttp> = OnceLock::new();
    SHELL_HTTP.get_or_init(ShellHttp::new)
}

/// Sleep without a direct tokio dependency: the tauri async runtime's
/// blocking pool covers the pause.
pub async fn sleep(duration: Duration) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(duration)).await;
}

/// Pulls `name` from a `Set-Cookie` response header, if present. Cookie
/// attributes beyond the first `;` are dropped.
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

/// Pure precedence decision behind [`ShellHttp::resolve_credential`]
/// (test seam): the caller's own credential wins; the stored scoped
/// token only fills a missing one; neither means the call goes out
/// unauthenticated.
fn credential_precedence(caller: Option<&str>, stored: Option<String>) -> Credential {
    match caller {
        Some(token) => Credential::Caller(token.to_string()),
        None => match stored {
            Some(token) => Credential::Scoped(token),
            None => Credential::Anonymous,
        },
    }
}

/// Paths that stay anonymous even when a scoped token is stored: the
/// CSRF bootstrap and the device-code pairing handshake carry no
/// credential by server contract, so pairing works identically with or
/// without a desktop sign-in. The query string is ignored (the pairing
/// status poll carries its code there).
fn anonymous_path(path: &str) -> bool {
    let bare = path.split('?').next().unwrap_or(path);
    bare == BOOTSTRAP_PATH || bare == PAIR_PATH || bare == PAIR_STATUS_PATH
}

/// Whether a finished request routes into the D2 invalidation path:
/// only a 401 answered to a call that went out with the scoped token.
fn routes_scoped_rejection(used_scoped: bool, status: StatusCode) -> bool {
    used_scoped && status == StatusCode::UNAUTHORIZED
}

/// The raw failure string handed to the D2 routing: the status plus the
/// server's `error` field, so the classifier sees its 401 marker even
/// on a bodyless answer. The string never contains credential material.
fn scoped_rejection_raw_error(status: StatusCode, body: &Value) -> String {
    let detail = body
        .get("error")
        .and_then(|e| e.as_str())
        .unwrap_or_default();
    if detail.is_empty() {
        format!("HTTP {}", status.as_u16())
    } else {
        format!("HTTP {}: {detail}", status.as_u16())
    }
}

#[cfg(test)]
pub(crate) mod test_mock {
    //! In-process HTTP/1.1 mock server for shell HTTP tests. Serves a
    //! scripted sequence of responses and records every request.

    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[derive(Debug, Clone)]
    pub struct MockRequest {
        pub method: String,
        pub path: String,
        pub headers: HashMap<String, String>,
        pub body: String,
    }

    #[derive(Debug, Clone)]
    pub struct MockResponse {
        pub status: u16,
        pub reason: &'static str,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    /// Scripted responses, consumed in order. With `repeat_last` set, the
    /// final response replays once the queue is empty.
    #[derive(Debug, Clone, Default)]
    pub struct MockScript {
        pub responses: Vec<MockResponse>,
        pub repeat_last: bool,
        last: Option<MockResponse>,
    }

    impl MockScript {
        pub fn new(responses: Vec<MockResponse>) -> Self {
            Self {
                responses,
                repeat_last: false,
                last: None,
            }
        }
    }

    #[derive(Debug)]
    pub struct MockServer {
        pub addr: SocketAddr,
        pub requests: Arc<Mutex<Vec<MockRequest>>>,
        script: Arc<Mutex<MockScript>>,
    }

    impl MockServer {
        pub fn start(script: MockScript) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("mock server bind");
            let addr = listener.local_addr().expect("mock server address");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let script = Arc::new(Mutex::new(script));
            let reqs = Arc::clone(&requests);
            let sc = Arc::clone(&script);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        continue;
                    };
                    let reqs = Arc::clone(&reqs);
                    let sc = Arc::clone(&sc);
                    thread::spawn(move || handle_connection(stream, &reqs, &sc));
                }
            });
            Self {
                addr,
                requests,
                script,
            }
        }

        pub fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        pub fn requests(&self) -> Vec<MockRequest> {
            self.requests.lock().unwrap().clone()
        }

        pub fn set_script(&self, script: MockScript) {
            *self.script.lock().unwrap() = script;
        }
    }

    fn fallback() -> MockResponse {
        MockResponse {
            status: 404,
            reason: "Not Found",
            headers: Vec::new(),
            body: "{}".to_string(),
        }
    }

    fn next_response(script: &Mutex<MockScript>) -> MockResponse {
        let mut guard = script.lock().unwrap();
        if guard.responses.is_empty() {
            if guard.repeat_last {
                return guard.last.clone().unwrap_or_else(fallback);
            }
            return fallback();
        }
        let resp = guard.responses.remove(0);
        guard.last = Some(resp.clone());
        resp
    }

    fn handle_connection(
        mut stream: TcpStream,
        requests: &Mutex<Vec<MockRequest>>,
        script: &Mutex<MockScript>,
    ) {
        let mut reader = BufReader::new(stream.try_clone().expect("stream clone"));
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
            return;
        }
        let mut headers: HashMap<String, String> = HashMap::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((key, value)) = trimmed.split_once(':') {
                headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        if headers
            .get("expect")
            .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
        {
            let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        }
        let mut body = String::new();
        if let Some(len) = headers
            .get("content-length")
            .and_then(|v| v.parse::<usize>().ok())
        {
            let mut buf = vec![0u8; len];
            if reader.read_exact(&mut buf).is_ok() {
                body = String::from_utf8_lossy(&buf).to_string();
            }
        }
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 2 {
            return;
        }
        requests.lock().unwrap().push(MockRequest {
            method: parts[0].to_string(),
            path: parts[1].to_string(),
            headers,
            body,
        });
        let resp = next_response(script);
        let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, resp.reason);
        let mut has_length = false;
        for (key, value) in &resp.headers {
            out.push_str(key);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
            if key.eq_ignore_ascii_case("content-length") {
                has_length = true;
            }
        }
        if !has_length {
            out.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
        }
        out.push_str("Connection: close\r\n\r\n");
        out.push_str(&resp.body);
        let _ = stream.write_all(out.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::test_mock::{MockResponse, MockScript, MockServer};
    use super::*;
    use serde_json::json;

    fn ok_json(body: &str) -> MockResponse {
        MockResponse {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: body.to_string(),
        }
    }

    fn csrf_response(token: &str) -> MockResponse {
        MockResponse {
            status: 200,
            reason: "OK",
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                (
                    "Set-Cookie".to_string(),
                    format!("{CSRF_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax"),
                ),
            ],
            body: "{}".to_string(),
        }
    }

    #[test]
    fn bootstrap_captures_csrf_cookie() {
        let server = MockServer::start(MockScript::new(vec![
            csrf_response("csrf-bootstrap-1"),
            ok_json("{\"ok\":true}"),
        ]));
        let http = ShellHttp::new();
        tauri::async_runtime::block_on(async {
            http.bootstrap(&server.url()).await.expect("bootstrap");
            let result = http
                .get(&server.url(), "/api/auth/status", None)
                .await
                .expect("get");
            assert!(result.is_success());
        });
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/auth/status");
        assert_eq!(requests[0].headers.get("authorization"), None);
    }

    #[test]
    fn state_changing_call_bootstraps_and_echoes_csrf() {
        let server = MockServer::start(MockScript::new(vec![
            csrf_response("csrf-abc123"),
            ok_json("{\"code\":\"ABCD2345\",\"expires_at\":\"2026-08-13T00:00:00Z\"}"),
        ]));
        let http = ShellHttp::new();
        tauri::async_runtime::block_on(async {
            let result = http
                .post(
                    &server.url(),
                    "/api/desktop/pair",
                    None,
                    Some(json!({"hostname": "dev-box"})),
                )
                .await
                .expect("post");
            assert!(result.is_success());
            assert_eq!(result.body["code"], "ABCD2345");
        });
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].path, "/api/auth/status");
        let pair = &requests[1];
        assert_eq!(pair.method, "POST");
        assert_eq!(pair.path, "/api/desktop/pair");
        assert_eq!(
            pair.headers.get("cookie").map(|s| s.as_str()),
            Some("csrf_token=csrf-abc123")
        );
        assert_eq!(
            pair.headers.get("x-csrf-token").map(|s| s.as_str()),
            Some("csrf-abc123")
        );
        assert_eq!(pair.headers.get("authorization"), None);
        assert!(pair.body.contains("dev-box"));
    }

    #[test]
    fn state_changing_call_reuses_bootstrapped_token() {
        let mut script = MockScript::new(vec![csrf_response("csrf-stable")]);
        script.repeat_last = true;
        let server = MockServer::start(script);
        let http = ShellHttp::new();
        tauri::async_runtime::block_on(async {
            http.bootstrap(&server.url()).await.expect("bootstrap");
            let result = http
                .delete(&server.url(), "/api/me/tokens/7", Some("tkn-secret"))
                .await
                .expect("delete");
            assert!(result.is_success());
        });
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].method, "DELETE");
        assert_eq!(
            requests[1].headers.get("cookie").map(|s| s.as_str()),
            Some("csrf_token=csrf-stable")
        );
        assert_eq!(
            requests[1].headers.get("x-csrf-token").map(|s| s.as_str()),
            Some("csrf-stable")
        );
        assert_eq!(
            requests[1].headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer tkn-secret")
        );
    }

    #[test]
    fn get_with_bearer_sends_authorization() {
        let server = MockServer::start(MockScript::new(vec![ok_json("{\"sessions\":[]}")]));
        let http = ShellHttp::new();
        tauri::async_runtime::block_on(async {
            let result = http
                .get(&server.url(), "/api/sessions", Some("tkn"))
                .await
                .expect("get");
            assert!(result.is_success());
        });
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer tkn")
        );
        assert_eq!(requests[0].headers.get("x-csrf-token"), None);
    }

    #[test]
    fn gone_response_surfaces_error_message() {
        let server = MockServer::start(MockScript::new(vec![MockResponse {
            status: 410,
            reason: "Gone",
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: "{\"error\":\"pairing code expired\"}".to_string(),
        }]));
        let http = ShellHttp::new();
        let result = tauri::async_runtime::block_on(async {
            http.get(
                &server.url(),
                "/api/desktop/pair/status?code=ABCD2345",
                None,
            )
            .await
            .expect("get")
        });
        assert_eq!(result.status, StatusCode::GONE);
        assert_eq!(
            result.server_error().as_deref(),
            Some("pairing code expired")
        );
    }

    #[test]
    fn bootstrap_retries_then_succeeds() {
        let server = MockServer::start(MockScript::new(vec![
            MockResponse {
                status: 500,
                reason: "Internal Server Error",
                headers: Vec::new(),
                body: "{\"error\":\"boom\"}".to_string(),
            },
            MockResponse {
                status: 500,
                reason: "Internal Server Error",
                headers: Vec::new(),
                body: "{\"error\":\"boom\"}".to_string(),
            },
            csrf_response("csrf-after-retries"),
        ]));
        let http = ShellHttp::new();
        tauri::async_runtime::block_on(async {
            http.bootstrap(&server.url()).await.expect("bootstrap");
        });
        assert_eq!(server.requests().len(), 3);
        assert_eq!(
            http.csrf_token(&server.url()).as_deref(),
            Some("csrf-after-retries")
        );
    }

    #[test]
    fn bootstrap_gives_up_after_max_attempts() {
        let mut script = MockScript::new(vec![MockResponse {
            status: 500,
            reason: "Internal Server Error",
            headers: Vec::new(),
            body: "{\"error\":\"boom\"}".to_string(),
        }]);
        script.repeat_last = true;
        let server = MockServer::start(script);
        let http = ShellHttp::new();
        let err = tauri::async_runtime::block_on(async {
            http.bootstrap(&server.url()).await.expect_err("must fail")
        });
        assert!(err.contains("CSRF bootstrap failed"));
        assert_eq!(server.requests().len(), BOOTSTRAP_ATTEMPTS as usize);
    }

    #[test]
    fn response_without_csrf_cookie_fails_bootstrap() {
        let mut script = MockScript::new(vec![ok_json("{}")]);
        script.repeat_last = true;
        let server = MockServer::start(script);
        let http = ShellHttp::new();
        let err = tauri::async_runtime::block_on(async {
            http.bootstrap(&server.url()).await.expect_err("must fail")
        });
        assert!(err.contains("server set no csrf cookie"));
    }

    // -------------------------------------------------------------------
    // Credential resolution (D3)
    // -------------------------------------------------------------------

    #[test]
    fn precedence_pins_pairing_first_and_scoped_fallback() {
        // Pairing credential present: it wins even when a scoped token
        // is stored (pairing stays the default identity).
        assert_eq!(
            credential_precedence(Some("dev-tkn"), Some("rgu-scoped".to_string())),
            Credential::Caller("dev-tkn".to_string())
        );
        assert_eq!(
            credential_precedence(Some("dev-tkn"), None),
            Credential::Caller("dev-tkn".to_string())
        );
        // No pairing credential: the scoped token fills in.
        assert_eq!(
            credential_precedence(None, Some("rgu-scoped".to_string())),
            Credential::Scoped("rgu-scoped".to_string())
        );
        // Neither: unauthenticated, exactly as before D3.
        assert_eq!(credential_precedence(None, None), Credential::Anonymous);
    }

    #[test]
    fn anonymous_endpoints_are_exempt_from_the_fill_in() {
        assert!(anonymous_path(BOOTSTRAP_PATH));
        assert!(anonymous_path(PAIR_PATH));
        assert!(anonymous_path("/api/desktop/pair/status?code=ABCD2345"));
        assert!(!anonymous_path("/api/sessions"));
        assert!(!anonymous_path("/api/me/tokens/7"));
    }

    #[test]
    fn missing_caller_credential_carries_the_stored_scoped_token() {
        let server = MockServer::start(MockScript::new(vec![ok_json("{\"sessions\":[]}")]));
        let http = ShellHttp::new();
        http.stub_scoped_token(Some("rgu-scoped-value".to_string()));
        tauri::async_runtime::block_on(async {
            let result = http
                .get(&server.url(), "/api/sessions", None)
                .await
                .expect("get");
            assert!(result.is_success());
        });
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer rgu-scoped-value")
        );
    }

    #[test]
    fn caller_credential_beats_the_stored_scoped_token_on_the_wire() {
        let server = MockServer::start(MockScript::new(vec![ok_json("{\"sessions\":[]}")]));
        let http = ShellHttp::new();
        http.stub_scoped_token(Some("rgu-scoped-value".to_string()));
        tauri::async_runtime::block_on(async {
            let result = http
                .get(&server.url(), "/api/sessions", Some("dev-tkn"))
                .await
                .expect("get");
            assert!(result.is_success());
        });
        let requests = server.requests();
        assert_eq!(
            requests[0].headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer dev-tkn"),
            "the paired credential must stay the default when present"
        );
    }

    #[test]
    fn no_stored_token_leaves_the_call_unauthenticated() {
        let server = MockServer::start(MockScript::new(vec![ok_json("{}")]));
        let http = ShellHttp::new();
        http.stub_scoped_token(None);
        tauri::async_runtime::block_on(async {
            let result = http
                .get(&server.url(), "/api/sessions", None)
                .await
                .expect("get");
            assert!(result.is_success());
        });
        let requests = server.requests();
        assert_eq!(
            requests[0].headers.get("authorization"),
            None,
            "without a stored record the call must look exactly as before D3"
        );
    }

    #[test]
    fn state_changing_call_carries_the_scoped_fill_in_with_csrf() {
        let server = MockServer::start(MockScript::new(vec![
            csrf_response("csrf-scoped-1"),
            ok_json("{\"ok\":true}"),
        ]));
        let http = ShellHttp::new();
        http.stub_scoped_token(Some("rgu-scoped-post".to_string()));
        tauri::async_runtime::block_on(async {
            let result = http
                .post(&server.url(), "/api/sessions", None, Some(json!({"id": 1})))
                .await
                .expect("post");
            assert!(result.is_success());
        });
        let requests = server.requests();
        let posted = &requests[1];
        assert_eq!(
            posted.headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer rgu-scoped-post")
        );
        assert_eq!(
            posted.headers.get("x-csrf-token").map(|s| s.as_str()),
            Some("csrf-scoped-1")
        );
    }

    #[test]
    fn unauthorized_scoped_call_is_answered_once_without_retry() {
        let server = MockServer::start(MockScript::new(vec![MockResponse {
            status: 401,
            reason: "Unauthorized",
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: "{\"error\":\"token expired\"}".to_string(),
        }]));
        let http = ShellHttp::new();
        http.stub_scoped_token(Some("rgu-revoked".to_string()));
        let result = tauri::async_runtime::block_on(async {
            http.get(&server.url(), "/api/sessions", None)
                .await
                .expect("get")
        });
        assert_eq!(result.status, StatusCode::UNAUTHORIZED);
        // Exactly one request went out: the rejection never retries.
        // The D2 routing cleared the stored record, so the next call
        // resolves to unauthenticated.
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn only_scoped_token_401s_route_into_invalidation() {
        assert!(routes_scoped_rejection(true, StatusCode::UNAUTHORIZED));
        // A 401 against the caller's own credential belongs to the
        // poller's signed-out flow, never to the token invalidation.
        assert!(!routes_scoped_rejection(false, StatusCode::UNAUTHORIZED));
        assert!(!routes_scoped_rejection(true, StatusCode::FORBIDDEN));
        assert!(!routes_scoped_rejection(
            true,
            StatusCode::INTERNAL_SERVER_ERROR
        ));
    }

    #[test]
    fn scoped_rejection_error_feeds_the_d2_classifier() {
        use crate::token_store::{classify_auth_error, AuthFailure};
        let raw = scoped_rejection_raw_error(
            StatusCode::UNAUTHORIZED,
            &json!({"error": "token expired"}),
        );
        assert_eq!(raw, "HTTP 401: token expired");
        assert_eq!(classify_auth_error(&raw), AuthFailure::TokenInvalidated);
        // A bodyless 401 still classifies through the status marker.
        let bare = scoped_rejection_raw_error(StatusCode::UNAUTHORIZED, &Value::Null);
        assert_eq!(bare, "HTTP 401");
        assert_eq!(classify_auth_error(&bare), AuthFailure::TokenInvalidated);
    }
}
