#!/usr/bin/env bash
set -euo pipefail

# Приёмка развёртывания: Caddy и Authelia не пропускают поддельные заголовки личности к хабу.
# Запускается один раз при развёртывании и после каждого изменения Caddy.
#   check-proxy.sh <origin> <cookie-jar>
# <origin> -- внешний адрес, например https://chat.example; <cookie-jar> -- файл cookie в формате curl
# с действующей сессией Authelia (curl -c). Необязательная EXPECTED_LOGIN сверяет login владельца.

usage() {
    printf 'Usage: %s <origin> <cookie-jar>\n' "$0" >&2
    exit 2
}

fail() {
    printf 'check-proxy: FAIL: %s\n' "$*" >&2
    exit 1
}

(( $# == 2 )) || usage
origin=${1%/}
cookies=$2
[[ -r $cookies ]] || { printf 'check-proxy: cookie jar is not readable\n' >&2; exit 2; }
for tool in curl jq; do
    command -v "$tool" >/dev/null || { printf 'check-proxy: %s is required\n' "$tool" >&2; exit 2; }
done

body=$(mktemp)
trap 'rm -f "$body"' EXIT

# 1. Действующая сессия плюс подделанные заголовки: отвечать должен владелец, а не «attacker».
status=$(curl -sS --max-time 15 -o "$body" -w '%{http_code}' -b "$cookies" \
    -H 'Remote-User: attacker' -H 'Remote-Groups: admins' \
    -H 'X-Canterlot-Proxy: forged' -H 'X-Forwarded-For: 127.0.0.1' \
    "$origin/api/v1/me")
[[ $status == 200 ]] || fail "request with a session returned HTTP $status"
jq -e '
    type == "object"
    and (keys == ["login", "owner_id"])
    and (.owner_id | type == "string" and test("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"))
    and (.login | type == "string" and length > 0)
' "$body" >/dev/null || fail "unexpected body of /api/v1/me"
login=$(jq -r .login "$body")
[[ $login != attacker ]] || fail "forged Remote-User reached the hub"
if [[ -n ${EXPECTED_LOGIN:-} && $login != "$EXPECTED_LOGIN" ]]; then
    fail "login differs from EXPECTED_LOGIN"
fi

# 2. Без сессии запрос останавливает Authelia: переход на вход (3xx) или 401/403 в формате Authelia.
#    Ответ хаба (его JSON с error или owner_id) означает, что forward_auth обойдён.
status=$(curl -sS --max-time 15 -o "$body" -w '%{http_code}' \
    -H 'Remote-User: attacker' -H 'X-Canterlot-Proxy: forged' \
    "$origin/api/v1/me")
case $status in
    301|302|303|307|308|401|403) ;;
    *) fail "request without a session returned HTTP $status" ;;
esac
if jq -e 'type == "object" and (has("owner_id") or has("error"))' "$body" >/dev/null 2>&1; then
    fail "request without a session reached the hub"
fi

printf 'check-proxy: OK (login %s)\n' "$login"
