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
- `Dockerfile` / `audit-entrypoint.sh` / `audit.sh` — the dockerized local
  audit: the image, the in-container contract, and the host launcher.
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

## Local audit (Docker)

The whole local run in one command, with no host tooling beyond docker:

```bash
./tests/e2e/audit.sh
```

`audit.sh` builds the audit image on first use (rust, node, xvfb,
tauri-driver, tauri-cli, and the same apt deps as `e2e.yml`), then runs
`audit-entrypoint.sh` in a container that mounts this repo at `/workspace`.
The test persea server is built from the pinned e2e ref (the same ref the
CI workflow checks out), cloned fresh inside the container, so the audit
never depends on the state of a local server checkout. To build from a
local server checkout instead (server-development testing), pass
`--server-dir /path/to/persea`. The cargo target dir and the npm install
live in named volumes, so repeat runs reuse the build. Screenshots land in
`./audit-shots` under this repo, or wherever `PERSEA_E2E_SHOTS` points on
the host. Pass `--no-deb` to skip the deb bundle; that step is a full
release build and the slowest part of the run.

The entrypoint is the single contract and mirrors the CI job step by step:

1. `npm install` in `tests/e2e` (never writes a lockfile into the checkout)
2. clone the pinned persea ref (or use the mounted checkout) and provision
   the server via `provision-server.sh` on port 8099
3. `cargo build` the app in `src-tauri` unless a binary already exists at
   `PERSEA_E2E_APPS_DIR` (default `src-tauri/target/debug`)
4. when `AUDIT_DEB=1`, build the deb bundle and export `PERSEA_E2E_DEB`
   for the deb-smoke spec
5. run `xvfb-run -a node run-specs.js` with the provisioned env and print
   the screenshot dir

Required entrypoint env: `PERSEA_E2E_SHOTS`. Optional: `PERSEA_SERVER_REF`
(default `43215ab`, the pinned e2e ref), `PERSEA_SERVER_DIR` (mounted
checkout), `AUDIT_DEB` (defaults to 0) and `PERSEA_E2E_APPS_DIR`. The
entrypoint exits non-zero when the suite fails and tails the server log
to help diagnose.

The same container becomes the GH Actions job later: the job runs the
entrypoint as its run step (a `docker run` with the checkout mounted at
`/workspace`, or the image as the job container) and uploads the
`audit-shots` contents as an artifact. The runner needs no extra install
steps because the image carries the whole toolchain.

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

## Full-check specs (local infra)

The full-check specs (login, navigation, connection, ldap) verify real
behavior against a target server and need local infra:

- `login.spec.js`, `navigation.spec.js`: need `PERSEA_E2E_LOGIN_EMAIL` and
  `PERSEA_E2E_LOGIN_PASSWORD` (real credentials on the target server);
  they skip without them.
- `connection.spec.js`: needs an SSH target reachable from the server.
  Default target: a container on the docker bridge (`172.17.0.1`), user
  `sshuser` / `ssh-test-password-2026`, published on port 2222. Override
  with `PERSEA_E2E_SSH_HOST` / `_PORT` / `_USER` / `_PASSWORD`. The server
  must have guacd configured.
- `ldap.spec.js`: needs an enabled LDAP provider on the server (the
  running auth chain picks providers up at restart). The server repo
  provides the harness: `docker compose -f docker-compose.ldap.yml up -d
  --wait` (port 3389) plus the seed in `tests/fixtures/ldap-seed.ldif`
  (alice / alice-ldap-password-2026). Configure the provider through the
  admin auth page or `POST /api/auth/providers`, then restart the server.
  The spec skips with a named reason when no provider is enabled.

## Screenshots

`docs/screenshots/` holds the canonical desktop set (captured by
`run-specs.js` with `PERSEA_E2E_SHOTS`). Regeneration: the `e2e.yml`
workflow (workflow_dispatch) captures fresh shots and opens a PR when they
differ (requires the repo setting "Allow GitHub Actions to create and
approve pull requests"; without it the branch is pushed and the PR link
printed). The PR check warns, never fails, on drift.
