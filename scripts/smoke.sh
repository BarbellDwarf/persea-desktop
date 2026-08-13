#!/usr/bin/env bash
set -euo pipefail

# Smoke test: build the app and verify the window process opens and
# stays alive. Requires webkit2gtk-4.1 dev packages and an X/Wayland
# display (run under xvfb-run on headless machines). The tauri-driver
# based harness is D16; this script is the D01 placeholder.

cd "$(dirname "$0")/.."

cargo build --manifest-path src-tauri/Cargo.toml

BIN="src-tauri/target/debug/persea-desktop"
if [ ! -x "$BIN" ]; then
  echo "error: no binary at $BIN (did the build fail?)" >&2
  exit 1
fi

echo "smoke: launching $BIN, keeping it alive for 15s"
"$BIN" &
PID=$!

sleep 15

if kill -0 "$PID" 2>/dev/null; then
  echo "smoke: app still alive after 15s, window opened"
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  exit 0
fi

echo "smoke: app exited before the 15s mark" >&2
wait "$PID" 2>/dev/null || true
exit 1
