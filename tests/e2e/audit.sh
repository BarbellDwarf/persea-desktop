#!/usr/bin/env bash
# Host launcher for the dockerized local UI audit. The only host
# requirement is docker; everything else (rust, node, the app build, the
# test persea server, xvfb, tauri-driver) lives in the audit image.
#
# Usage: ./tests/e2e/audit.sh [--server-dir <persea-repo>] [--no-deb]
#
# By default the test server is built from the pinned persea ref, cloned
# inside the container (the same ref the CI e2e workflow uses), so no
# local server checkout is needed. Pass --server-dir to build from a
# mounted checkout instead (for server-development testing).
#
# Mounts this repo at /workspace, keeps the cargo build and the npm
# install in named volumes so repeat runs reuse them, and writes
# screenshots to ./audit-shots under this repo unless PERSEA_E2E_SHOTS
# points elsewhere.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
IMAGE="persea-desktop-audit:latest"
TARGET_VOLUME="persea-audit-target"
CARGO_VOLUME="persea-audit-cargo"
NPM_VOLUME="persea-audit-npm"

AUDIT_DEB=1
SERVER_DIR=""
for arg in "$@"; do
  case "$arg" in
    --no-deb) AUDIT_DEB=0 ;;
    --server-dir)
      SERVER_DIR="${2:-}"
      [ -n "$SERVER_DIR" ] || { echo "[audit] --server-dir needs a path" >&2; exit 1; }
      shift
      ;;
    *) echo "unknown option: $arg" >&2; exit 1 ;;
  esac
  shift || true
done

command -v docker >/dev/null 2>&1 || { echo "[audit] docker is required" >&2; exit 1; }
if [ -n "$SERVER_DIR" ]; then
  [ -f "$SERVER_DIR/Cargo.toml" ] || { echo "[audit] no Cargo.toml in $SERVER_DIR (pass the persea repo)" >&2; exit 1; }
fi

SHOTS_DIR="${PERSEA_E2E_SHOTS:-$REPO_ROOT/audit-shots}"
mkdir -p "$SHOTS_DIR"

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "[audit] building the audit image (first run only)..."
  docker build -t "$IMAGE" -f "$SCRIPT_DIR/Dockerfile" "$SCRIPT_DIR"
fi

echo "[audit] screenshots: $SHOTS_DIR"
if [ -n "$SERVER_DIR" ]; then
  echo "[audit] server: mounted checkout at $SERVER_DIR"
else
  echo "[audit] server: pinned persea ref (cloned in the container)"
fi
docker run --rm \
  -v "$REPO_ROOT:/workspace" \
  -v "$TARGET_VOLUME:/workspace/src-tauri/target" \
  -v "$CARGO_VOLUME:/usr/local/cargo/registry" \
  -v "$NPM_VOLUME:/workspace/tests/e2e/node_modules" \
  -v "$SHOTS_DIR:/workspace/audit-shots" \
  -e PERSEA_E2E_SHOTS=/workspace/audit-shots \
  -e AUDIT_DEB="$AUDIT_DEB" \
  ${SERVER_DIR:+-v "$SERVER_DIR:/persea" -e PERSEA_SERVER_DIR=/persea} \
  "$IMAGE"
echo "[audit] screenshots: $SHOTS_DIR"
