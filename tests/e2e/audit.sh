#!/usr/bin/env bash
# Host launcher for the dockerized local UI audit. The only host
# requirement is docker; everything else (rust, node, the app build, the
# test persea server, xvfb, tauri-driver) lives in the audit image.
#
# Usage: ./tests/e2e/audit.sh <persea-repo> [--no-deb]
#
# Mounts this repo at /workspace and the persea repo at /persea, keeps
# the cargo build and the npm install in named volumes so repeat runs
# reuse them, and writes screenshots to ./audit-shots under this repo
# unless PERSEA_E2E_SHOTS points elsewhere.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
IMAGE="persea-desktop-audit:latest"
TARGET_VOLUME="persea-audit-target"
CARGO_VOLUME="persea-audit-cargo"
NPM_VOLUME="persea-audit-npm"

PERSEA_REPO="${1:?usage: ./tests/e2e/audit.sh <persea-repo> [--no-deb]}"
shift || true
AUDIT_DEB=1
for arg in "$@"; do
  case "$arg" in
    --no-deb) AUDIT_DEB=0 ;;
    *) echo "unknown option: $arg" >&2; exit 1 ;;
  esac
done

command -v docker >/dev/null 2>&1 || { echo "[audit] docker is required" >&2; exit 1; }
[ -f "$PERSEA_REPO/Cargo.toml" ] || { echo "[audit] no Cargo.toml in $PERSEA_REPO (pass the persea repo)" >&2; exit 1; }

SHOTS_DIR="${PERSEA_E2E_SHOTS:-$REPO_ROOT/audit-shots}"
mkdir -p "$SHOTS_DIR"

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "[audit] building the audit image (first run only)..."
  docker build -t "$IMAGE" -f "$SCRIPT_DIR/Dockerfile" "$SCRIPT_DIR"
fi

echo "[audit] persea repo: $PERSEA_REPO"
echo "[audit] screenshots: $SHOTS_DIR"
docker run --rm \
  -v "$REPO_ROOT:/workspace" \
  -v "$PERSEA_REPO:/persea" \
  -v "$TARGET_VOLUME:/workspace/src-tauri/target" \
  -v "$CARGO_VOLUME:/usr/local/cargo/registry" \
  -v "$NPM_VOLUME:/workspace/tests/e2e/node_modules" \
  -v "$SHOTS_DIR:/workspace/audit-shots" \
  -e PERSEA_SERVER_DIR=/persea \
  -e PERSEA_E2E_SHOTS=/workspace/audit-shots \
  -e AUDIT_DEB="$AUDIT_DEB" \
  "$IMAGE"
echo "[audit] screenshots: $SHOTS_DIR"
