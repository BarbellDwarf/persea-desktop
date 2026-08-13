#!/usr/bin/env bash
set -euo pipefail

# Dev runner: builds and runs Persea Desktop against a local persea
# server. D01 stub: the URL is validated and exported for the app to
# consume; real instance management lands in D02.

PERSEA_URL="${PERSEA_URL:-http://127.0.0.1:8089}"

case "$PERSEA_URL" in
  http://*|https://*) ;;
  *) echo "error: PERSEA_URL must be an http(s) URL, got '$PERSEA_URL'" >&2; exit 1 ;;
esac

export PERSEA_URL
echo "persea-desktop dev -> server: $PERSEA_URL"

cd "$(dirname "$0")/.."

if command -v cargo-tauri >/dev/null 2>&1 || command -v tauri >/dev/null 2>&1; then
  exec cargo tauri dev "$@"
fi

echo "tauri-cli not installed; falling back to cargo run (embedded shell assets)"
exec cargo run --manifest-path src-tauri/Cargo.toml "$@"
