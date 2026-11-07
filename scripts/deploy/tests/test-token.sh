#!/usr/bin/env bash
# Токен подтверждения (решение владельца 5): годен 2 часа от dry-run/репетиции,
# привязан к id релиза и к хэшу точного плана. Проверки lib.sh — в отдельном bash.
# shellcheck disable=SC2015,SC2016 # assert pattern; код для bash -c
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

ID=aaaaaaaaaaaa
PLAN=0123456789ab
# check_token ТОКЕН ID ПЛАН [NOW] -> печатает "rc:<код> <причина>"
check_token() {
    bash -c 'source "$1/lib.sh"; set +e; msg="$(confirm_token_check "$2" "$3" "$4" "$5")"; printf "rc:%s %s" "$?" "${msg}"' \
        _ "${DEPLOY_DIR}" "$1" "$2" "$3" "${4:-}"
}
mint() { bash -c 'source "$1/lib.sh"; confirm_token "$2" "$3" "$4"' _ "${DEPLOY_DIR}" "$1" "$2" "$3"; }
NOW="$(date -u +%s)"

case_begin "a fresh token for the same release and plan is accepted"
T="$(mint "${ID}" "${PLAN}" "${NOW}")"
[[ "${T}" =~ ^[0-9]{10}-[0-9a-f]{12}-[0-9a-f]{8}$ ]] && case_ok "token shape <epoch>-<plan>-<check>: ${T}" || case_fail "odd token ${T}"
OUT="$(check_token "${T}" "${ID}" "${PLAN}" "${NOW}")"
assert_eq "${OUT}" "rc:0 " "fresh token accepted"
OUT="$(check_token "$(mint "${ID}" "${PLAN}" "$((NOW - 7199))")" "${ID}" "${PLAN}" "${NOW}")"
assert_eq "${OUT}" "rc:0 " "a token 1 h 59 min 59 s old is still accepted"
case_end

case_begin "an expired token is refused with a clear message"
OUT="$(check_token "$(mint "${ID}" "${PLAN}" "$((NOW - 7201))")" "${ID}" "${PLAN}" "${NOW}")"
case "${OUT}" in rc:1*"expired"*"120 min"*) case_ok "expired after 2 h: ${OUT#rc:1 }" ;; *) case_fail "expired token: ${OUT}" ;; esac
OUT="$(check_token "$(mint "${ID}" "${PLAN}" "$((NOW + 3600))")" "${ID}" "${PLAN}" "${NOW}")"
case "${OUT}" in rc:1*"future"*) case_ok "future token refused" ;; *) case_fail "future token: ${OUT}" ;; esac
case_end

case_begin "a token of another release or another plan is refused"
OUT="$(check_token "$(mint bbbbbbbbbbbb "${PLAN}" "${NOW}")" "${ID}" "${PLAN}" "${NOW}")"
case "${OUT}" in rc:1*"not issued for release ${ID}"*) case_ok "other release refused" ;; *) case_fail "other release: ${OUT}" ;; esac
OUT="$(check_token "$(mint "${ID}" ffffffffffff "${NOW}")" "${ID}" "${PLAN}" "${NOW}")"
case "${OUT}" in rc:1*"plan changed"*"ffffffffffff"*"${PLAN}"*) case_ok "changed plan refused, both hashes named" ;; *) case_fail "changed plan: ${OUT}" ;; esac
T="$(mint "${ID}" "${PLAN}" "${NOW}")"
OUT="$(check_token "${T%-*}-00000000" "${ID}" "${PLAN}" "${NOW}")"
case "${OUT}" in rc:1*"altered"*) case_ok "edited check part refused" ;; *) case_fail "edited token: ${OUT}" ;; esac
for bad in "" deadbeefdeadbeef "x-y-z" "$(printf '%s:%s' "${ID}" "$(date -u +%F)" | sha256sum | cut -c1-16)"; do
    OUT="$(check_token "${bad}" "${ID}" "${PLAN}" "${NOW}")"
    case "${OUT}" in rc:1*"malformed"*) case_ok "malformed '${bad}' refused (incl. the old same-day token)" ;; *) case_fail "malformed '${bad}': ${OUT}" ;; esac
done
case_end

case_begin "the plan hash follows the baseline, the dist and the toolkit"
setup_case token-plan
write_configs
deploy_release hub bbbbbbbbbbbb
deploy_release web bbbbbbbbbbbb
deploy_release worker bbbbbbbbbbbb
mkdir -p "${FAKE_ROOT}/dist/${ID}"
printf 'sha=%s\n' "${ID}" >"${FAKE_ROOT}/dist/${ID}/RELEASE"
printf 'x  ./RELEASE\n' >"${FAKE_ROOT}/dist/${ID}/SHA256SUMS"
ph() { bash -c 'source "$1/lib.sh"; plan_hash "$2" "$3" "$4" "$5" "$6"' _ "$1" "${ID}" "${FAKE_ROOT}/dist/${ID}" "$2" "$3" "$4"; }
P0="$(ph "${DEPLOY_DIR}" bbbbbbbbbbbb bbbbbbbbbbbb bbbbbbbbbbbb)"
assert_eq "$(ph "${DEPLOY_DIR}" bbbbbbbbbbbb bbbbbbbbbbbb bbbbbbbbbbbb)" "${P0}" "plan hash is deterministic"
[ "$(ph "${DEPLOY_DIR}" bbbbbbbbbbbb cccccccccccc bbbbbbbbbbbb)" != "${P0}" ] && case_ok "another deployed web release changes the plan" || case_fail "baseline not in the plan"
printf 'y  ./RELEASE\n' >"${FAKE_ROOT}/dist/${ID}/SHA256SUMS"
[ "$(ph "${DEPLOY_DIR}" bbbbbbbbbbbb bbbbbbbbbbbb bbbbbbbbbbbb)" != "${P0}" ] && case_ok "a changed dist SHA256SUMS changes the plan" || case_fail "dist not in the plan"
printf 'format=1\ncapabilities=a\nharnesses=b\n' >"${FAKE_ROOT}/dist/${ID}/EXPECT"
P1="$(ph "${DEPLOY_DIR}" bbbbbbbbbbbb bbbbbbbbbbbb bbbbbbbbbbbb)"
mkdir -p "${FAKE_ROOT}/toolkit"
cp -r "${DEPLOY_DIR}/." "${FAKE_ROOT}/toolkit/"
printf '\n# edited\n' >>"${FAKE_ROOT}/toolkit/verify.sh"
[ "$(ph "${FAKE_ROOT}/toolkit" bbbbbbbbbbbb bbbbbbbbbbbb bbbbbbbbbbbb)" != "${P1}" ] && case_ok "an edited toolkit script changes the plan" || case_fail "toolkit not in the plan"
teardown_case
case_end

printf 'token harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
