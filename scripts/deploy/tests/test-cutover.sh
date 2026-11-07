#!/usr/bin/env bash
# Тесты cutover.sh под новую политику: никакого автоматического отката БД и хаба.
# До flip: любая неудача/сигнал — рестарт воркера на его предыдущем релизе.
# С flip: СТОП и доклад, без действий (воркер остановлен и безопасен).
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

cargo() { echo "cargo $*" >>"${FAKE_ROOT}/state/calls.log"; return 0; }
export -f cargo
sleep() { case "${1:-}" in 1|2|3|5|60) command sleep 0.1;; *) command sleep "$@";; esac; }
export -f sleep

SHA_NEW=aaaaaaaaaaaa
SHA_PREV=bbbbbbbbbbbb
EXEC1=""
EXEC2=""

write_ownership() {
    mkdir -p "${WORKER_DATA}/ownership"
    python3 - "${WORKER_DATA}/ownership/snapshot.toml" "${EXEC1}" <<'EOF'
import sys
path, live = sys.argv[1], sys.argv[2]
def varint(n):
    out = b""
    while True:
        b = n & 0x7F; n >>= 7
        out += bytes([b | (0x80 if n else 0)])
        if not n: return out
def rec(exec_id, phase):
    b = varint(1 << 3 | 2) + varint(len(exec_id)) + exec_id.encode() + varint(14 << 3 | 0) + varint(phase)
    return "[" + ", ".join(str(x) for x in b) + "]"
dead = "deadbeef-dead-4ead-beef-deadbeefdead"
open(path, "w").write("seq = 0\nrecords = [%s, %s]\n" % (rec(live, 0), rec(dead, 3)))
EOF
}

setup_cutover() {
    setup_case "cutover-$1"
    export SITE_CODES="meeting.fenicu.com:200 auth.fenicu.com:200 git.fenicu.com:200 caddy.fenicu.com:302 gotify.fenicu.com:200 immich.fenicu.com:200 bookshelf.fenicu.com:200 hub.fenicu.com:302"
    EXEC1="$(new_exec_id)"; EXEC2="$(new_exec_id)"; export EXEC1 EXEC2
    write_configs
    deploy_release hub "${SHA_PREV}"
    deploy_release web "${SHA_PREV}"
    deploy_release worker "${SHA_PREV}"
    printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
    cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/state/db-pre.txt"
    printf '%s\n%s\n' "${EXEC1}" "${EXEC2}" >"${FAKE_ROOT}/state/live-ids"
    printf 'aaaaaaaa-0000-4000-8000-000000000001\n' >"${FAKE_ROOT}/state/live-convs"
    export FAKE_HUB_MIGRATE_TO=41 FAKE_MIGR_FAILED_AFTER=0
    export FAKE_READYZ_MODE=byhub FAKE_READYZ_NEW_SHA="${SHA_NEW}" FAKE_READYZ_NEW=1
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-worker >/dev/null
    FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-hub >/dev/null
    spawn_session "${EXEC1}" "${SHA_PREV}" 100
    spawn_session "${EXEC2}" "${SHA_PREV}" 200
    mkdir -p "${WORKER_DATA}/cj"
    python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', 50) + b'\0'*4)" >"${WORKER_DATA}/cj/ack"
    write_ownership
    # Staged новый релиз.
    mkdir -p "${HUB_RELEASES}/${SHA_NEW}/bin"
    cp /bin/sleep "${HUB_RELEASES}/${SHA_NEW}/bin/canterlot-hub"
    chmod +x "${HUB_RELEASES}/${SHA_NEW}/bin/canterlot-hub"
    printf 'sha=%s\ndate=x\nmigrations_last=0041_fake_harness.sql\n' "${SHA_NEW}" >"${HUB_RELEASES}/${SHA_NEW}/RELEASE"
    printf 'sha256-placeholder\n' >"${HUB_RELEASES}/${SHA_NEW}/SHA256SUMS"
    mkdir -p "${WEB_RELEASES}/${SHA_NEW}/_app/immutable/chunks"
    printf '{"version":"%s"}' "${SHA_NEW}" >"${WEB_RELEASES}/${SHA_NEW}/_app/version.json"
    printf 'console.log(1)\n' >"${WEB_RELEASES}/${SHA_NEW}/_app/immutable/chunks/probe.js"
    mkdir -p "${WORKER_RELEASES}/${SHA_NEW}/bin"
    cp "${FAKE_WORKER_BIN}" "${WORKER_RELEASES}/${SHA_NEW}/bin/canterlot-worker"
    chmod +x "${WORKER_RELEASES}/${SHA_NEW}/bin/canterlot-worker"
    printf 'sha=%s\ndate=x\nmigrations_last=0041_fake_harness.sql\n' "${SHA_NEW}" >"${WORKER_RELEASES}/${SHA_NEW}/RELEASE"
    DIST_DIR="${FAKE_ROOT}/dist/${SHA_NEW}"
    mkdir -p "${DIST_DIR}"
    printf 'sha=%s\ndate=x\nmigrations_last=0041_fake_harness.sql\n' "${SHA_NEW}" >"${DIST_DIR}/RELEASE"
    # CSP цели выведен из финального билда: хэш встроенного скрипта 200.html (проверка до flip).
    mkdir -p "${DIST_DIR}/web"
    printf '<html><script>window.x=1</script></html>\n' >"${DIST_DIR}/web/200.html"
    printf "CSP-BODY-NEW script-src '%s'" 'sha256-hUmDl3yY/7GaaX+Zuy8aEy8IzxgpBs7xccqYHs2/7kQ=' >"${DIST_DIR}/csp.txt"
    : >"${DIST_DIR}/SHA256SUMS"
    printf '{"version":"%s"}' "${SHA_PREV}" >"${FAKE_ROOT}/container/version.json"
    printf 'CSP-BODY-PREV' >"${FAKE_ROOT}/container/csp"
    printf 'CANTERLOT_CSP="CSP-BODY-PREV"\n' >"${WEB_ENV}"
    printf 'CANTERLOT_CSP="CSP-BODY-PREV"\n' >"${WEB_ENV}.${SHA_PREV}"
    SUP1="$(sup_pid_of "${EXEC1}")"; SUP2="$(sup_pid_of "${EXEC2}")"; export SUP1 SUP2
}
confirm_for() { printf '%s:%s' "$1" "$(date -u +%Y-%m-%d)" | sha256sum | cut -c1-16; }
run_cutover() {
    bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "$(confirm_for "${SHA_NEW}")" \
        --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1
}
hub_is() { [ "$(readlink "${HUB_CURRENT}" | sed 's|.*/||')" = "$1" ]; }
web_is() { [ "$(readlink "${WEB_BUILD_LINK}" | sed 's|.*/||')" = "$1" ]; }
worker_is() { [ "$(readlink "${WORKER_CURRENT}" | sed 's|.*/||')" = "$1" ]; }
db_migr_is() {
    [ "$(grep '^MIGR_MAX=' "${FAKE_ROOT}/state/db.txt" | cut -d= -f2)" = "$1" ] \
        && [ "$(grep '^FAILED=' "${FAKE_ROOT}/state/db.txt" | cut -d= -f2)" = "$2" ]
}
dropdb_logged() { grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log"; }
# Любое восстановление базы: dropdb/createdb или pg_restore не в режимах
# проверки (--list / -f /dev/null). Проверки дампа разрешены, restore — нет.
restore_logged() {
    awk '
        /^dropdb / || /^createdb / { found = 1 }
        /^pg_restore / && !/--list/ && !/-f \/dev\/null/ { found = 1 }
        END { exit found ? 0 : 1 }
    ' "${FAKE_ROOT}/state/calls.log"
}
pgdump_logged() { grep -q '^pg_dump ' "${FAKE_ROOT}/state/calls.log"; }
compose_up_count() { grep -c "compose up -d caddy" "${FAKE_ROOT}/state/calls.log" || true; }
# set -m: фоновые задания не игнорят INT (иначе trap cutover не сработает в harness).
run_cutover_bg() {
    set -m
    bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "$(confirm_for "${SHA_NEW}")" \
        --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1 &
    CUTOVER_PID=$!
}
wait_for_log() {
    local log="$1" marker="$2" _
    for _ in $(seq 1 1800); do
        grep -q "${marker}" "${log}" 2>/dev/null && return 0
        sleep 0.1
    done
    return 1
}

# --- A0. SHA с shell-метасимволами отклоняется до любой удалённой команды ---
case_begin "invalid release id rejected"
setup_cutover badsha
if ( cd "${FAKE_ROOT}" && bash "${DEPLOY_DIR}/cutover.sh" --execute "';>PWN;#xxxx" --confirm x --source "${REPO_ROOT}" \
        --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" ) >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_fail "exit 0 despite an invalid release id"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/cutover.log" "invalid release id" "invalid release id reported"
[ ! -e "${FAKE_ROOT}/PWN" ] && case_ok "no injected file created" || case_fail "shell injection executed"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
grep -q 'stop canterlot-worker' "${FAKE_ROOT}/state/calls.log" && case_fail "worker stopped" || case_ok "worker untouched"
teardown_case
case_end

# --- A0b. боевой прогон без верного --confirm отказывает до любых действий ---
case_begin "real run refused without the correct --confirm"
setup_cutover noconfirm
if bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --source "${REPO_ROOT}" \
        --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_fail "exit 0 without --confirm"
else
    case_ok "exit nonzero without --confirm"
fi
assert_contains "${FAKE_ROOT}/cutover.log" "requires --confirm" "missing token reported"
if bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm deadbeefdeadbeef --source "${REPO_ROOT}" \
        --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_fail "exit 0 with a wrong --confirm"
else
    case_ok "exit nonzero with a wrong --confirm"
fi
assert_contains "${FAKE_ROOT}/cutover.log" "wrong --confirm" "wrong token reported"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker untouched" || case_fail "worker moved"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
grep -q 'stop canterlot-worker' "${FAKE_ROOT}/state/calls.log" && case_fail "worker stopped" || case_ok "worker never stopped"
teardown_case
case_end

# --- A. ворота не прошли до стопа: воркер даже не останавливался ---
case_begin "gates fail before stop"
setup_cutover gatesfail
export FAKE_CT_LOCK=1
if run_cutover; then case_fail "exit 0 despite failed gates"; else case_ok "exit nonzero"; fi
unset FAKE_CT_LOCK
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker untouched" || case_fail "worker moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker still active (never stopped)" || case_fail "worker not active"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
if ls "${FAKE_ROOT}/hub/backups"/canterlot-pre-*.dump >/dev/null 2>&1; then case_fail "dump file created"; else case_ok "no dump file"; fi
assert_proc_alive "${SUP1}" "session 1 alive"
assert_proc_alive "${SUP2}" "session 2 alive"
teardown_case
case_end

# --- A2. провал осмотра pvesh — жёстко, а не «блокировки нет» ---
case_begin "pvesh inspection fails"
setup_cutover pvesfail
export FAKE_PVES_FAIL=1
if run_cutover; then case_fail "exit 0 despite failed pvesh"; else case_ok "exit nonzero"; fi
unset FAKE_PVES_FAIL
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker untouched" || case_fail "worker moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker still active (never stopped)" || case_fail "worker not active"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
assert_contains "${FAKE_ROOT}/cutover.log" "cannot discover the node of CT" "pvesh failure reported (node discovery)"
teardown_case
case_end

# --- A2b. нода найдена, но запрос статуса CT падает — жёстко, а не «блокировки нет» ---
case_begin "pvesh lock query fails after node discovery"
setup_cutover pvesstatusfail
export FAKE_PVES_FAIL_STATUS=1
if run_cutover; then case_fail "exit 0 despite failed CT status query"; else case_ok "exit nonzero"; fi
unset FAKE_PVES_FAIL_STATUS
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker untouched" || case_fail "worker moved"
assert_contains "${FAKE_ROOT}/cutover.log" "cannot query CT 130 lock state" "status query failure reported"
teardown_case
case_end

# --- A2c. PROXMOX_NODE расходится с настоящей нодой CT — отказ до любых изменений ---
case_begin "PROXMOX_NODE disagrees with the real node"
setup_cutover pvewrongnode
export PROXMOX_NODE=proxmox-main
if run_cutover; then case_fail "exit 0 despite a wrong PROXMOX_NODE"; else case_ok "exit nonzero"; fi
export PROXMOX_NODE=bigboy
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker still active (never stopped)" || case_fail "worker not active"
assert_contains "${FAKE_ROOT}/cutover.log" "PROXMOX_NODE=proxmox-main but CT 130 runs on node bigboy" "both names reported"
teardown_case
case_end

# --- A3. провал удалённого ps — жёстко, а не «процессов нет» ---
case_begin "remote ps inspection fails"
setup_cutover psfail
export FAKE_FAIL_CMD="ps -eo"
if run_cutover; then case_fail "exit 0 despite failed ps"; else case_ok "exit nonzero"; fi
unset FAKE_FAIL_CMD
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker still active (never stopped)" || case_fail "worker not active"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
assert_contains "${FAKE_ROOT}/cutover.log" "cannot inspect vzdump" "ps failure reported as inspection failure"
teardown_case
case_end
# --- A4. провал запроса compactions — жёстко, а не «таблицы нет» ---
case_begin "compactions query fails"
setup_cutover compactionsfail
export FAKE_PSQL_FAIL_MATCH="information_schema.tables"
if run_cutover; then case_fail "exit 0 despite failed compactions query"; else case_ok "exit nonzero"; fi
unset FAKE_PSQL_FAIL_MATCH
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker untouched" || case_fail "worker moved"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
assert_contains "${FAKE_ROOT}/cutover.log" "cannot query compactions table" "query failure reported"
teardown_case
case_end

# --- B. набор супервизоров изменился после preflight: новый допустим, записанный пропавший — нет ---
case_begin "new supervisor is allowed, a recorded one disappearing is not"
setup_cutover supset
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "preflight PASS" || case_fail "no preflight marker"
# Новый супервизор (без строки хаба) — это не дрейф записанных процессов.
SUPX_DIR="${WORKER_DATA}/executions/ffffffff-ffff-4fff-8fff-ffffffffffff"
mkdir -p "${SUPX_DIR}"
setsid bash -c "exec -a '${WORKER_RELEASES}/${SHA_PREV}/bin/canterlot-worker' '${WORKER_RELEASES}/${SHA_PREV}/bin/canterlot-worker' supervise --exec-dir '${SUPX_DIR}'" >/dev/null 2>&1 <&- &
rc=0; wait "${CUTOVER_PID}" || rc=$?
[ "${rc}" = "0" ] && case_ok "exit 0 with a new supervisor (set equality not required)" || case_fail "exit ${rc} on a new supervisor"
hub_is "${SHA_NEW}" && case_ok "hub flipped" || case_fail "hub not flipped"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- C. сессия умерла после стопа воркера ---
case_begin "session gone after worker stop"
setup_cutover stoprace
# Реальная задержка stop: CLI успевает умереть до сверки тиков после остановки.
export FAKE_SYSTEMCTL_STOP_DELAY=2
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "stopping worker" || case_fail "no stop marker"
CLI1="$(ps -o pid= --ppid "${SUP1}" 2>/dev/null | tr -s ' ' '\n' | grep -v '^$' | head -1)"
[ -n "${CLI1}" ] && kill "${CLI1}" || case_fail "no CLI to kill"
rc=0; wait "${CUTOVER_PID}" || rc=$?
unset FAKE_SYSTEMCTL_STOP_DELAY
[ "${rc}" != "0" ] && case_ok "exit nonzero" || case_fail "exit 0 despite gone session"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker still prev" || case_fail "worker moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran after rollback"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker restarted" || case_fail "worker not restarted"
assert_proc_alive "${SUP1}" "supervisor alive"
assert_proc_alive "${SUP2}" "session 2 alive"
teardown_case
case_end

# --- D. vzdump падает ---
case_begin "vzdump fails"
setup_cutover vzdumpfail
export FAKE_FAIL_VZDUMP=1
if run_cutover; then case_fail "exit 0 despite vzdump failure"; else case_ok "exit nonzero"; fi
unset FAKE_FAIL_VZDUMP
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker still prev" || case_fail "worker moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker restarted" || case_fail "worker not restarted"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- E. pg_dump падает ---
case_begin "pg_dump fails"
setup_cutover pgdumpfail
export FAKE_FAIL_PG_DUMP=1
if run_cutover; then case_fail "exit 0 despite pg_dump failure"; else case_ok "exit nonzero"; fi
unset FAKE_FAIL_PG_DUMP
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
pgdump_logged && case_ok "pg_dump attempted" || case_fail "pg_dump never ran"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker restarted" || case_fail "worker not restarted"
teardown_case
case_end

# --- F. дамп не прошёл проверку (битый TOC) ---
case_begin "dump verification fails"
setup_cutover corruptdump
export FAKE_DUMP_CORRUPT=1
if run_cutover; then case_fail "exit 0 despite corrupt dump"; else case_ok "exit nonzero"; fi
unset FAKE_DUMP_CORRUPT
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "dump verification failed" "verification failure logged"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker restarted" || case_fail "worker not restarted"
teardown_case
case_end

# --- F2. pg_restore -f /dev/null — проверка чтением, база не меняется ---
case_begin "pg_restore -f /dev/null is read-only"
setup_cutover pgreadonly
READONLY_DUMP="${FAKE_ROOT}/hub/backups/readonly.dump"
printf 'TOC-LINE-01\nTOC-LINE-02 _sqlx_migrations table\npayload-in-dump\n' >"${READONLY_DUMP}"
cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/state/db-before-readonly.txt"
if "${FAKE_ROOT}/fake-bin/pg_restore" -f /dev/null "${READONLY_DUMP}" >/dev/null 2>&1; then
    case_ok "read-only pg_restore exit 0"
else
    case_fail "read-only pg_restore failed"
fi
cmp -s "${FAKE_ROOT}/state/db-before-readonly.txt" "${FAKE_ROOT}/state/db.txt" \
    && case_ok "fake database untouched by -f /dev/null" \
    || case_fail "fake database changed by -f /dev/null"
grep -q '^pg_restore .*-f /dev/null ' "${FAKE_ROOT}/state/calls.log" \
    && case_ok "read-only restore was logged as such" || case_fail "read-only restore not logged"
teardown_case
case_end

# --- G. hub flip падает до смены current: безопасное допеределочное восстановление ---
case_begin "hub flip fails with current unchanged"
setup_cutover flipfail
: >"${FAKE_ROOT}/state/calls.log"
export FAKE_FAIL_CMD="mv -T"
if run_cutover; then case_fail "exit 0 despite hub flip failure"; else case_ok "exit nonzero"; fi
unset FAKE_FAIL_CMD
hub_is "${SHA_PREV}" && case_ok "hub untouched (no flip-back: nothing was flipped)" || case_fail "hub moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "provably unchanged" "readback proved current unchanged"
assert_not_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "pre-flip recovery, not stop-and-report"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker restarted on its previous release" || case_fail "worker left stopped"
assert_proc_alive "${SUP1}" "session 1 alive"
[ ! -e "${HUB_BASE}/current.new" ] && case_ok "leftover current.new removed" || case_fail "current.new left behind"
teardown_case
case_end

# --- G2. leftover current.new: отказ до остановки воркера ---
case_begin "leftover current.new blocks cutover before stop"
setup_cutover currentnew
ln -s "releases/${SHA_PREV}" "${HUB_BASE}/current.new"
: >"${FAKE_ROOT}/state/calls.log"
if run_cutover; then case_fail "exit 0 with leftover current.new"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/cutover.log" "leftover" "leftover current.new reported"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
grep -q 'stop canterlot-worker' "${FAKE_ROOT}/state/calls.log" \
    && case_fail "worker stopped before prereq check" || case_ok "worker never stopped"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
teardown_case
case_end

# --- G3. flip успел сменить current, проверка упала: STOP-and-report ---
case_begin "failed flip with changed current stops and reports"
setup_cutover flipchanged
export FAKE_FAIL_CMD="readlink -m"
if run_cutover; then case_fail "exit 0 despite post-flip verification failure"; else case_ok "exit nonzero"; fi
unset FAKE_FAIL_CMD
hub_is "${SHA_NEW}" && case_ok "hub is on the new release (flip happened)" || case_fail "hub not flipped"
assert_contains "${FAKE_ROOT}/cutover.log" "treating as post-flip" "changed current classified as post-flip"
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker started (must stay stopped)" || case_ok "worker stays stopped"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
teardown_case
case_end

# --- G4. неполный целевой hub-релиз: отказ до остановки воркера ---
case_begin "incomplete hub target blocks cutover before stop"
setup_cutover incompletetarget
rm -f "${HUB_RELEASES}/${SHA_NEW}/bin/canterlot-hub"
: >"${FAKE_ROOT}/state/calls.log"
if run_cutover; then case_fail "exit 0 with incomplete hub target"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/cutover.log" "hub target release ${SHA_NEW} is incomplete" "incomplete target reported"
grep -q 'stop canterlot-worker' "${FAKE_ROOT}/state/calls.log" \
    && case_fail "worker stopped before prereq check" || case_ok "worker never stopped"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
teardown_case
case_end

# --- H. readyz падает после рестарта: СТОП, никаких действий, воркер остановлен ---
# (миграции уже могли накатиться — восстановление только вручную через rollback-db.sh)
case_begin "hub readyz fails"
setup_cutover readyzfail
export FAKE_READYZ_NEW=0
if run_cutover; then case_fail "exit 0 despite readyz failure"; else case_ok "exit nonzero"; fi
unset FAKE_READYZ_NEW
hub_is "${SHA_NEW}" && case_ok "hub stays flipped (no auto downgrade)" || case_fail "hub flipped back"
worker_is "${SHA_PREV}" && case_ok "worker never flipped" || case_fail "worker moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore, ever"
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
assert_contains "${FAKE_ROOT}/cutover.log" "rollback-db.sh" "manual restore command printed"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker started (must stay stopped)" || case_ok "worker stays stopped"
assert_proc_alive "${SUP1}" "session 1 alive"
assert_proc_alive "${SUP2}" "session 2 alive"
# Напечатанная команда восстановления должна работать дословно: режим +x и
# никаких скрытых подстановок, которых нет в отчёте.
RECOVERY_CMD="$(grep -m1 'scripts/deploy/rollback-db.sh' "${FAKE_ROOT}/cutover.log" | sed 's/^[[:space:]]*//')"
if [ -n "${RECOVERY_CMD}" ] && ( cd "${REPO_ROOT}" && eval "${RECOVERY_CMD}" ) >"${FAKE_ROOT}/recovery.log" 2>&1; then
    case_ok "printed recovery command runs verbatim"
else
    case_fail "printed recovery command failed"; tail -3 "${FAKE_ROOT}/recovery.log" 2>/dev/null
fi
hub_is "${SHA_PREV}" && case_ok "recovery flipped hub back" || case_fail "recovery did not flip hub"
[ "$(grep '^MIGR_MAX=' "${FAKE_ROOT}/state/db.txt" | cut -d= -f2)" = "40" ] \
    && case_ok "recovery restored the dump" || case_fail "recovery did not restore the dump"
teardown_case
case_end

# --- I. ошибка миграции в журнале: то же, что H (СТОП, без действий) ---
case_begin "migration error"
setup_cutover migrationerror
export FAKE_JOURNAL_MIGRATION_ERROR=1 FAKE_MIGR_FAILED_AFTER=0
if run_cutover; then case_fail "exit 0 despite migration error"; else case_ok "exit nonzero"; fi
unset FAKE_JOURNAL_MIGRATION_ERROR FAKE_MIGR_FAILED_AFTER
hub_is "${SHA_NEW}" && case_ok "hub stays flipped" || case_fail "hub moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "migration_error in hub journal" "migration error gate fired"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker started" || case_ok "worker stays stopped"
teardown_case
case_end

# --- I2. журнал хаба не читается — жёстко, не «ошибок нет» ---
case_begin "hub journal inspection fails"
setup_cutover journalfail
export FAKE_FAIL_JOURNALCTL=1
if run_cutover; then case_fail "exit 0 despite unreadable journal"; else case_ok "exit nonzero"; fi
unset FAKE_FAIL_JOURNALCTL
hub_is "${SHA_NEW}" && case_ok "hub stays flipped" || case_fail "hub moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "cannot read hub journal after restart" "journal failure reported"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker started" || case_ok "worker stays stopped"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- J. PWA падает: мягко, flip-back, выкат продолжается, конец — успех ---
case_begin "PWA failure is soft"
setup_cutover pwafail
export FAKE_FAIL_CADDY_UP=1
if run_cutover; then case_ok "exit 0"; else case_fail "exit nonzero on soft PWA failure"; fi
unset FAKE_FAIL_CADDY_UP
hub_is "${SHA_NEW}" && case_ok "hub on new" || case_fail "hub not new"
worker_is "${SHA_NEW}" && case_ok "worker on new" || case_fail "worker not new"
web_is "${SHA_PREV}" && case_ok "web flipped back to prev" || case_fail "web not prev"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "PWA failure (soft" "PWA soft failure recorded"
assert_contains "${FAKE_ROOT}/cutover.log" "verify PASS" "verify passed after PWA fallback"
[ "$(cat "${WEB_ENV}")" = "$(cat "${WEB_ENV}.${SHA_PREV}")" ] && case_ok "env restored to prev" || case_fail "env not restored"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- J2. dist csp.txt пропал: отказ ДО любого flip, ничего не тронуто ---
case_begin "missing dist csp refuses before any flip"
setup_cutover pwacspmissing
rm -f "${FAKE_ROOT}/dist/${SHA_NEW}/csp.txt"
if run_cutover; then case_fail "exit 0 without a dist CSP"; else case_ok "exit nonzero"; fi
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub flipped"
worker_is "${SHA_PREV}" && case_ok "worker untouched" || case_fail "worker flipped"
assert_contains "${FAKE_ROOT}/cutover.log" "CSP is not derived from the final PWA build, refusing before any flip" "refusal reported"
[ "$(cat "${WEB_ENV}")" = "$(cat "${WEB_ENV}.${SHA_PREV}")" ] \
    && case_ok "env untouched" || case_fail "env overwritten"
teardown_case
case_end

# --- J2b. csp.txt не выведен из финального билда (устаревший хэш): отказ до flip ---
case_begin "stale csp hash refuses before any flip"
setup_cutover pwacspstale
printf "CSP-BODY-NEW script-src 'sha256-STALE'" >"${FAKE_ROOT}/dist/${SHA_NEW}/csp.txt"
if run_cutover; then case_fail "exit 0 with a stale CSP hash"; else case_ok "exit nonzero"; fi
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub flipped"
assert_contains "${FAKE_ROOT}/cutover.log" "differ from the dist web/200.html inline scripts" "mismatch counts reported"
assert_not_contains "${FAKE_ROOT}/cutover.log" "STALE" "hash value not printed"
teardown_case
case_end

# --- K. старт воркера падает: СТОП, без отката, воркер остановлен ---
case_begin "worker start fails"
setup_cutover workerstartfail
export FAKE_FAIL_WORKER_START=once
if run_cutover; then case_fail "exit 0 despite worker start failure"; else case_ok "exit nonzero"; fi
unset FAKE_FAIL_WORKER_START
hub_is "${SHA_NEW}" && case_ok "hub stays new (no auto downgrade)" || case_fail "hub moved"
worker_is "${SHA_NEW}" && case_ok "worker stays flipped (no auto flip-back)" || case_fail "worker flipped back"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "worker start failed" "start failure recorded"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker started (must stay stopped)" || case_ok "worker stays stopped"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- L. счастливый путь: всё новое, verify PASS, один recreate ---
case_begin "cutover happy"
setup_cutover happy
if run_cutover; then case_ok "exit 0"; else case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/cutover.log"; fi
hub_is "${SHA_NEW}" && case_ok "hub on new" || case_fail "hub not new"
worker_is "${SHA_NEW}" && case_ok "worker on new" || case_fail "worker not new"
web_is "${SHA_NEW}" && case_ok "web on new" || case_fail "web not new"
db_migr_is 41 0 && case_ok "db migrated" || case_fail "db not migrated"
restore_logged && case_fail "database restore ran on happy path" || case_ok "no dropdb/createdb/pg_restore restore"
[ "$(compose_up_count)" = "1" ] && case_ok "exactly one caddy recreate" || case_fail "recreate count wrong"
grep -q "compose run" "${FAKE_ROOT}/state/calls.log" && case_fail "compose run used" || case_ok "no compose run"
assert_contains "${FAKE_ROOT}/cutover.log" "verify PASS" "verify passed inside cutover"
assert_proc_alive "${SUP1}" "session 1 supervisor alive"
assert_proc_alive "${SUP2}" "session 2 supervisor alive"
teardown_case
case_end

# --- M. INT после стопа воркера: только рестарт на prev, hub untouched ---
case_begin "INT after worker stop"
setup_cutover sigint-stopped
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "stopping worker" || case_fail "no stop marker"
kill -INT "${CUTOVER_PID}" || case_fail "kill failed"
rc=0; wait "${CUTOVER_PID}" || rc=$?
[ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker still prev" || case_fail "worker moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker restarted by trap" || case_fail "worker not started"
assert_proc_alive "${SUP1}" "session 1 alive"
assert_proc_alive "${SUP2}" "session 2 alive"
teardown_case
case_end

# --- N. TERM после бэкапа: только рестарт, дамп цел, база нетронута ---
case_begin "TERM after backup"
setup_cutover sigterm-backedup
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "restore points:" || case_fail "no backup marker"
kill -TERM "${CUTOVER_PID}" || case_fail "kill failed"
rc=0; wait "${CUTOVER_PID}" || rc=$?
[ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
# Ветку определяет фаза, записанная обработчиком, а не текущий линк хаба:
# сигнал мог прийти между mv и сменой PHASE.
LATE_PHASE="$(sed -n 's/.*STOP-AND-REPORT (phase \([^)]*\)).*/\1/p' "${FAKE_ROOT}/cutover.log" | head -1)"
EARLY_PHASE="$(sed -n 's/.*STOP before hub flip (phase \([^)]*\)).*/\1/p' "${FAKE_ROOT}/cutover.log" | head -1)"
if [ -n "${LATE_PHASE}" ]; then
    case "${LATE_PHASE}" in
        hub-flipping|hub-flipped|hub-restarting|hub-verified|pwa|pwa-done|worker-flipping|worker-starting|worker-started|verifying)
            case_ok "late path phase ${LATE_PHASE}" ;;
        *) case_fail "unexpected post-flip phase ${LATE_PHASE}" ;;
    esac
    [ -z "${EARLY_PHASE}" ] && case_ok "no early-path marker" || case_fail "both early and late markers"
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
        && case_fail "worker started (must stay stopped)" || case_ok "worker stays stopped"
    assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
elif [ -n "${EARLY_PHASE}" ]; then
    case "${EARLY_PHASE}" in
        early|worker-stopping|worker-stopped|backing-up|dumping|dump-verifying|backed-up)
            case_ok "early path phase ${EARLY_PHASE}" ;;
        *) case_fail "unexpected early phase ${EARLY_PHASE}" ;;
    esac
    hub_is "${SHA_PREV}" && case_ok "hub untouched on early path" || case_fail "hub moved on early path"
    diff -q "${FAKE_ROOT}/state/db-pre.txt" <(grep -v '^TOC' "${FAKE_ROOT}/state/db.txt" 2>/dev/null || cat "${FAKE_ROOT}/state/db.txt") >/dev/null \
        && case_ok "db intact" || case_fail "db changed"
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
        && case_ok "worker restarted by trap" || case_fail "worker not started"
    assert_not_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "no post-flip report on early path"
else
    case_fail "no recovery phase recorded in the log"
fi
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore, ever"
teardown_case
case_end

# --- O. INT после рестарта хаба: СТОП, без действий с базой/хабом, воркер остановлен ---
case_begin "INT after hub restart"
setup_cutover sigint-hubflipped
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "hub restarted once" || case_fail "no restart marker"
kill -INT "${CUTOVER_PID}" || case_fail "kill failed"
rc=0; wait "${CUTOVER_PID}" || rc=$?
[ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
hub_is "${SHA_NEW}" && case_ok "hub stays flipped (no auto downgrade)" || case_fail "hub moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
if worker_is "${SHA_PREV}"; then
    case_ok "worker not flipped yet"
elif worker_is "${SHA_NEW}"; then
    case_ok "worker flipped before the signal"
else
    case_fail "worker in unknown state"
fi
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker running after post-flip failure" || case_ok "worker stopped by the handler"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- P. TERM в середине PWA: СТОП, воркер остановлен, хаб новый, без действий ---
case_begin "TERM during PWA"
setup_cutover sigterm-pwa
export FAKE_CADDY_UP_DELAY=5
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "PWA step (all failures soft)" || case_fail "no PWA marker"
kill -TERM "${CUTOVER_PID}" || case_fail "kill failed"
rc=0; wait "${CUTOVER_PID}" || rc=$?
unset FAKE_CADDY_UP_DELAY
[ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
hub_is "${SHA_NEW}" && case_ok "hub stays new (PWA never rolls back hub)" || case_fail "hub moved"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker started (must stay stopped)" || case_ok "worker stays stopped"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- Q. сигнал во время verify: СТОП, воркер остановлен, хаб новый, без действий ---
case_begin "signal during verify"
setup_cutover sigverify
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "running verification" || case_fail "no verify marker"
kill -TERM "${CUTOVER_PID}" || case_fail "kill failed"
rc=0; wait "${CUTOVER_PID}" || rc=$?
[ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
hub_is "${SHA_NEW}" && case_ok "hub stays new" || case_fail "hub moved"
worker_is "${SHA_NEW}" && case_ok "worker was started on the new release" || case_fail "worker not on new"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker running after signal during verify" || case_ok "worker stopped by the handler"
assert_proc_alive "${SUP1}" "session 1 alive"
assert_proc_alive "${SUP2}" "session 2 alive"
teardown_case

case_end

# --- R. TERM после старта воркера: СТОП, воркер остановлен, хаб новый, без действий ---
case_begin "TERM after worker start"
setup_cutover sigterm-workerstarted
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "session PIDs and ticks unchanged after worker start" \
    || case_fail "no worker-start marker"
kill -TERM "${CUTOVER_PID}" || case_fail "kill failed"
rc=0; wait "${CUTOVER_PID}" || rc=$?
[ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
hub_is "${SHA_NEW}" && case_ok "hub stays new" || case_fail "hub moved"
worker_is "${SHA_NEW}" && case_ok "worker stays flipped (no auto flip-back)" || case_fail "worker flipped back"
restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker running after post-start signal" || case_ok "worker stopped by the handler"
assert_proc_alive "${SUP1}" "session 1 alive"
assert_proc_alive "${SUP2}" "session 2 alive"
teardown_case
case_end

# --- S. второй сигнал внутри обработчика: восстановление не обрывается ---
case_begin "second signal inside handler"
setup_cutover reentry
export FAKE_SYSTEMCTL_START_DELAY=1
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "stopping worker" || case_fail "no stop marker"
for _ in $(seq 1 600); do
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker || break
    sleep 0.1
done
kill -INT "${CUTOVER_PID}" || case_fail "first signal failed"
kill -TERM "${CUTOVER_PID}" || case_fail "second signal failed"
wait_for_log "${FAKE_ROOT}/cutover.log" "STOP before hub flip" || case_fail "handler did not start"
# Третий сигнал застаёт обработчик внутри fake systemctl start (реальная задержка).
kill -INT "${CUTOVER_PID}" 2>/dev/null || true
rc=0; wait "${CUTOVER_PID}" || rc=$?
unset FAKE_SYSTEMCTL_START_DELAY
[ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker still prev" || case_fail "worker moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "handler completed recovery despite signals" || case_fail "recovery was aborted"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
assert_not_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "no post-flip report on early path"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- T. сигнал после flip при activating/auto-restart: воркер всё равно остановлен ---
for state in activating auto-restart; do
    case_begin "signal during verify with worker ${state}"
    setup_cutover "verify-${state}"
    run_cutover_bg
    wait_for_log "${FAKE_ROOT}/cutover.log" "running verification" || case_fail "no verify marker"
    printf '%s' "${state}" >"${FAKE_ROOT}/state/unit-local-canterlot-worker.active"
    kill -TERM "${CUTOVER_PID}" || case_fail "kill failed"
    rc=0; wait "${CUTOVER_PID}" || rc=$?
    [ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
    hub_is "${SHA_NEW}" && case_ok "hub stays new" || case_fail "hub moved"
    worker_is "${SHA_NEW}" && case_ok "worker was on the new release" || case_fail "worker not new"
    STOP_STATE="$(FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" show canterlot-worker -p ActiveState --value)"
    STOP_PID="$(FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" show canterlot-worker -p MainPID --value)"
    [ "${STOP_STATE}" = "inactive" ] && case_ok "worker ActiveState inactive" || case_fail "worker state ${STOP_STATE}"
    [ "${STOP_PID}" = "0" ] && case_ok "worker has no MainPID" || case_fail "worker MainPID ${STOP_PID}"
    restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
    assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
    assert_contains "${FAKE_ROOT}/cutover.log" "worker verified stopped" "stop verified in the handler"
    assert_proc_alive "${SUP1}" "session 1 alive"
    teardown_case
    case_end
done

# --- V1. неучтённый `canterlot-worker run` переживает стоп юнита: отказ до бэкапов ---
case_begin "untracked control worker blocks cutover after unit stop"
setup_cutover orphanworker
# Управляющий воркер вне юнита: тот же бинарь, argv[1]=run, живёт после systemctl stop.
setsid "${WORKER_RELEASES}/${SHA_PREV}/bin/canterlot-worker" run >/dev/null 2>&1 <&- &
sleep 0.5
# $! ненадёжен: при включённом job control setsid форкается, и $! — уже мёртвая обёртка.
ORPHAN_PID="$(ps -eo pid=,args= | awk -v dir="${WORKER_RELEASES}/" 'index($2, dir) == 1 && $3 == "run" {print $1; exit}')"
[ -n "${ORPHAN_PID}" ] && case_ok "untracked worker is running" || case_fail "setup: orphan worker not found"
if run_cutover; then case_fail "exit 0 despite an untracked control worker"; else case_ok "exit nonzero"; fi
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
grep -q '^vzdump ' "${FAKE_ROOT}/state/calls.log" && case_fail "vzdump ran" || case_ok "vzdump never ran"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
assert_contains "${FAKE_ROOT}/cutover.log" "still runs" "orphan worker reported"
assert_proc_alive "${ORPHAN_PID}" "orphan worker not signalled by the script"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- V2. обрыв ssh при ещё идущем удалённом flip: чтение current ждёт lock ---
case_begin "transport loss during flip: readback waits for the outstanding flip"
setup_cutover flipdetach
export FAKE_SSH_DETACH_MATCH="mv -T current.new current" FAKE_MV_DELAY=2
if run_cutover; then case_fail "exit 0 despite the lost flip connection"; else case_ok "exit nonzero"; fi
unset FAKE_SSH_DETACH_MATCH FAKE_MV_DELAY
hub_is "${SHA_NEW}" && case_ok "the remote flip completed (current is new)" || case_fail "flip did not complete"
assert_not_contains "${FAKE_ROOT}/cutover.log" "provably unchanged" "old current was not trusted"
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "post-flip stop-and-report printed"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker restarted on the old release while current is new" || case_ok "worker stays stopped"
restore_logged && case_fail "database restore ran" || case_ok "no restore"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- V3a. retention между проверкой цели и flip не может удалить цель (резерв) ---
case_begin "retention during cutover cannot remove the reserved target"
setup_cutover reservetarget
touch -d '2020-01-01 00:00:00' "${HUB_RELEASES}/${SHA_NEW}"
cat >"${FAKE_ROOT}/retain-hook.sh" <<EOF
#!/usr/bin/env bash
[ ! -e "${FAKE_ROOT}/state/retain-ran" ] || exit 0
: >"${FAKE_ROOT}/state/retain-ran"
RETAIN_COUNT=1 bash "${DEPLOY_DIR}/retain.sh" --apply >"${FAKE_ROOT}/retain-hook.log" 2>&1
EOF
chmod +x "${FAKE_ROOT}/retain-hook.sh"
export FAKE_SSH_AFTER_CMD="SHA256SUMS" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/retain-hook.sh"
run_cutover && case_ok "cutover completed" || case_fail "cutover failed"
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
[ -e "${FAKE_ROOT}/state/retain-ran" ] && case_ok "the competing retention actually ran" || case_fail "retention hook never ran"
assert_contains "${FAKE_ROOT}/retain-hook.log" "reserved by a running operation" "retention saw the reservation"
[ -d "${HUB_RELEASES}/${SHA_NEW}" ] && case_ok "target release survived retention" || case_fail "target release was removed"
hub_is "${SHA_NEW}" && case_ok "hub flipped" || case_fail "hub not flipped"
ls "${HUB_BASE}"/.reserved.* >/dev/null 2>&1 && case_fail "reservations left after success" || case_ok "reservations released"
teardown_case
case_end

# --- V3b. цель исчезла между проверкой и flip: flip отказывает под lock, current не трогается ---
case_begin "target removed before the flip is refused inside the lock"
setup_cutover targetgone
cat >"${FAKE_ROOT}/rm-hook.sh" <<EOF
#!/usr/bin/env bash
[ ! -e "${FAKE_ROOT}/state/rm-ran" ] || exit 0
: >"${FAKE_ROOT}/state/rm-ran"
rm -rf "${HUB_RELEASES}/${SHA_NEW}"
EOF
chmod +x "${FAKE_ROOT}/rm-hook.sh"
export FAKE_SSH_AFTER_CMD="SHA256SUMS" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/rm-hook.sh"
if run_cutover; then case_fail "exit 0 with a vanished target"; else case_ok "exit nonzero"; fi
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
[ -e "${FAKE_ROOT}/state/rm-ran" ] && case_ok "the target was removed by the competitor" || case_fail "hook never ran"
hub_is "${SHA_PREV}" && case_ok "hub current untouched" || case_fail "hub current moved"
[ -e "$(readlink -m "${HUB_CURRENT}")" ] && case_ok "current resolves" || case_fail "current dangles"
[ ! -e "${HUB_BASE}/current.new" ] && case_ok "no leftover current.new" || case_fail "current.new left"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker restarted on its previous release" || case_fail "worker left stopped"
teardown_case
case_end

# --- V4. печатаемые команды восстановления берут общий lock и рабочие ---
case_begin "printed recovery commands take the host lock and work"
setup_cutover printedcmds
export FAKE_FAIL_CMD="readlink -m"
run_cutover && case_fail "exit 0 despite post-flip failure" || case_ok "exit nonzero"
unset FAKE_FAIL_CMD
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "report printed"
HUBCMD="$(grep -E '^ +ssh fakehub ' "${FAKE_ROOT}/cutover.log" | grep 'systemctl restart' | head -1 | sed 's/^ *//')"
WEBCMD="$(grep -E '^ +ssh fakeweb ' "${FAKE_ROOT}/cutover.log" | grep 'docker compose' | head -1 | sed 's/^ *//')"
WORKERCMD="$(grep -E '^ +flock ' "${FAKE_ROOT}/cutover.log" | grep 'canterlot-worker' | head -1 | sed 's/^ *//')"
[[ "${HUBCMD}" == *"flock"*"${HUB_BASE}/.deploy.lock"* ]] && case_ok "hub recovery takes the hub lock" || case_fail "hub recovery has no lock: [${HUBCMD}]"
[[ "${WEBCMD}" == *"flock"*"${WEB_SITES_BASE}/.deploy.lock"* ]] && case_ok "web recovery takes the web lock" || case_fail "web recovery has no lock: [${WEBCMD}]"
[[ "${WORKERCMD}" == *"${WORKER_BASE}/.deploy.lock"* ]] && case_ok "worker recovery takes the worker lock" || case_fail "worker recovery has no lock: [${WORKERCMD}]"
if [ -n "${HUBCMD}" ]; then
    FAKE_HOST=fakehub bash -c "${HUBCMD}" >"${FAKE_ROOT}/printed-hub.log" 2>&1 \
        && case_ok "printed hub command runs" || case_fail "printed hub command failed"
    hub_is "${SHA_PREV}" && case_ok "printed hub command flipped back" || case_fail "hub not flipped back"
fi
teardown_case
case_end

# --- U. HUP во время восстановления: обработчик не обрывается до старта воркера ---
case_begin "HUP during early recovery"
setup_cutover huprecovery
export FAKE_SYSTEMCTL_START_DELAY=2
: >"${FAKE_ROOT}/state/calls.log"
run_cutover_bg
wait_for_log "${FAKE_ROOT}/cutover.log" "stopping worker" || case_fail "no stop marker"
kill -INT "${CUTOVER_PID}" || case_fail "INT failed"
wait_for_log "${FAKE_ROOT}/cutover.log" "STOP before hub flip" || case_fail "recovery did not start"
# Дождаться, пока обработчик войдёт в медленный старт воркера, и оборвать сессию HUP'ом.
for _ in $(seq 1 400); do
    grep -q 'start canterlot-worker' "${FAKE_ROOT}/state/calls.log" 2>/dev/null && break
    sleep 0.02
done
kill -HUP "${CUTOVER_PID}" 2>/dev/null || true
rc=0; wait "${CUTOVER_PID}" || rc=$?
unset FAKE_SYSTEMCTL_START_DELAY
[ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker still prev" || case_fail "worker moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker restarted despite HUP" || case_fail "HUP aborted recovery"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
assert_not_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "no post-flip report on early path"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

printf 'cutover harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
