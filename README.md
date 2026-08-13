# Persea Desktop

Desktop shell for the Persea remote access server: a thin Tauri 2 client
whose webview logs into a persea instance and hosts the remote desktop
sessions. No server or guacd is embedded, you point the app at your own
persea instance (BYO server).

Current state: D01 scaffold with the D03 navigation lockdown in place.
The window opens and shows a placeholder shell page; instance
management, pairing, tray and the other desktop features land in later
Planned and tracked locally; implementation notes live in the repository history.

## Repo layout

| Path | Purpose |
|------|---------|
| `src-tauri/` | Rust app: Tauri shell, config, capabilities, icons |
| `shell/` | Local HTML/JS pages (no bundler, plain files) |
| `docs/` | Documentation (full docs land with D17) |
| `scripts/` | Dev and smoke-test helpers |
| `.github/workflows/` | CI: 3-OS check/fmt/clippy/test, cargo audit, CodeQL |

## Prerequisites

Rust 1.88 is pinned by `rust-toolchain.toml` (raised from 1.85 on
2026-08-13: the zbus-based keyring stores need Rust ≥ 1.87, so the
Debian-13 native toolchain is no longer sufficient — use rustup on
Debian too). With rustup installed the right toolchain installs itself
on first build. Tauri requires the system webview development libraries:

### Debian / Ubuntu

```sh
sudo apt update
sudo apt install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev libdbus-1-dev
```

### RHEL / Fedora (EPEL)

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

## Platform notes

Per-OS behavior differs enough to warrant dedicated pages:

- **macOS** (`docs/macos.md`): the app ships ad-hoc signed. First
  launch shows the Gatekeeper "unidentified developer" warning;
  right-click → Open (or Privacy & Security → Open Anyway) bypasses
  it, and **every update re-prompts** until notarization lands. macOS 15
  is stricter about the bypass paths. Tested on macOS 14/15, arm64 +
  x86_64.
- **Linux** (`docs/linux-troubleshooting.md`): WebKitGTK 4.1 quirks.
  The deb declares the GStreamer codec + VA-API stack explicitly
  (`gstreamer1.0-libav`, `gstreamer1.0-plugins-bad`,
  `gstreamer1.0-vaapi`, `mesa-va-drivers`); the rpm declares
  `webkit2gtk4.1` (RHEL 10 needs EPEL 10 first). NVIDIA blank windows
  are fixed with `WEBKIT_DISABLE_DMABUF_RENDERER=1` /
  `WEBKIT_DISABLE_COMPOSITING_MODE=1`.
- **Wayland** (`docs/wayland.md`): global hotkeys are unavailable
  (X11-only plugin), the Win/Super key capture is best-effort, the tray
  needs a tray host (KDE native, GNOME needs the AppIndicator
  extension), kiosk and tab-strip docking are best-effort. X11 has no
  such limits.
- **Windows**: the installer bootstraps the WebView2 Evergreen runtime
  when it is missing (download bootstrapper, silent). The installer is
  unsigned, so SmartScreen shows "Windows protected your PC"; click
  **More info → Run anyway** (an EV code-signing cert is the future
  fix). Windows reserves some hotkey chords (for example Win+L,
  Ctrl+Alt+Del): registering one shows a conflict in Settings →
  Shortcuts and the chord stays inactive; pick a free chord.

## Development

The `shell/` frontend is plain HTML with no build step. With no
`devUrl` configured, `tauri dev` serves `frontendDist` with its built-in
dev server, so there is nothing to start beforehand.

```sh
# one-time: the Tauri CLI
cargo install tauri-cli --version 2.11.4 --locked

cargo tauri dev
```

The app opens a window titled "Persea Desktop". Point it at a local
persea server with the dev script (stub until D02):

```sh
PERSEA_URL=http://127.0.0.1:8089 ./scripts/dev.sh
```

## Navigation allowlist

The webview is locked down to the configured instance and the identity
providers its login redirects to. Any navigation off those origins is
blocked: http(s) links are handed to your system browser instead,
everything else is dropped. Blocked navigations are logged with the host
only, never the full URL, so log lines stay clean of query strings and
tokens.

The allowlist has two config inputs (both fed by the instance and shell
config, see D02):

- **Instance origins**: the scheme, host and port of each persea
  instance the app connects to. A bare host is treated as `https://`.
- **`auth.extra_allowed_hosts`**: bare hostnames of OIDC/SAML identity
  providers, used for the login redirect chain. Defaults to empty.

An instance entry that does not parse as an http(s) URL is skipped with
a startup warning.

If an OIDC login stalls, look for a log line like

```
[persea-desktop] navigation lockdown: blocked host login.corp.example.com
```

and add that host to `auth.extra_allowed_hosts`. Any scheme and port on
the host is allowed, so list the bare hostname only. Matching is exact:
`idp.example.com` does not cover `login.idp.example.com`. The bundled
shell's own origin (`tauri://localhost`) is always allowed, and http(s)
localhost targets are allowed in dev builds.

`window.open` from a remote page is only honored for URLs inside an
instance origin (multi-window support is D05); everything else is
rejected, with http(s) URLs handed to the system browser.

## Testing

```sh
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml --all --check
cargo audit --manifest-path src-tauri/Cargo.toml
```

`scripts/smoke.sh` builds the binary and checks that the window process
stays alive; it needs an X/Wayland display (use `xvfb-run` headless).
The tauri-driver based test harness is D16.

## Identity notes

- Binary name: `persea-desktop`, display name "Persea Desktop", version
  1.2.0, identifier `dev.persea.desktop` (chosen over
  `com.persea.desktop`; revisit before the first release, D14).
- Icons under `src-tauri/icons/` are generated placeholders
  (`scripts/gen-placeholder-icons.py`); the real artwork is D18.

## Support

Persea Desktop is part of the persea project, funded by its community. If the app saves you time, consider sponsoring the project on [Open Collective](https://opencollective.com/persea): contributions pay for CI infrastructure, cross-platform build and signing certificates, test machines, and development time.

## License

Apache-2.0, see [LICENSE](LICENSE) and
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
