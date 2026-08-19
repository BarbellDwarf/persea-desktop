#!/usr/bin/env bash
# Run the full desktop UI audit inside the audit container. This is the
# single contract for both the local docker launcher (audit.sh) and,
# later, the equivalent GH Actions job, so it stays self-contained: no
# host-specific paths, no interactive prompts.
#
# Env:
#   PERSEA_SERVER_DIR    mounted persea repo path (required)
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

: "${PERSEA_SERVER_DIR:?PERSEA_SERVER_DIR is required (mounted persea repo)}"
: "${PERSEA_E2E_SHOTS:?PERSEA_E2E_SHOTS is required (screenshot output dir)}"
mkdir -p "$PERSEA_E2E_SHOTS"

echo "[audit] installing node deps..."
(
  cd "$E2E_DIR"
  # --no-package-lock: never write a lockfile into the mounted checkout.
  npm install --no-package-lock
)

echo "[audit] provisioning the persea server on 8099..."
PROVISION_OUT="$(bash "$E2E_DIR/provision-server.sh" "$PERSEA_SERVER_DIR" 8099)"
export "$(printf '%s\n' "$PROVISION_OUT" | grep '^PERSEA_E2E')"
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
  echo "[audit] building the deb bundle (release)..."
  (
    cd "$WORKSPACE/src-tauri"
    # CI mode avoids interactive prompts; updater artifacts are skipped
    # because the signing key only exists in the release secrets.
    CI=true cargo tauri build --bundles deb \
      --config '{"bundle":{"createUpdaterArtifacts":false}}'
  )
  shopt -s nullglob
  DEBS=("$WORKSPACE/src-tauri/target/release/bundle/deb"/*.deb)
  [ "${#DEBS[@]}" -gt 0 ] || { echo "[audit] deb build produced no .deb" >&2; exit 1; }
  export PERSEA_E2E_DEB="${DEBS[0]}"
  echo "[audit] deb bundle: $PERSEA_E2E_DEB"
else
  echo "[audit] skipping the deb bundle (AUDIT_DEB is not 1)"
  unset PERSEA_E2E_DEB || true
fi

echo "[audit] running the E2E specs under xvfb..."
(
  cd "$E2E_DIR"
  xvfb-run -a node run-specs.js
)

echo
echo "[audit] screenshots: $PERSEA_E2E_SHOTS"
echo "[audit] audit complete"
