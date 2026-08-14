# Persea Desktop

Desktop shell for the Persea remote access server: a Tauri 2 client
whose webview logs into a persea instance and hosts the remote desktop
sessions. No server or guacd is embedded; you point the app at your
own persea instance (BYO server).

## What it does

- **Session tabs**: sessions open in the main window with a docked tab
  strip. Pop a session out into its own window, expand it to
  fullscreen on a monitor, or keep it inline.
- **Device pairing**: register this device with a server through a
  device-code flow. The paired token lives in your OS keychain and
  powers the tray, notifications and file transfers.
- **Tray**: one menu per server with its live sessions, pair status
  and quick actions. The icon shows when sessions are active and when
  a server rejected your token.
- **Notifications**: session started, ended, error and idle warnings.
  Off by default, one toggle in Settings.
- **Drag-drop transfers**: drag files onto an RDP session window to
  upload them to the session's drive; downloads land in a folder you
  control.
- **Global hotkeys**: summon the window (`Ctrl+Alt+P`) and cycle
  sessions (`Ctrl+Shift+Tab`), configurable in Settings.
- **Kiosk mode**: a locked-down fullscreen mode for thin-client
  terminals, enabled per server and exitable with a secret chord.
- **Enterprise provisioning**: installers can pre-configure servers,
  lock instances, pin kiosk mode and override settings.
- **Release and beta channels**: stable installers on the Releases
  page, a rolling beta pre-release for testers.

## Install

| OS | Guide |
|----|-------|
| Windows | [docs/install-windows.md](docs/install-windows.md) |
| macOS | [docs/install-macos.md](docs/install-macos.md) |
| Linux | [docs/install-linux.md](docs/install-linux.md) |

First launch on Windows shows a SmartScreen warning and on macOS a
Gatekeeper warning: the installers are not yet code-signed. The
guides explain the one-time bypass on each OS.

## First run

1. Launch the app and add your server (name + URL) on the welcome
   page.
2. Log in inside the app window with any method the server supports.
3. Pair this device from Settings → Device pairing.
4. Open a session: it appears as a tab, in the tray, and (when
   enabled) in notifications.

Full walkthrough: [docs/getting-started.md](docs/getting-started.md).

## Repo layout

| Path | Purpose |
|------|---------|
| `src-tauri/` | Rust app: Tauri shell, config, capabilities, icons |
| `shell/` | Local HTML/JS pages (no bundler, plain files) |
| `docs/` | Documentation ([index](docs/README.md)) |
| `tests/e2e/` | WebDriver end-to-end test suite |
| `scripts/` | Dev and smoke-test helpers |
| `.github/workflows/` | CI, E2E, release and beta workflows |

## Platform notes

Per-OS behavior differs enough to warrant dedicated pages:

- **macOS** ([docs/macos.md](docs/macos.md)): the app ships ad-hoc
  signed. First launch shows the Gatekeeper "unidentified developer"
  warning; right-click → Open (or Privacy & Security → Open Anyway)
  bypasses it, and every update re-prompts until notarization lands.
  macOS 15 is stricter about the bypass paths. Tested on macOS 14/15,
  arm64 + x86_64.
- **Linux** ([docs/linux-troubleshooting.md](docs/linux-troubleshooting.md)):
  WebKitGTK 4.1 quirks. The deb declares the GStreamer codec + VA-API
  stack explicitly; the rpm declares `webkit2gtk4.1` (RHEL 10 needs
  EPEL 10 first). NVIDIA blank windows are fixed with
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` /
  `WEBKIT_DISABLE_COMPOSITING_MODE=1`.
- **Wayland** ([docs/wayland.md](docs/wayland.md)): global hotkeys are
  unavailable (X11-only plugin), the Win/Super key capture is
  best-effort, the tray needs a tray host (KDE native, GNOME needs the
  AppIndicator extension), kiosk and tab-strip docking are
  best-effort. X11 has no such limits.
- **Windows**: the installer bootstraps the WebView2 Evergreen runtime
  when it is missing (download bootstrapper, silent). The installer is
  unsigned, so SmartScreen shows "Windows protected your PC"; click
  **More info → Run anyway** (an EV code-signing cert is planned).
  Windows reserves some hotkey chords (for example Win+L,
  Ctrl+Alt+Del): registering one shows a conflict in Settings →
  Shortcuts and the chord stays inactive; pick a free chord.

## Documentation

User and administrator documentation lives in `docs/`, indexed from
[docs/README.md](docs/README.md): getting started, per-OS install
guides, transfers, hotkeys, kiosk mode, keychain, Wayland notes,
enterprise provisioning, release and beta channels.

## Development

Developers: see [docs/development.md](docs/development.md) for setup
per OS, running the app (`cargo tauri dev`), testing, the E2E suite
and CI.

## Support

Persea Desktop is part of the persea project, funded by its community.
If the app saves you time, consider supporting the project on
[Ko-Fi](https://ko-fi.com/barbelldwarf): contributions pay for CI
infrastructure, cross-platform build and signing certificates, test
machines, and development time.

## License

Apache-2.0, see [LICENSE](LICENSE) and
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
