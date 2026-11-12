#!/usr/bin/env bash
set -euo pipefail

# Приёмка развёртывания: Caddy пропускает к хабу только его собственный вход и вычищает
# поддельные заголовки. Запускается при развёртывании и после каждого изменения Caddy.
# Cookie владельца на диске не нужна.

usage() {
    cat >&2 <<'EOF'
Использование: check-proxy.sh <origin> [--positive]

<origin> — внешний адрес, например https://hub.example.com.

Без флага — только проверки без сеанса:
  - GET /api/v1/me отвечает JSON 401 unauthenticated самого хаба (не 401/302/HTML от Caddy);
  - поддельные Remote-User (логин владельца), X-Canterlot-Proxy, X-Canterlot-Client-Ip
    и X-Forwarded-For не дают входа, и ни один ответ не несёт X-Canterlot-Proxy-Leak;
  - GET / отдаёт 200 и HTML.

--positive — дополнительно вход и выход: на хосте хаба (ssh HUB_SSH, как у scripts/deploy)
  выпускается ссылка восстановления, скрипт входит по ней во временный jar, проверяет
  /api/v1/me и адрес клиента в /api/v1/auth/sessions, выходит и удаляет jar. Ссылка
  остаётся только в переменной скрипта и нигде не печатается.
  Внимание: вход создаёт сеанс, владельцу придёт push о входе; новая ссылка отменяет
  прежнюю неиспользованную ссылку владельца.

Окружение:
  EXPECTED_LOGIN  логин владельца: им подделывается Remote-User, с ним сверяется /api/v1/me
                  (без значения подделывается «owner»).
  HUB_SSH, HUB_CURRENT, HUB_CONFIG  хост и пути хаба для --positive
                  (умолчания — scripts/deploy/deploy.conf).
EOF
    exit 2
}

fail() {
    printf 'check-proxy: FAIL: %s\n' "$*" >&2
    exit 1
}

origin=""
positive=0
while (($#)); do
    case "$1" in
        --positive) positive=1; shift ;;
        -h | --help) usage ;;
        -*) usage ;;
        *) [[ -z $origin ]] || usage; origin=${1%/}; shift ;;
    esac
done
[[ $origin =~ ^https?://[^/[:space:]]+$ ]] || usage
for tool in curl jq; do
    command -v "$tool" >/dev/null || { printf 'check-proxy: %s is required\n' "$tool" >&2; exit 2; }
done
forged_login=${EXPECTED_LOGIN:-owner}

work=$(mktemp -d)
body="$work/body"
headers="$work/headers"
jar="$work/jar"
signed_in=0

# Удаляются только файлы, созданные этим скриптом в собственном каталоге mktemp.
cleanup() {
    if ((signed_in)); then
        curl -sS --max-time 15 -o /dev/null -b "$jar" -c "$jar" \
            -H "Origin: $origin" -H 'X-Canterlot-Client: 1' \
            -X POST "$origin/api/v1/auth/logout" >/dev/null 2>&1 || true
    fi
    rm -f -- "$body" "$headers" "$jar"
    rmdir -- "$work" || printf 'check-proxy: WARN: temporary directory %s is not empty\n' "$work" >&2
}
trap cleanup EXIT

# request <метод> <путь> [аргументы curl…]: тело в $body, заголовки в $headers, код в stdout.
request() {
    local method=$1 path=$2
    shift 2
    : >"$headers"
    # При сбое соединения curl сам печатает код 000.
    curl -sS --max-time 15 -o "$body" -D "$headers" -w '%{http_code}' -X "$method" "$@" "$origin$path" || true
}

has_header() { grep -qi "^$1:" "$headers"; }

no_leak() {
    if has_header X-Canterlot-Proxy-Leak; then
        fail "$1: the hub received Remote-User (X-Canterlot-Proxy-Leak): Caddy must strip Remote-*"
    fi
}

# Ответ — собственный JSON-401 хаба с одним из кодов $2… (а не вход Caddy).
expect_hub_401() {
    local label=$1 status=$2
    shift 2
    if has_header WWW-Authenticate; then
        fail "$label: HTTP $status with WWW-Authenticate: Caddy basic_auth still answers instead of the hub"
    fi
    [[ $status == 401 ]] || fail "$label: HTTP $status, expected the hub's 401"
    local codes
    codes=$(printf '%s\n' "$@" | jq -R . | jq -sc .)
    jq -e --argjson codes "$codes" '
        type == "object"
        and (.error | type == "object")
        and (.error.code as $c | $codes | index($c) != null)
        and (.error.correlation_id | type == "string")
    ' "$body" >/dev/null 2>&1 || fail "$label: 401 is not the hub's JSON error ($*): an auth layer in front of the hub?"
}

uuid_re='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'

# --- без сеанса ---
status=$(request GET /api/v1/me)
expect_hub_401 "GET /api/v1/me without a session" "$status" unauthenticated
no_leak "GET /api/v1/me without a session"

# Только Remote-User: так виден пропуск Remote-* даже при верном секрете прокси (откат на <= 1.29).
status=$(request GET /api/v1/me -H "Remote-User: $forged_login" -H 'Remote-Groups: admins')
expect_hub_401 "forged Remote-User" "$status" unauthenticated
no_leak "forged Remote-User"

status=$(request GET /api/v1/me \
    -H "Remote-User: $forged_login" -H 'Remote-Groups: admins' \
    -H 'X-Canterlot-Proxy: forged' -H 'X-Canterlot-Client-Ip: 203.0.113.7' \
    -H 'X-Forwarded-For: 127.0.0.1')
expect_hub_401 "forged proxy headers" "$status" unauthenticated
no_leak "forged proxy headers"

status=$(request GET /)
[[ $status == 200 ]] || fail "GET / returned HTTP $status, expected 200 (the PWA shell is public)"
grep -qi '^content-type:[[:space:]]*text/html' "$headers" || fail "GET / is not HTML"

printf 'check-proxy: OK without a session (hub 401 unauthenticated, forged headers stripped, / is HTML)\n'
((positive)) || exit 0

# --- вход по одноразовой ссылке восстановления ---
deploy_conf="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/deploy/deploy.conf"
# shellcheck disable=SC1090,SC1091 # deploy.conf лежит в scripts/deploy, путь вычисляется
source "$deploy_conf"
shq() { printf "'%s'" "${1//\'/\'\\\'\'}"; }
remote="runuser -u canterlot -- $(shq "$HUB_CURRENT/bin/canterlot-hub") auth recovery-link --config $(shq "$HUB_CONFIG") --minutes 5"
link=$(ssh -n -o BatchMode=yes -o ConnectTimeout=10 "$HUB_SSH" "$remote" </dev/null) \
    || fail "canterlot-hub auth recovery-link failed on $HUB_SSH"
if [[ ! $link =~ ^(https?://[^/#[:space:]]+)/recover#([A-Za-z0-9_-]{43})$ ]]; then
    link=""
    fail "unexpected output of canterlot-hub auth recovery-link (not printed)"
fi
link_origin=${BASH_REMATCH[1]}
token=${BASH_REMATCH[2]}
link=""
[[ $link_origin == "$origin" ]] || fail "the hub issues links for $link_origin, not $origin ([auth].origin)"

: >"$jar"
status=$(printf '{"token":"%s"}' "$token" | request POST /api/v1/auth/recovery \
    -b "$jar" -c "$jar" -A canterlot-check-proxy \
    -H "Origin: $origin" -H 'X-Canterlot-Client: 1' -H 'Content-Type: application/json' \
    --data-binary @-)
token=""
[[ $status == 200 ]] || fail "POST /api/v1/auth/recovery returned HTTP $status"
signed_in=1
no_leak "POST /api/v1/auth/recovery"

status=$(request GET /api/v1/me -b "$jar" -c "$jar")
[[ $status == 200 ]] || fail "GET /api/v1/me with the new session returned HTTP $status"
jq -e --arg re "$uuid_re" '
    type == "object"
    and (keys == ["login", "owner_id", "session_id"])
    and (.owner_id | type == "string" and test($re))
    and (.session_id | type == "string" and test($re))
    and (.login | type == "string" and length > 0)
' "$body" >/dev/null || fail "unexpected body of /api/v1/me"
login=$(jq -r .login "$body")
session_id=$(jq -r .session_id "$body")
if [[ -n ${EXPECTED_LOGIN:-} && $login != "$EXPECTED_LOGIN" ]]; then
    fail "login differs from EXPECTED_LOGIN"
fi

status=$(request GET /api/v1/auth/sessions -b "$jar" -c "$jar")
[[ $status == 200 ]] || fail "GET /api/v1/auth/sessions returned HTTP $status"
jq -e --arg sid "$session_id" '.current_id == $sid and ([.sessions[] | select(.current and .id == $sid)] | length == 1)' \
    "$body" >/dev/null || fail "the sessions list does not mark this session as current"
client_ip=$(jq -r --arg sid "$session_id" '.sessions[] | select(.id == $sid) | .last_ip // ""' "$body")
# Пустой адрес — Caddy не передаёт header_up X-Canterlot-Client-Ip (или секрет прокси не совпал).
[[ -n $client_ip ]] || fail "the hub does not know the client IP of this session: add header_up X-Canterlot-Client-Ip to Caddy"
case $client_ip in
    10.* | 192.168.* | 172.1[6-9].* | 172.2[0-9].* | 172.3[01].* | 127.* | f[cd]*:* | ::1)
        printf 'check-proxy: WARN: the hub sees a private client IP %s: fine from the LAN, a NAT/docker address otherwise\n' "$client_ip" >&2 ;;
esac

status=$(request POST /api/v1/auth/logout -b "$jar" -c "$jar" -H "Origin: $origin" -H 'X-Canterlot-Client: 1')
[[ $status == 200 ]] || fail "POST /api/v1/auth/logout returned HTTP $status"
signed_in=0
status=$(request GET /api/v1/me -b "$jar" -c "$jar")
expect_hub_401 "GET /api/v1/me after logout" "$status" unauthenticated session_revoked

printf 'check-proxy: OK sign-in (login %s, session %s, client IP %s), logged out\n' "$login" "$session_id" "$client_ip"
