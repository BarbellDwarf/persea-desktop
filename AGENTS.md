# AGENTS.md: Project state for persea-desktop

## What this project is

Persea Desktop is the Tauri 2 desktop client for persea (the server repo: persea-grove/persea). It wraps the persea web UI in a native shell: multi-server instances, tabs, pop-out windows, system tray, drag-drop file transfers, device pairing, keychain, hotkeys, kiosk mode, provisioning, and auto-update.

## Key files

- `src-tauri/`: the Tauri app (Rust)
  - `src/main.rs` + `src/lib.rs`: entry, dispatcher wiring
  - `src/instances.rs`: server instance config, probe, sectioned settings
  - `src/windows.rs`: multi-window manager (main window, session tabs, tab strip)
  - `src/tray.rs`: system tray, state-dot icons (`icons/tray.png`, `tray-active.png`, `tray-signedout.png`)
  - `src/transfer.rs`: drag-drop transfers, hidden transfer window
  - `src/drop.rs`: drop-zone overlay window (hidden, transparent; ignore-cursor-events applied after show, see dropzone caveat)
  - `src/kiosk.rs`, `src/hotkeys.rs`, `src/pairing.rs`, `src/bridge.rs`, `src/keychain.rs`, `src/provision.rs`
  - `tauri.conf.json`: bundle config, updater endpoints, icons
  - `capabilities/`: capability scopes (shell windows only; remote origins get none)
- `docs/`: user-facing docs (getting started, per-OS installs, transfers, kiosk, provisioning, beta channel)
- `.github/workflows/`: CI (gates), beta.yml, release.yml (tag-triggered)

## Known pitfalls (do not reintroduce)

- **Dropzone overlay**: `set_ignore_cursor_events(true)` must only run after the window is shown. Called at setup on the hidden, unrealized window it aborts the app on Linux (tao#1178). The call lives in `position_overlay` after `.show()`.
- **`setup-rust-toolchain` input is `target` (singular)**: passing `targets:` is silently ignored and cross-targets never install. Used in release.yml and beta.yml build jobs.
- **`gh` needs a git context**: release/beta publish jobs must `actions/checkout` before any `gh release` command.
- **Tray on Wayland**: tray and `set_ignore_cursor_events` are best-effort; drop targets resolve via LAST_TARGET fallback.

## Rules for agents

### Git discipline

- **Never run `git reset`, `git checkout .`, or `git stash`**: these destroy parallel work. If the tree has unexpected changes, leave them alone and work around them.
- **Never leave uncommitted work**: commit or `WIP:` before stopping.
- **Commit messages**: Conventional Commits (`fix:`, `feat:`, `docs:`, `style:`, `test:`). Reference GitHub issues so they link: `fix: ... (persea-desktop#N)`, or `Closes #N` when the PR resolves it.
- **Push after commit**: the branch is shared.
- **Never issue two edits to the same file in one parallel tool batch**: same-file edits race and silently clobber each other.

### Verification

- **`cargo check` is NOT enough**: run `cargo test` and `cargo fmt --check` too (from `src-tauri/`).
- **CI must be green** (`gh run list`) before moving on.
- **Tests are guardrails**: if your change breaks a test, your change is wrong. Fix your change, not the test (unless the test is stale, then note it and ask).

### Security

- **Never log secrets**: API keys, passwords, pairing tokens stay out of logs.
- **Fail closed**: remote origins get ZERO capabilities (`capabilities/remote.json`); the desktop bridge refuses origins not allowlisted.
- **Keychain**: use the keyring abstraction (`src/keychain.rs`); never store secrets in plain files or the webview.

## Issue tracking

**GitHub issues are the source of truth.** Create tickets on GitHub, assign them to a project, and work them through to close. No local ticket files.

- **Projects** (org `persea-grove`):
  - `persea-desktop bugs` — desktop bugs
  - `persea bugs` — server bugs
  - `persea features` — features for both repos
- **New bugs** → `gh issue create --repo persea-grove/persea-desktop --label bug`, then `gh project item-add` to `persea-desktop bugs`.
- **New features** → `gh issue create` with an `enhancement` label, then `gh project item-add` to `persea features`.
- **Status lives in the issue**: opened → assigned to project → worked → closed (with the fix PR linked).
- Check `gh issue list --repo persea-grove/persea-desktop --state open` before starting work.
