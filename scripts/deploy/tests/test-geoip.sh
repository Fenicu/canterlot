#!/usr/bin/env bash
# Шаг geoip выката (lib.sh: geoip_due, geoip_refresh): базы нет или она старше
# GEOIP_MAX_AGE_DAYS — canterlot-hub geoip update на хосте хаба через run_write;
# свежая база не трогается; любая неудача мягкая; репетиция только печатает.
# shellcheck disable=SC2015,SC2016 # assert pattern; код для bash -c
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

SHA=aaaaaaaaaaaa

# Хаб с подменённым CLI: geoip update пишет файл базы (или падает по FAKE_GEOIP_FAIL).
setup_geoip() {
    setup_case "geoip-$1"
    write_configs
    deploy_release hub "${SHA}"
    cat >"${HUB_RELEASES}/${SHA}/bin/canterlot-hub" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${FAKE_ROOT}/state/hub-cli.log"
[ "$1 $2" = "geoip update" ] || { echo "unexpected command" >&2; exit 64; }
if [ "${FAKE_GEOIP_FAIL:-0}" = "1" ]; then echo "geoip_download_failed" >&2; exit 1; fi
mkdir -p "$(dirname "${HUB_GEOIP_PATH}")"
printf 'mmdb\n' >"${HUB_GEOIP_PATH}"
echo "updated"
EOF
    chmod +x "${HUB_RELEASES}/${SHA}/bin/canterlot-hub"
}
# refresh [DEPLOY_MODE]: geoip_refresh в отдельном bash (set -e библиотеки не попадает в тест).
refresh() {
    local count_file="${FAKE_ROOT}/state/rehearse-count"
    : >"${count_file}"
    DEPLOY_MODE="${1:-real}" REHEARSE_COUNT_FILE="${count_file}" bash -c '
        source "$1/lib.sh"
        geoip_refresh
        echo "rc=$? soft=${SOFT_FAILURES}"' _ "${DEPLOY_DIR}" >"${FAKE_ROOT}/geoip.log" 2>&1
}
cli_calls() { cat "${FAKE_ROOT}/state/hub-cli.log" 2>/dev/null || true; }

case_begin "a missing database is downloaded"
setup_geoip missing
rm -f "${HUB_GEOIP_PATH}"
refresh
assert_contains "${FAKE_ROOT}/geoip.log" "rc=0 soft=0" "no soft failure"
assert_contains "${FAKE_ROOT}/geoip.log" "dbip-city-lite.mmdb is missing: running canterlot-hub geoip update" "reason named"
assert_eq "$(cli_calls)" "geoip update --config ${HUB_CONFIG}" "CLI called once with the hub config"
[ -s "${HUB_GEOIP_PATH}" ] && case_ok "database written" || case_fail "database missing after update"
assert_contains "${FAKE_ROOT}/geoip.log" "geoip: update done" "success logged"
teardown_case
case_end

case_begin "a fresh database is left alone"
setup_geoip fresh
touch -d '34 days ago' "${HUB_GEOIP_PATH}"
refresh
assert_contains "${FAKE_ROOT}/geoip.log" "rc=0 soft=0" "no soft failure"
assert_contains "${FAKE_ROOT}/geoip.log" "is 34 days old, no update" "age logged"
assert_eq "$(cli_calls)" "" "CLI not called"
teardown_case
case_end

case_begin "a database older than 35 days is refreshed"
setup_geoip stale
touch -d '36 days ago' "${HUB_GEOIP_PATH}"
refresh
assert_contains "${FAKE_ROOT}/geoip.log" "is 36 days old (more than 35)" "age and limit named"
assert_eq "$(cli_calls)" "geoip update --config ${HUB_CONFIG}" "CLI called"
assert_contains "${FAKE_ROOT}/geoip.log" "rc=0 soft=0" "no soft failure"
teardown_case
case_end

case_begin "a failed update is a soft note, never fatal"
setup_geoip fail
rm -f "${HUB_GEOIP_PATH}"
FAKE_GEOIP_FAIL=1 refresh
assert_contains "${FAKE_ROOT}/geoip.log" "rc=0 soft=1" "one soft failure, rc 0"
assert_contains "${FAKE_ROOT}/geoip.log" "SOFT-FAIL: geoip update failed; sign-in locations stay empty" "soft failure named"
[ ! -e "${HUB_GEOIP_PATH}" ] && case_ok "nothing written" || case_fail "file appeared"
teardown_case
case_end

case_begin "an unreadable hub is a soft note"
setup_geoip unreadable
rm -f "${HUB_GEOIP_PATH}"
FAKE_FAIL_CMD="stat -c" refresh
assert_contains "${FAKE_ROOT}/geoip.log" "rc=0 soft=1" "one soft failure, rc 0"
assert_contains "${FAKE_ROOT}/geoip.log" "geoip: cannot read" "reason named"
assert_eq "$(cli_calls)" "" "CLI not called"
teardown_case
case_end

case_begin "rehearsal prints the update and runs nothing"
setup_geoip rehearse
rm -f "${HUB_GEOIP_PATH}"
refresh rehearse
assert_contains "${FAKE_ROOT}/geoip.log" "WOULD-WRITE #1 (geoip update):" "write printed"
assert_contains "${FAKE_ROOT}/geoip.log" 'geoip\ update\ --config' "printed command names the subcommand"
assert_eq "$(cli_calls)" "" "CLI not called"
[ ! -e "${HUB_GEOIP_PATH}" ] && case_ok "nothing written" || case_fail "file appeared"
teardown_case
case_end

printf '\nPASS %s FAIL %s\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
