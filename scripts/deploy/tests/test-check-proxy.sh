#!/usr/bin/env bash
# scripts/check-proxy.sh против подменённого curl (сайт за Caddy) и подменённого CLI хаба:
# JSON-401 хаба принимается, 401/302 от Caddy и метка X-Canterlot-Proxy-Leak — провал;
# положительная проверка входит по ссылке восстановления, выходит, удаляет jar и
# нигде не печатает ссылку.
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

SHA=aaaaaaaaaaaa
ORIGIN=https://hub.example
TOKEN=SeCrEtToKeN_check-proxy_0123456789abcdefghi

setup_cp() {
    setup_case "check-proxy-$1"
    write_configs
    deploy_release hub "${SHA}"
    printf '%s' "${TOKEN}" >"${FAKE_ROOT}/state/issued-token"
    # CLI хаба: печатает ссылку восстановления (FAKE_CLI_OUT подменяет вывод).
    cat >"${HUB_RELEASES}/${SHA}/bin/canterlot-hub" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${FAKE_ROOT}/state/hub-cli.log"
if [ -n "${FAKE_CLI_OUT:-}" ]; then printf '%s\n' "${FAKE_CLI_OUT}"; exit 0; fi
printf 'https://hub.example/recover#%s\n' "$(cat "${FAKE_ROOT}/state/issued-token")"
EOF
    chmod +x "${HUB_RELEASES}/${SHA}/bin/canterlot-hub"
    mkdir -p "${FAKE_ROOT}/cp-bin"
    # Сайт за Caddy: режим CP_MODE выбирает ответ прокси, сеанс хранится в jar.
    cat >"${FAKE_ROOT}/cp-bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
out=/dev/null hdr="" method=GET jar_in="" jar_out="" data=0 url="" ua=""
hs=()
while (($#)); do
    case "$1" in
        -sS) shift ;;
        --max-time | -w) shift 2 ;;
        -o) out=$2; shift 2 ;;
        -D) hdr=$2; shift 2 ;;
        -X) method=$2; shift 2 ;;
        -H) hs+=("$2"); shift 2 ;;
        -b) jar_in=$2; shift 2 ;;
        -c) jar_out=$2; shift 2 ;;
        -A) ua=$2; shift 2 ;;
        --data-binary) [ "$2" = "@-" ] || exit 98; data=1; shift 2 ;;
        -*) echo "stub-curl: unexpected flag $1" >&2; exit 99 ;;
        *) url=$1; shift ;;
    esac
done
state="${FAKE_ROOT}/state"
path="/${url#https://hub.example/}"
[ "${url}" != "https://hub.example" ] || path=/
posted=""
if [ "${data}" = "1" ]; then posted="$(cat)"; printf '%s\n' "${posted}" >>"${state}/posted"; fi
header() {
    local h
    for h in "${hs[@]}"; do
        if [ "${h%%:*}" = "$1" ]; then printf '%s' "${h#*: }"; return 0; fi
    done
    return 1
}
printf '%s %s ua=%s headers=%s\n' "${method}" "${path}" "${ua}" "$(IFS='|'; printf '%s' "${hs[*]}")" >>"${state}/requests"
authed=0
if [ -n "${jar_in}" ] && grep -q '^session=' "${jar_in}" 2>/dev/null; then authed=1; fi
extra=()
mode="${CP_MODE:-hub}"
if [ "${mode}" = "leak" ] && header Remote-User >/dev/null; then extra+=("X-Canterlot-Proxy-Leak: 1"); fi
respond() {
    local code=$1 ctype=$2 body=$3 line
    if [ -n "${hdr}" ]; then
        {
            printf 'HTTP/1.1 %s\r\n' "${code}"
            [ -z "${ctype}" ] || printf 'content-type: %s\r\n' "${ctype}"
            for line in "${extra[@]}"; do printf '%s\r\n' "${line}"; done
            printf '\r\n'
        } >"${hdr}"
    fi
    printf '%s' "${body}" >"${out}"
    printf '%s' "${code}"
    exit 0
}
hub_401() {
    respond 401 application/json "{\"error\":{\"code\":\"$1\",\"message\":\"Требуется вход\",\"scope\":\"auth\",\"retry\":\"after_condition\",\"correlation_id\":\"01a1cf1a-0000-7000-8000-000000000001\"}}"
}
SID=01a1cf1a-0000-7000-8000-00000000000a
case "${path}" in
    /)
        [ "${mode}" != "root-401" ] || respond 401 "" ""
        respond 200 "text/html; charset=utf-8" "<!doctype html><title>canterlot</title>" ;;
    /api/*)
        case "${mode}" in
            caddy-basic) extra+=('WWW-Authenticate: Basic realm="restricted"'); respond 401 "" "" ;;
            caddy-302) extra+=("Location: https://auth.example/"); respond 302 "text/html" "<a href=\"https://auth.example/\">Found</a>" ;;
        esac ;;
esac
origin_ok=0
if [ "$(header Origin || true)" = "https://hub.example" ] && [ "$(header X-Canterlot-Client || true)" = "1" ]; then origin_ok=1; fi
case "${method} ${path}" in
    "GET /api/v1/me")
        [ "${authed}" = "1" ] || hub_401 unauthenticated
        respond 200 application/json "{\"owner_id\":\"01a18c86-e8a2-74da-86da-26eaa3fe6eaa\",\"login\":\"owner\",\"session_id\":\"${SID}\"}" ;;
    "POST /api/v1/auth/recovery")
        [ "${origin_ok}" = "1" ] || respond 403 application/json '{"error":{"code":"forbidden"}}'
        [ "${posted}" = "{\"token\":\"$(cat "${state}/issued-token")\"}" ] || hub_401 invalid_credentials
        printf 'session=%s\n' "${SID}" >"${jar_out}"
        respond 200 application/json "{\"session\":{\"id\":\"${SID}\"}}" ;;
    "GET /api/v1/auth/sessions")
        [ "${authed}" = "1" ] || hub_401 unauthenticated
        ip='"198.51.100.23"'
        [ "${mode}" != "no-ip" ] || ip=null
        respond 200 application/json "{\"current_id\":\"${SID}\",\"idle_timeout_days\":90,\"sessions\":[{\"id\":\"${SID}\",\"current\":true,\"last_ip\":${ip},\"created_ip\":${ip}},{\"id\":\"01a1cf1a-0000-7000-8000-00000000000b\",\"current\":false,\"last_ip\":\"192.0.2.1\"}]}" ;;
    "POST /api/v1/auth/logout")
        [ "${authed}" = "1" ] || hub_401 unauthenticated
        [ "${origin_ok}" = "1" ] || respond 403 application/json '{"error":{"code":"forbidden"}}'
        printf 'logout\n' >>"${state}/logouts"
        : >"${jar_out}"
        respond 200 application/json '{}' ;;
esac
respond 404 application/json '{"error":{"code":"not_found"}}'
EOF
    chmod +x "${FAKE_ROOT}/cp-bin/curl"
}
# run_cp [аргументы check-proxy.sh]: stdout и stderr вместе в cp.log, код в CP_RC.
run_cp() {
    CP_RC=0
    PATH="${FAKE_ROOT}/cp-bin:${PATH}" bash "${REPO_ROOT}/scripts/check-proxy.sh" "${ORIGIN}" "$@" >"${FAKE_ROOT}/cp.log" 2>&1 || CP_RC=$?
}
tmp_left() { find "${FAKE_ROOT}/tmp" -mindepth 1 | wc -l; }
logouts() { if [ -f "${FAKE_ROOT}/state/logouts" ]; then wc -l <"${FAKE_ROOT}/state/logouts"; else echo 0; fi; }
no_secret_printed() {
    assert_not_contains "${FAKE_ROOT}/cp.log" "${TOKEN}" "the recovery token is never printed"
    assert_not_contains "${FAKE_ROOT}/cp.log" "recover#" "the recovery link is never printed"
}

case_begin "negatives: the hub's JSON 401 is accepted, forged headers carry the owner login"
setup_cp negatives
EXPECTED_LOGIN=owner run_cp
assert_eq "${CP_RC}" "0" "exit 0"
assert_contains "${FAKE_ROOT}/cp.log" "check-proxy: OK without a session" "OK printed"
assert_contains "${FAKE_ROOT}/state/requests" "GET /api/v1/me ua= headers=Remote-User: owner|Remote-Groups: admins" "Remote-User alone forged with the owner login"
assert_contains "${FAKE_ROOT}/state/requests" "X-Canterlot-Client-Ip: 203.0.113.7|X-Forwarded-For: 127.0.0.1" "client IP and X-Forwarded-For forged"
assert_contains "${FAKE_ROOT}/state/requests" "X-Canterlot-Proxy: forged" "proxy secret forged"
assert_contains "${FAKE_ROOT}/state/requests" "GET / ua=" "the PWA shell probed"
[ ! -e "${FAKE_ROOT}/state/hub-cli.log" ] && case_ok "no CLI call without --positive" || case_fail "CLI called"
assert_eq "$(tmp_left)" "0" "temporary directory removed"
teardown_case
case_end

case_begin "Caddy basic_auth 401 is rejected"
setup_cp basic
CP_MODE=caddy-basic run_cp
assert_eq "${CP_RC}" "1" "exit 1"
assert_contains "${FAKE_ROOT}/cp.log" "Caddy basic_auth still answers" "reason named"
assert_eq "$(tmp_left)" "0" "temporary directory removed"
teardown_case
case_end

case_begin "a Caddy redirect is rejected"
setup_cp redirect
CP_MODE=caddy-302 run_cp
assert_eq "${CP_RC}" "1" "exit 1"
assert_contains "${FAKE_ROOT}/cp.log" "HTTP 302, expected the hub's 401" "reason named"
teardown_case
case_end

case_begin "X-Canterlot-Proxy-Leak on a forged Remote-User is rejected"
setup_cp leak
CP_MODE=leak run_cp
assert_eq "${CP_RC}" "1" "exit 1"
assert_contains "${FAKE_ROOT}/cp.log" "forged Remote-User: the hub received Remote-User (X-Canterlot-Proxy-Leak)" "leak named"
teardown_case
case_end

case_begin "a closed PWA shell is rejected"
setup_cp root
CP_MODE=root-401 run_cp
assert_eq "${CP_RC}" "1" "exit 1"
assert_contains "${FAKE_ROOT}/cp.log" "GET / returned HTTP 401" "reason named"
teardown_case
case_end

case_begin "positive: sign-in by a recovery link, client IP, logout, jar removed, link never printed"
setup_cp positive
EXPECTED_LOGIN=owner run_cp --positive
assert_eq "${CP_RC}" "0" "exit 0"
assert_contains "${FAKE_ROOT}/cp.log" "OK sign-in (login owner, session 01a1cf1a-0000-7000-8000-00000000000a, client IP 198.51.100.23), logged out" "positive OK printed"
assert_eq "$(cat "${FAKE_ROOT}/state/hub-cli.log")" "auth recovery-link --config ${HUB_CONFIG} --minutes 5" "link issued by the hub CLI of the current release"
assert_eq "$(cat "${FAKE_ROOT}/state/posted")" "{\"token\":\"${TOKEN}\"}" "token posted in the body (stdin), not argv"
assert_contains "${FAKE_ROOT}/state/requests" "POST /api/v1/auth/recovery ua=canterlot-check-proxy headers=Origin: https://hub.example|X-Canterlot-Client: 1" "recovery POST carries Origin and X-Canterlot-Client"
assert_eq "$(logouts)" "1" "logged out exactly once"
assert_eq "$(tmp_left)" "0" "jar and its directory removed"
no_secret_printed
teardown_case
case_end

case_begin "positive: an unknown client IP fails, the session is still logged out"
setup_cp noip
CP_MODE=no-ip run_cp --positive
assert_eq "${CP_RC}" "1" "exit 1"
assert_contains "${FAKE_ROOT}/cp.log" "add header_up X-Canterlot-Client-Ip to Caddy" "missing header_up named"
assert_eq "$(logouts)" "1" "logout on the failure path"
assert_eq "$(tmp_left)" "0" "jar and its directory removed"
no_secret_printed
teardown_case
case_end

case_begin "positive: unexpected CLI output fails without echoing it"
setup_cp garbage
FAKE_CLI_OUT="https://hub.example/recover#${TOKEN}extra" run_cp --positive
assert_eq "${CP_RC}" "1" "exit 1"
assert_contains "${FAKE_ROOT}/cp.log" "unexpected output of canterlot-hub auth recovery-link (not printed)" "reason named"
assert_eq "$(logouts)" "0" "no sign-in, no logout"
assert_eq "$(tmp_left)" "0" "temporary directory removed"
no_secret_printed
teardown_case
case_end

case_begin "positive: a link for another origin fails without echoing the token"
setup_cp origin
FAKE_CLI_OUT="https://other.example/recover#${TOKEN}" run_cp --positive
assert_eq "${CP_RC}" "1" "exit 1"
assert_contains "${FAKE_ROOT}/cp.log" "the hub issues links for https://other.example, not https://hub.example" "origin mismatch named"
[ ! -e "${FAKE_ROOT}/state/posted" ] && case_ok "nothing posted" || case_fail "token posted to the wrong check"
no_secret_printed
teardown_case
case_end

case_begin "positive: a login other than EXPECTED_LOGIN fails and logs out"
setup_cp login
EXPECTED_LOGIN=someone-else run_cp --positive
assert_eq "${CP_RC}" "1" "exit 1"
assert_contains "${FAKE_ROOT}/cp.log" "login differs from EXPECTED_LOGIN" "reason named"
assert_eq "$(logouts)" "1" "logout on the failure path"
assert_eq "$(tmp_left)" "0" "jar and its directory removed"
no_secret_printed
teardown_case
case_end

case_begin "usage"
setup_cp usage
CP_RC=0
bash "${REPO_ROOT}/scripts/check-proxy.sh" >"${FAKE_ROOT}/cp.log" 2>&1 || CP_RC=$?
assert_eq "${CP_RC}" "2" "no origin: exit 2"
assert_contains "${FAKE_ROOT}/cp.log" "push о входе" "usage warns about the sign-in push"
teardown_case
case_end

printf '\nPASS %s FAIL %s\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
