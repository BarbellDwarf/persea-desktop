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

/// Shared shell HTTP client with per-instance CSRF state.
#[derive(Debug)]
pub struct ShellHttp {
    client: reqwest::Client,
    csrf: Mutex<HashMap<String, Option<String>>>,
}

impl ShellHttp {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("persea-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client with static options cannot fail");
        Self {
            client,
            csrf: Mutex::new(HashMap::new()),
        }
    }

    fn base_url(&self, instance_url: &str) -> String {
        instance_url.trim_end_matches('/').to_string()
    }

    /// GET request. GETs are CSRF-exempt; `bearer` is attached when the
    /// caller has a paired token.
    pub async fn get(
        &self,
        instance_url: &str,
        path: &str,
        bearer: Option<&str>,
    ) -> Result<HttpResult, String> {
        let url = format!("{}{}", self.base_url(instance_url), path);
        let mut req = self.client.get(&url);
        if let Some(token) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        self.dispatch(req, instance_url).await
    }

    /// Generic state-changing call with the CSRF double-submit contract:
    /// the `csrf_token` cookie and the `X-CSRF-Token` header are echoed
    /// on every POST/PUT/DELETE/PATCH, plus the optional Bearer header.
    pub async fn send(
        &self,
        method: Method,
        instance_url: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> Result<HttpResult, String> {
        let token = self.ensure_csrf(instance_url).await?;
        let url = format!("{}{}", self.base_url(instance_url), path);
        let mut req = self.client.request(method, &url);
        if let Some(token) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(tok) = token {
            req = req
                .header(header::COOKIE, format!("{CSRF_COOKIE}={tok}"))
                .header(CSRF_HEADER, tok);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        self.dispatch(req, instance_url).await
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
        self.send(Method::PUT, instance_url, path, bearer, body).await
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
    ) -> Result<HttpResult, String> {
        let resp = req.send().await.map_err(|e| format!("request failed: {e}"))?;
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
        stream: TcpStream,
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
        let server = MockServer::start(MockScript::new(vec![csrf_response("csrf-stable")]));
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
        let server = MockServer::start(MockScript::new(vec![ok_json(
            "{\"sessions\":[]}",
        )]));
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
            http.get(&server.url(), "/api/desktop/pair/status?code=ABCD2345", None)
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
        let server = MockServer::start(MockScript {
            responses: vec![MockResponse {
                status: 500,
                reason: "Internal Server Error",
                headers: Vec::new(),
                body: "{\"error\":\"boom\"}".to_string(),
            }],
            repeat_last: true,
            last: None,
        });
        let http = ShellHttp::new();
        let err = tauri::async_runtime::block_on(async {
            http.bootstrap(&server.url()).await.expect_err("must fail")
        });
        assert!(err.contains("CSRF bootstrap failed"));
        assert_eq!(server.requests().len(), BOOTSTRAP_ATTEMPTS as usize);
    }

    #[test]
    fn response_without_csrf_cookie_fails_bootstrap() {
        let server = MockServer::start(MockScript {
            responses: vec![ok_json("{}")],
            repeat_last: true,
            last: None,
        });
        let http = ShellHttp::new();
        let err = tauri::async_runtime::block_on(async {
            http.bootstrap(&server.url()).await.expect_err("must fail")
        });
        assert!(err.contains("server set no csrf cookie"));
    }
}
