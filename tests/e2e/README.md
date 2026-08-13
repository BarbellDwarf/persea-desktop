# Persea Desktop E2E tests

The suite drives the REAL app through [tauri-driver](https://v2.tauri.app/develop/e2e/)
(WebDriver for Tauri) with a WebDriver client. Playwright cannot drive
tauri-driver: Playwright speaks CDP-style transports, not WebDriver.

## Layout

- `provision-server.sh` — builds the persea server (branch v1.2.0), creates
  an admin API key, starts it against a throwaway config, prints env values.
- `driver.js` / `run-specs.js` — WebDriver session helpers and the spec runner.
- `specs/shell.spec.js` — the shell's own pages (welcome, settings, pairing).
- `specs/inherited.spec.js` — the remote persea UI in the webview (login,
  connections, sessions, admin settings).
- `.github/workflows/e2e.yml` — the CI matrix + screenshot regeneration.

## Running locally

```bash
# 1. Build the app (needs the platform webview dev libraries):
cargo tauri build
# 2. Install tauri-driver:
cargo install tauri-driver
# 3. Provision the test server:
eval "$(tests/e2e/provision-server.sh /path/to/persea)"
# 4. Run the specs:
cd tests/e2e && npm install && PERSEA_E2E_BASE_URL="$PERSEA_E2E_BASE_URL" \
  PERSEA_E2E_API_KEY="$PERSEA_E2E_API_KEY" node run-specs.js
```

A display is required on Linux (use `xvfb-run` on headless boxes).

## Engine matrix

| Behavior | WebView2 | WebKitGTK | WKWebView |
|---|---|---|---|
| Shell pages (settings/pairing/welcome) | ✓ | ✓ | ✓ |
| Login + connections + sessions render | ✓ | ✓ | ✓ |
| Downloads → save dialog | CI (D05 adapter) | CI | CI |
| window.open → session window | CI | CI | CI |
| confirm() → native dialog | CI | CI | CI |
| File inputs → native picker | CI | CI | CI |
| Live RDP/SSH session + drive | CI with guacd | CI with guacd | CI with guacd |

Specs marked "CI" run only in the full matrix (they need guacd on 4822 or
engine-specific plumbing); locally they degrade to render checks. Known
skips are tracked here; a skip must name the reason, never delete the spec.

## Screenshots

`docs/screenshots/` holds the canonical desktop set (captured by
`run-specs.js` with `PERSEA_E2E_SHOTS`). Regeneration: the `e2e.yml`
workflow (workflow_dispatch) captures fresh shots and opens a PR when they
differ (requires the repo setting "Allow GitHub Actions to create and
approve pull requests"; without it the branch is pushed and the PR link
printed). The PR check warns, never fails, on drift.
