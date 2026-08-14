#!/usr/bin/env bash
# Provision the test persea server for the desktop E2E suite.
#
# Builds the server binary from the persea repo (branch v1.2.0), creates an
# admin API key, and starts the server against a throwaway config. Prints
# the API key to stdout (the caller passes it to the specs via env).
#
# Usage: provision-server.sh <persea-repo> [port]
set -euo pipefail

PERSEA_REPO="${1:?usage: provision-server.sh <persea-repo> [port]}"
PORT="${2:-8099}"
WORK="$(mktemp -d /tmp/persea-e2e-server.XXXXXX)"
DB="$WORK/e2e.db"
REC="$WORK/recordings"
DRV="$WORK/drives"
CONF="$WORK/e2e.toml"

echo "[provision] building persea from $PERSEA_REPO (this takes a few minutes)..."
cargo build --release --manifest-path "$PERSEA_REPO/Cargo.toml" >/dev/null 2>&1
BIN="$PERSEA_REPO/target/release/persea"
[ -x "$BIN" ] || { echo "persea build failed" >&2; exit 1; }

cat > "$CONF" <<EOF
listen_addr = "127.0.0.1:$PORT"
guacd_addr = "127.0.0.1:4822"
db_path = "$DB"
recording_path = "$REC"
drive_path = "$DRV"
site_title = "Persea E2E"
rate_limit = false

[tls]
secure_cookies = false
EOF

echo "[provision] creating the admin key..."
KEY=$("$BIN" --config "$CONF" add-admin --name e2e-admin | grep "API Key:" | awk '{print $3}')
[ -n "$KEY" ] || { echo "add-admin failed" >&2; exit 1; }

echo "[provision] starting the server on 127.0.0.1:$PORT..."
"$BIN" --config "$CONF" > "$WORK/persea.log" 2>&1 &
echo $! > "$WORK/persea.pid"
for i in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:$PORT/api/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "PERSEA_E2E_BASE_URL=http://127.0.0.1:$PORT"
echo "PERSEA_E2E_API_KEY=$KEY"
echo "PERSEA_E2E_PID=$(cat "$WORK/persea.pid")"
echo "PERSEA_E2E_WORK=$WORK"
