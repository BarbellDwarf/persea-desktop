# Development

This page is for people who build, test or contribute to Persea
Desktop itself. Everything else in this folder is written for users
and administrators; this one is written for developers.

## Layout

| Path | What it is |
|------|------------|
| `src-tauri/` | The Rust app: Tauri 2 shell, backend modules, capabilities, icons |
| `shell/` | The app's own HTML/JS pages (welcome, settings, pairing, transfer, tab strip). Plain files, no bundler, no build step |
| `tests/e2e/` | The WebDriver end-to-end suite (see below) |
| `scripts/` | Dev and smoke-test helpers |
| `.github/workflows/` | CI, E2E, release and beta workflows |

The Rust modules under `src-tauri/src/`:

| Module | Responsibility |
|--------|----------------|
| `instances.rs` | The server list (name, URL, capabilities probe), persisted in `instances.json` |
| `pairing.rs` | Device-code pairing flow, token registry |
| `keyring.rs` | OS keychain access with Linux fallback tiers |
| `windows.rs` | Session tab manager: tab strip window, session windows, pop-out, expand |
| `bridge.rs` | Shell-to-page event bridge with the server's desktop bridge partial |
| `navigation.rs` | Navigation lockdown allowlist |
| `tray.rs` | Tray icon and menu |
| `poller.rs` | Session poller / SSE feed behind the tray and notifications |
| `notify.rs` | Desktop notifications |
| `transfer.rs`, `drop.rs`, `downloads.rs` | File transfers, drag-drop capture, download interception |
| `kiosk.rs` | Kiosk mode |
| `hotkeys.rs` | Global shortcuts |
| `hooks/` | Win/Super key capture per platform (X11, Wayland, Windows, macOS) |
| `provisioning.rs` | Enterprise provisioning merge |
| `shell_config.rs` | Shell appearance settings |
| `platform.rs` | Per-OS menu bar |
| `http.rs` | Shell HTTP client with the CSRF contract |

## Prerequisites

Rust is pinned by `rust-toolchain.toml` (currently 1.88); with rustup
installed the right toolchain installs itself on first build. The
Debian 13 native toolchain is too old for the keyring store crates
(zbus needs Rust 1.87+), so use rustup on Debian too.

Install the Tauri CLI once:

```sh
cargo install tauri-cli --version 2.11.4 --locked
```

### Debian / Ubuntu

```sh
sudo apt update
sudo apt install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev libdbus-1-dev
```

### RHEL / Fedora

RHEL 10 removed WebKitGTK from the base repos: enable EPEL 10 first,
then install `webkit2gtk4.1-devel`. Fedora ships it in the default
repos.

```sh
sudo dnf install -y webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel librsvg2-devel libXdo-devel openssl-devel
```

### Windows

WebView2 (Evergreen runtime) is preinstalled on Windows 11 and broadly
deployed on Windows 10. Install the Rust MSVC toolchain via rustup and
the Microsoft C++ Build Tools. `cargo tauri dev` needs no extra system
setup on Windows.

### macOS

Install the Xcode Command Line Tools (`xcode-select --install`); the
WKWebView engine ships with macOS.

## Run the app

The `shell/` frontend is plain HTML with no build step, and
`tauri.conf.json` has no `devUrl`, so `tauri dev` serves the shell
assets itself: nothing to start beforehand.

```sh
cargo tauri dev
```

`scripts/dev.sh` is a thin wrapper that validates `PERSEA_URL` and
runs `cargo tauri dev` (falling back to plain `cargo run` when the
Tauri CLI is missing). Point it at a local persea server:

```sh
PERSEA_URL=http://127.0.0.1:8089 ./scripts/dev.sh
```

The app manages servers at runtime (welcome page, Settings →
Instances), so for a quick start you can also just launch and add the
server in the UI.

Dev builds allow `http://` localhost navigation targets; release
builds restrict instances to `https://` (except localhost). The
navigation lockdown is active in dev builds too: the webview only
navigates to configured instance origins, the shell's own pages, and
the identity providers of the configured `auth.extra_allowed_hosts`.

## Test

```sh
cd src-tauri

cargo test          # unit + module tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
cargo audit         # dependency vulnerability scan
```

`scripts/smoke.sh` builds the binary and checks that the window
process stays alive. It needs an X/Wayland display; use `xvfb-run`
headless.

## End-to-end tests

The suite in `tests/e2e/` drives the real app binary through
tauri-driver (WebDriver for Tauri). Playwright cannot drive
tauri-driver: Playwright speaks CDP-style transports, not WebDriver.
Read `tests/e2e/README.md` for the full details; the short version:

```sh
# 1. Build the app (needs the platform webview dev libraries):
cargo tauri build
# 2. Install tauri-driver:
cargo install tauri-driver
# 3. Provision a test persea server (checks out and builds the server):
eval "$(tests/e2e/provision-server.sh /path/to/persea)"
# 4. Run the specs:
cd tests/e2e && npm install && PERSEA_E2E_BASE_URL="$PERSEA_E2E_BASE_URL" \
  PERSEA_E2E_API_KEY="$PERSEA_E2E_API_KEY" node run-specs.js
```

A display is required on Linux (use `xvfb-run` on headless boxes).
Specs that need guacd (live RDP/SSH sessions) run in the full CI
matrix and degrade to render checks locally.

The canonical desktop screenshots under `docs/screenshots/` are
captured by the E2E suite with `PERSEA_E2E_SHOTS` set. The e2e
workflow's manual run regenerates them and opens a PR when they
drift.

## CI

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| `ci.yml` | every push/PR to main | check, fmt, clippy, tests on Windows, Linux, macOS; `cargo audit` |
| `e2e.yml` | PRs touching `src-tauri/`, `shell/`, `tests/e2e/`; manual dispatch | builds the app, provisions a test server, runs the WebDriver specs on all three OSes. Manual runs also regenerate the screenshots |
| `codeql.yml` | PRs and schedule | CodeQL static analysis |
| `release.yml` | `v*` tag | builds and publishes the release artifact matrix (see [release.md](release.md)) |
| `beta.yml` | manual dispatch | builds and publishes the beta pre-release (see [beta.md](beta.md)) |

The release and beta workflows run the shared CI checks as gates
before publishing anything: a failing check aborts the run before any
artifact ships.

## Architecture notes

- **Navigation lockdown.** The webview is allowlisted to the
  configured instances plus their identity providers. Anything else is
  blocked: http(s) links go to the system browser, everything else is
  dropped. Blocked navigations are logged with the host only, never
  the full URL, so log lines stay clean of query strings and tokens.
  If an OIDC login stalls, look for a log line like
  `[persea-desktop] navigation lockdown: blocked host login.corp.example.com`
  and add that host to `auth.extra_allowed_hosts` (bare hostname;
  matching is exact, `idp.example.com` does not cover
  `login.idp.example.com`).
- **Remote IPC.** Remote persea pages get Tauri IPC only through the
  `remote.json` capability, which grants `core:event:default` to the
  configured instance origins and nothing else. Shell-only commands
  (keyring, pairing, tabs, transfers) are declared in the app manifest
  and granted only to the shell's own pages in `default.json`, so a
  server page can never invoke them.
- **Identity.** The main window is code-built in `lib.rs` (the
  navigation handlers are build-time-only in Tauri 2.11) and carries
  the same per-instance webview data store as session windows, so a
  popped-out session keeps the login cookie.
- **Data files.** All state lives in the app config/data directory:
  `instances.json` (servers + cached capability probes),
  `pairing.json` (token registry metadata; secrets live in the
  keychain), `hotkeys.json`, `windows.json` (tab preferences),
  `shell.json` (appearance), `notifications.json`, and on Linux the
  fallback `keyring.db` / `keyring-key` when no Secret Service daemon
  exists.

## Releasing

How the release pipeline works and how to cut a release:
[release.md](release.md). The beta channel: [beta.md](beta.md). The
enterprise provisioning contract (schema, delivery paths, trust
rules): [provisioning.md](provisioning.md).

- pages.spec.js (shell page-render checks)
