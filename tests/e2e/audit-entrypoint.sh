#!/usr/bin/env bash
# Run the full desktop UI audit inside the audit container. This is the
# single contract for both the local docker launcher (audit.sh) and,
# later, the equivalent GH Actions job, so it stays self-contained: no
# host-specific paths, no interactive prompts.
#
# The test server is built from the pinned persea ref (the same ref the
# CI e2e workflow checks out), cloned fresh inside the container, so the
# audit never depends on the state of a local server checkout. A
# mounted checkout is used only when PERSEA_SERVER_DIR is set.
#
# Env:
#   PERSEA_SERVER_REF    persea ref to clone and build (default 43215ab,
#                        matching e2e.yml)
#   PERSEA_SERVER_DIR    mounted persea repo path; when set, used
#                        instead of the pinned-ref clone
#   PERSEA_E2E_SHOTS     screenshot output dir (required)
#   AUDIT_DEB            1 to build the deb bundle and export
#                        PERSEA_E2E_DEB for the deb-smoke spec; 0 or
#                        unset skips the deb build
#   PERSEA_E2E_APPS_DIR  optional override for the built app; defaults to
#                        the debug build dir
set -euo pipefail

WORKSPACE=/workspace
E2E_DIR="$WORKSPACE/tests/e2e"
APP_DIR="${PERSEA_E2E_APPS_DIR:-$WORKSPACE/src-tauri/target/debug}"
APP_BIN="$APP_DIR/persea-desktop"

: "${PERSEA_E2E_SHOTS:?PERSEA_E2E_SHOTS is required (screenshot output dir)}"
mkdir -p "$PERSEA_E2E_SHOTS"

echo "[audit] installing node deps..."
(
  cd "$E2E_DIR"
  # --no-package-lock: never write a lockfile into the mounted checkout.
  npm install --no-package-lock
)

if [ -n "${PERSEA_SERVER_DIR:-}" ]; then
  SERVER_DIR="$PERSEA_SERVER_DIR"
  echo "[audit] using the mounted persea checkout at $SERVER_DIR"
else
  REF="${PERSEA_SERVER_REF:-43215ab}"
  SERVER_DIR="$(mktemp -d /tmp/persea-server.XXXXXX)"
  echo "[audit] cloning persea at $REF (the pinned e2e ref)..."
  git clone --no-checkout https://github.com/persea-grove/persea.git "$SERVER_DIR"
  git -C "$SERVER_DIR" checkout "$REF"
fi

echo "[audit] provisioning the persea server on 8099..."
PROVISION_OUT="$(bash "$E2E_DIR/provision-server.sh" "$SERVER_DIR" 8099)"
# Export each PERSEA_E2E_* line separately: a single quoted export would
# fold the whole multi-line output into one value.
while IFS= read -r line; do
  export "$line"
done <<< "$(printf '%s\n' "$PROVISION_OUT" | grep '^PERSEA_E2E')"
# The server stays up for the whole run; the container exits afterwards.
trap 'kill "${PERSEA_E2E_PID:-}" 2>/dev/null || true' EXIT
echo "[audit] server ready at $PERSEA_E2E_BASE_URL"

if [ -x "$APP_BIN" ]; then
  echo "[audit] reusing the prebuilt app at $APP_BIN"
else
  echo "[audit] building the app (debug)..."
  cargo build --manifest-path "$WORKSPACE/src-tauri/Cargo.toml"
fi
export PERSEA_E2E_APPS_DIR="$APP_DIR"

if [ "${AUDIT_DEB:-0}" = "1" ]; then
  shopt -s nullglob
  DEBS=("$WORKSPACE/src-tauri/target/release/bundle/deb"/*.deb)
  if [ "${#DEBS[@]}" -gt 0 ] && [ ! "$APP_BIN" -nt "${DEBS[0]}" ]; then
    echo "[audit] reusing the existing deb bundle: ${DEBS[0]}"
    export PERSEA_E2E_DEB="${DEBS[0]}"
  else
    echo "[audit] building the deb bundle (release)..."
    (
      cd "$WORKSPACE/src-tauri"
      # CI mode avoids interactive prompts; updater artifacts are skipped
      # because the signing key only exists in the release secrets.
      CI=true cargo tauri build --bundles deb \
        --config '{"bundle":{"createUpdaterArtifacts":false}}'
    )
    DEBS=("$WORKSPACE/src-tauri/target/release/bundle/deb"/*.deb)
    [ "${#DEBS[@]}" -gt 0 ] || { echo "[audit] deb build produced no .deb" >&2; exit 1; }
    export PERSEA_E2E_DEB="${DEBS[0]}"
    echo "[audit] deb bundle: $PERSEA_E2E_DEB"
  fi
else
  echo "[audit] skipping the deb bundle (AUDIT_DEB is not 1)"
  unset PERSEA_E2E_DEB || true
fi

echo "[audit] running the E2E specs under xvfb..."
# Re-check the server before the specs: the builds above take tens of
# minutes, and a server that died in between would fail every
# server-dependent spec with a confusing timeout.
if ! curl -fsS "http://127.0.0.1:8099/api/health" >/dev/null 2>&1; then
  echo "[audit] the test server is not responding before the spec run; log tail:"
  tail -40 "${PERSEA_E2E_WORK:-/nonexistent}/persea.log" 2>/dev/null || true
  exit 1
fi
echo "[audit] server pid ${PERSEA_E2E_PID:-unknown}; work dir ${PERSEA_E2E_WORK:-unknown}"
if [ -n "${PERSEA_E2E_PID:-}" ] && kill -0 "$PERSEA_E2E_PID" 2>/dev/null; then
  echo "[audit] server process alive"
else
  echo "[audit] server process DEAD (pid ${PERSEA_E2E_PID:-unknown})"
fi
if [ -f "${PERSEA_E2E_WORK:-/nonexistent}/persea.log" ]; then
  echo "[audit] server log size: $(wc -c < "${PERSEA_E2E_WORK}/persea.log") bytes"
else
  echo "[audit] server log missing at ${PERSEA_E2E_WORK:-unset}/persea.log"
fi
if ! (
  cd "$E2E_DIR"
  xvfb-run -a node run-specs.js
); then
  echo "[audit] spec run failed; server process:"
  if [ -n "${PERSEA_E2E_PID:-}" ] && kill -0 "$PERSEA_E2E_PID" 2>/dev/null; then
    echo "[audit] server process still alive"
  else
    echo "[audit] server process DEAD (pid ${PERSEA_E2E_PID:-unknown})"
  fi
  echo "[audit] server log tail:"
  tail -40 "${PERSEA_E2E_WORK:-/nonexistent}/persea.log" 2>/dev/null || true
  exit 1
fi

echo
echo "[audit] screenshots: $PERSEA_E2E_SHOTS"
echo "[audit] audit complete"
