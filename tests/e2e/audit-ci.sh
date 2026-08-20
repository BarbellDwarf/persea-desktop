#!/usr/bin/env bash
# Audit CI setup: seed the OpenLDAP harness, configure the LDAP provider on
# the provisioned server, create the LDAP users, restart the server so the
# auth chain picks the provider up, and stand up the SSH target (local user
# + LDAP passthrough via nslcd on the same sshd).
#
# Env (set by the audit workflow / provision-server.sh):
#   PERSEA_E2E_BASE_URL     the provisioned server (required)
#   PERSEA_E2E_SERVER_PID    the server process pid (required)
#   PERSEA_E2E_SERVER_WORK   the provision work dir holding e2e.toml
#   PERSEA_SERVER_DIR        the persea repo clone (binary + seed fixture)
#   PERSEA_E2E_ADMIN_EMAIL   default e2e-admin
#   PERSEA_E2E_ADMIN_PASSWORD default e2e-admin-password-12345
#   PERSEA_E2E_LDAP_URI      default ldap://127.0.0.1:3389
set -euo pipefail

BASE="${PERSEA_E2E_BASE_URL:?PERSEA_E2E_BASE_URL is required}"
PID="${PERSEA_E2E_SERVER_PID:?PERSEA_E2E_SERVER_PID is required}"
WORK="${PERSEA_E2E_SERVER_WORK:?PERSEA_E2E_SERVER_WORK is required}"
SERVER_DIR="${PERSEA_SERVER_DIR:?PERSEA_SERVER_DIR is required}"
ADMIN_EMAIL="${PERSEA_E2E_ADMIN_EMAIL:-e2e-admin}"
ADMIN_PASSWORD="${PERSEA_E2E_ADMIN_PASSWORD:-e2e-admin-password-12345}"
LDAP_URI="${PERSEA_E2E_LDAP_URI:-ldap://127.0.0.1:3389}"
PORT="${BASE##*:}"
HOST="${BASE%%:*}"

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JAR="$(mktemp)"
CSRF=""

api_login() { # user password
  local jar="$JAR" csrf
  curl -fsS -c "$jar" "$HOST:$PORT/" > /dev/null
  csrf="$(awk '/csrf_token/{print $7}' "$jar")"
  CSRF="$csrf"
  curl -s -o /dev/null -b "$jar" -c "$jar" -X POST "$HOST:$PORT/auth/login" \
    --data-urlencode "csrf_token=$csrf" --data-urlencode "username=$1" \
    --data-urlencode "password=$2"
}

api() { # method path [json]
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -s -b "$JAR" -X "$method" "http://$HOST:$PORT$path" \
      -H "Content-Type: application/json" -H "X-CSRF-Token: $CSRF" -d "$body"
  else
    curl -s -b "$JAR" -X "$method" "http://$HOST:$PORT$path" -H "X-CSRF-Token: $CSRF"
  fi
}

echo "[ci-audit] seeding the LDAP harness..."
ldapadd -x -H "${LDAP_URI#ldap://}" -D cn=admin,dc=example,dc=com -w admin \
  -f "$SERVER_DIR/tests/fixtures/ldap-seed.ldif" > /dev/null 2>&1 || true
ldapadd -x -H "${LDAP_URI#ldap://}" -D cn=admin,dc=example,dc=com -w admin \
  -f "$DIR/ldap-posix.ldif" > /dev/null 2>&1 || true

echo "[ci-audit] configuring the LDAP provider and users..."
api_login "$ADMIN_EMAIL" "$ADMIN_PASSWORD"
PROVIDER_JSON="$(cat <<EOF
{"name":"LDAP","type":"ldap","config":{"url":"$LDAP_URI","bind_dn":"cn=admin,dc=example,dc=com","bind_password":"admin","search_base":"ou=users,dc=example,dc=com","search_filter":"(uid={})","group_search_base":"ou=groups,dc=example,dc=com","group_search_filter":"(member={})","tls_skip_verify":false,"starttls":false,"connect_timeout_secs":10,"display_name_attr":"cn","email_attr":"mail"}}
EOF
)"
api POST /api/auth/providers "$PROVIDER_JSON" > /dev/null || true
for entry in 'alice@example.com|Alice Example' 'bob@example.com|Bob Example'; do
  email="${entry%%|*}"
  name="${entry##*|}"
  api POST /api/users "{\"email\":\"$email\",\"name\":\"$name\",\"password\":\"ldap-only-user-2026\",\"role\":\"viewer\"}" > /dev/null || true
done

echo "[ci-audit] restarting the server for the new auth chain..."
kill "$PID" 2>/dev/null || true
sleep 2
BIN="$SERVER_DIR/target/release/persea"
CONF="$WORK/e2e.toml"
nohup "$BIN" --config "$CONF" > "$WORK/persea.log" 2>&1 &
echo $! > "$WORK/persea.pid"
for i in $(seq 1 30); do
  curl -fsS "$HOST:$PORT/api/health" > /dev/null 2>&1 && break
  sleep 1
done

echo "[ci-audit] standing up the SSH targets (local + LDAP passthrough)..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq > /dev/null 2>&1
apt-get install -y -qq libnss-ldapd libpam-ldapd > /dev/null 2>&1 || true
cat > /etc/nslcd.conf <<CONF
uid nslcd
gid nslcd
uri $LDAP_URI
base dc=example,dc=com
binddn cn=admin,dc=example,dc=com
bindpw admin
CONF
sed -i "s/^passwd:.*/passwd: files ldap/" /etc/nsswitch.conf
sed -i "s/^group:.*/group: files ldap/" /etc/nsswitch.conf
# Local target user.
useradd -m -s /bin/bash sshuser 2>/dev/null || true
echo 'sshuser:ssh-test-password-2026' | chpasswd 2>/dev/null || true
mkdir -p /run/sshd
ssh-keygen -A > /dev/null 2>&1 || true
cat > /etc/ssh/sshd_config-audit <<CONF
Port 2222
PasswordAuthentication yes
UsePAM yes
AllowUsers sshuser alice bob
CONF
nslcd
sleep 2
/usr/sbin/sshd -f /etc/ssh/sshd_config-audit || true

echo "[ci-audit] setup complete"
