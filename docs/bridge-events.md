# Desktop bridge events

The shell and the persea page talk over Tauri's event bus. This document is
the contract; the server side mirrors it in the desktop bridge partial
(`templates/partials/desktop_bridge.html` on the persea server, live on the
main branch).

## Transport and scoping

- Tauri 2 gates remote-origin IPC by **capability only**: the shell ships
  `src-tauri/capabilities/remote.json`, which grants `core:event:default`
  (listen, unlisten, emit, emitTo) to the origins in its `remote.urls` list.
  The v1 `dangerousRemoteDomainIpcAccess` config key does not exist in the v2
  schema (`SecurityConfig` in tauri-utils 2.9.3 is `deny_unknown_fields`), so
  the capability file is the whole mechanism. There is no `all: true`; an
  origin that is not listed gets zero IPC, and every call from it is rejected
  by the ACL before it reaches any command.
- Origins are build-time data (capabilities compile into the binary). The
  instance provisioning work writes the configured instance origins into
  `src-tauri/capabilities/remote.json` before building. At startup,
  `bridge::register` validates every configured instance origin against that
  baked allowlist and fails closed: unlisted origins get no bridge features
  (warning logged, origin excluded).
- `app.withGlobalTauri: true` in `tauri.conf.json` injects
  `window.__TAURI__` (the tauri crate's own `scripts/bundle.global.js`) on
  every page load, including remote persea pages. That is what the server's
  desktop bridge partial guards on; the global API itself is inert where the
  ACL denies.
- The page must allow the IPC transports in CSP; the server handles that via
  the `[desktop] allow_bridge` flag, which adds `tauri://localhost` and
  `http://ipc.localhost` to `connect-src`.
- Shell-to-page delivery uses Tauri's internal emit, which is a no-op when
  the page has no listener for the event (no error, no console spam).
- The shell's init script (`bridge::init_script()`) installs the page-side
  plumbing at document start, only when `window.__TAURI__` exists. It must be
  applied at webview creation via
  `WebviewWindowBuilder::initialization_script(bridge::init_script())`; Tauri
  2.11 has no runtime API to add init scripts to an existing webview, and
  `webview.eval` is blocked by the page's nonce CSP. The main window is
  created before the setup hook runs, so the window plumbing owns the
  handoff.

## Capability diff review checklist (acceptance)

- [ ] `remote.json` grants **only** `core:event:default`. No window, tray,
      fs, shell, opener, or clipboard permissions.
- [ ] `remote.json` targets only the windows that host persea pages
      (`main` today).
- [ ] `default.json` (local pages) has no `remote` section.
- [ ] `remote.urls` contains exactly the configured instance origins
      (`scheme://host[:port]`); a leading `*.` subdomain wildcard is
      allowed, everything else fails closed.
- [ ] No other capability file gains a `remote` section.

## Event schema

Event names allow only alphanumerics, `-`, `/`, `:` and `_` (Tauri
`EventName`). Payloads are JSON.

### Shell to page

| Event | Payload | Consumer |
|-------|---------|----------|
| `key-inject` | `{ "keysym": number, "down": boolean }` | Session client (Win-key injection) |
| `file-drop` | `{ "paths": string[] }` | Session client drive upload |
| `session-command` | `{ "cmd": "fullscreen" \| "close" \| "navigate", "arg"?: string }` | Session client (fullscreen toggle, close, navigate to `arg`) |
| `desktop-mode` | `{ "on": boolean }` | All pages: with `on: true` the page hides its own tab bar (the shell's tab strip replaces it) |

The server's desktop bridge partial binds these four names and dispatches
them through `window.perseaDesktop.on(name, handler)`.

### Page to shell

| Event | Payload | Shell consumer |
|-------|---------|----------------|
| `session-ready` | `{ "session_id": string }` | Session lifecycle: the page has an active session |
| `drive-browser-open` | (none) | Drive browser |
| `session-ended` | `{ "session_id": string }` | Session lifecycle: clean up the session window |

The shell receives these via `bridge::register`, which buffers them;
`bridge::drain_page_events()` hands them to the dispatcher. Page code emits
them through `window.perseaShell.emit(name, payload)` (installed by the shell
init script); the call is a silent no-op when `window.__TAURI__` is absent
(plain browser).

## Rust API (src-tauri/src/bridge.rs)

- `pub fn register(app: &mut tauri::App, instance_origins: Vec<String>) -> Vec<String>`:
  validates origins against the baked allowlist (fail closed), installs the
  page-to-shell listeners, stores the app handle. Returns the allowed
  origins.
- `pub fn init_script() -> &'static str`: the document-start page plumbing.
- `pub fn desktop_bridge_available() -> bool` / `pub fn set_bridge_available(bool)`:
  gated on the server's `desktop_bridge` capability probe, mirroring
  the server's `init_allow_bridge` pattern. False until set.
- `pub fn allowed_origins() -> &'static [String]`: the validated origins.
- Emit helpers (all target the `main` window's webview, all return
  `Result<(), BridgeError>` and fail closed when the bridge is unavailable):
  `emit_key_inject(keysym: u32, down: bool)`,
  `emit_file_drop(paths: Vec<String>)`,
  `emit_session_command(cmd: SessionCommandKind, arg: Option<String>)`,
  `emit_desktop_mode(on: bool)`.
- `pub fn drain_page_events() -> Vec<PageEvent>`: buffered page-to-shell
  events (`PageEvent { name, payload }`).

## Failure modes (all silent or logged, never a crash)

- Origin not allowlisted: ACL rejects the call ("not allowed from remote
  context"); the page's try/catch swallows it; the shell logs one warning at
  startup.
- Page not listening: emit is a no-op.
- `window.__TAURI__` absent (plain browser): the bridge partial and the init
  script both stay inert; page behavior is byte-identical to today.
- Server without `allow_bridge` (flag off): the page CSP blocks the IPC
  transport; `window.perseaDesktop` never binds; features degrade to the
  non-bridge path.
