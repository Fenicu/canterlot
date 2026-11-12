#!/usr/bin/env bash
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
# Тесты verify.sh: каждый вид проверки считается жёстко, тики сверяются на обоих
# чтениях, без --state сверка идёт по собственному снимку (пустого PASS нет).
set -euo pipefail
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"
# shellcheck source-path=SCRIPTDIR
# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=../lib.sh
source "${DEPLOY_DIR}/lib.sh"

cargo() { echo "cargo $*"; return 0; }
export -f cargo
sleep() { case "${1:-}" in 1|60) command sleep 0.1;; *) command sleep "$@";; esac; }
export -f sleep

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

setup_verify() {
    setup_case "verify-$1"
    export SITE_CODES="meeting.example.com:200 git.example.com:200 caddy.example.com:302 gotify.example.com:200 immich.example.com:200 bookshelf.example.com:200 hub.example.com:200"
    EXEC1="$(new_exec_id)"; EXEC2="$(new_exec_id)"; export EXEC1 EXEC2
    write_configs
    deploy_release hub "${SHA_PREV}"
    deploy_release web "${SHA_PREV}"
    deploy_release worker "${SHA_PREV}"
    printf 'MIGR_MAX=40\nFAILED=0\npayload\n' >"${FAKE_ROOT}/state/db.txt"
    printf '%s\n%s\n' "${EXEC1}" "${EXEC2}" >"${FAKE_ROOT}/state/live-ids"
    printf 'aaaaaaaa-0000-4000-8000-000000000001\n' >"${FAKE_ROOT}/state/live-convs"
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-worker >/dev/null
    FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-hub >/dev/null
    spawn_session "${EXEC1}" "${SHA_PREV}" 100
    spawn_session "${EXEC2}" "${SHA_PREV}" 200
    mkdir -p "${WORKER_DATA}/cj"
    python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', 50) + b'\0'*4)" >"${WORKER_DATA}/cj/ack"
    write_ownership
    printf '{"version":"%s"}' "${SHA_PREV}" >"${FAKE_ROOT}/container/version.json"
    printf 'CSP-BODY' >"${FAKE_ROOT}/container/csp"
    printf 'CANTERLOT_CSP="CSP-BODY"\n' >"${WEB_ENV}"
    printf 'sha=%s\ndate=x\n' "${SHA_PREV}" >"${HUB_RELEASES}/${SHA_PREV}/RELEASE"
    printf 'migrations_last=0040_monthly_limits.sql\n' >>"${HUB_RELEASES}/${SHA_PREV}/RELEASE"
    printf 'sha=%s\ndate=x\nmigrations_last=0040_fake.sql\n' "${SHA_PREV}" >"${WORKER_CURRENT}/RELEASE"
    # dist развёрнутого релиза: без него сравнения миграций/CSP с dist не выполняются.
    mkdir -p "${FAKE_ROOT}/dist/${SHA_PREV}"
    printf 'sha=%s\ndate=x\nmigrations_last=0040_monthly_limits.sql\n' "${SHA_PREV}" >"${FAKE_ROOT}/dist/${SHA_PREV}/RELEASE"
    printf 'CSP-BODY' >"${FAKE_ROOT}/dist/${SHA_PREV}/csp.txt"
    mkdir -p "${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks"
    printf 'console.log(1)\n' >"${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js"
    for ext in br zst gz; do
        printf 'precompressed\n' >"${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js.${ext}"
    done
    SUP1="$(sup_pid_of "${EXEC1}")"; SUP2="$(sup_pid_of "${EXEC2}")"; export SUP1 SUP2
}

run_verify() {
    bash "${DEPLOY_DIR}/verify.sh" --dist "${FAKE_ROOT}/dist" "$@" >"${FAKE_ROOT}/verify.log" 2>&1
}

# V1: всё здорово — PASS, свой снимок тиков неизменен.
case_begin "verify happy"
setup_verify happy
if ! bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" --state "${FAKE_ROOT}/snap.state" >/dev/null 2>&1; then
    case_fail "preflight failed"
else
if run_verify "${SHA_PREV}" --state "${FAKE_ROOT}/snap.state"; then case_ok "exit 0"; else case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/verify.log"; fi
assert_contains "${FAKE_ROOT}/verify.log" "verify PASS" "PASS printed"
assert_contains "${FAKE_ROOT}/verify.log" "own tick snapshot unchanged (read 2)" "own snapshot compared"
fi
teardown_case
case_end

# V2: без --state — тоже PASS, но только по собственному снимку (не всухую).
case_begin "verify without state file"
setup_verify nostate
if run_verify "${SHA_PREV}"; then case_ok "exit 0"; else case_fail "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "no --state given" "no-state noted"
assert_contains "${FAKE_ROOT}/verify.log" "own tick snapshot unchanged (read 2)" "own snapshot compared"
teardown_case
case_end

# V3: подсунутый state с чужим тиком — жёстко.
case_begin "verify crafted bad tick"
setup_verify badtick
TICK="$(proc_ticks "${EXEC1}" | grep -v mcp-relay | head -1 | cut -d' ' -f2)"
PID="$(proc_ticks "${EXEC1}" | grep -v mcp-relay | head -1 | cut -d' ' -f1)"
printf '%s sup=%s:%s cli=none relay=none sup_exe=/x\n' "${EXEC1}" "${PID}" "$((TICK + 5000))" >"${FAKE_ROOT}/snap.state"
if run_verify "${SHA_PREV}" --state "${FAKE_ROOT}/snap.state"; then case_fail "exit 0 despite bad tick"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "tick changed" "tick change counted"
teardown_case
case_end

# V4: --state на несуществующий файл — жёстко.
case_begin "verify missing state file"
setup_verify missingstate
if run_verify "${SHA_PREV}" --state "${FAKE_ROOT}/nope.state"; then case_fail "exit 0 despite missing state"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "state file missing" "missing state counted"
teardown_case
case_end

# V5: readyz не ok — жёстко.
case_begin "verify readyz fail"
setup_verify readyzfail
export FAKE_READYZ_OK=0
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite readyz fail"; else case_ok "exit nonzero"; fi
unset FAKE_READYZ_OK
assert_contains "${FAKE_ROOT}/verify.log" "hub readyz not ok" "readyz failure counted"
teardown_case
case_end

# V6: failed миграции — жёстко.
case_begin "verify failed migrations"
setup_verify failedmigr
printf 'MIGR_MAX=40\nFAILED=2\npayload\n' >"${FAKE_ROOT}/state/db.txt"
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite failed migrations"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "failed migrations present" "failed migrations counted"
teardown_case
case_end

# V7: version.json чужой — жёстко.
case_begin "verify version mismatch"
setup_verify vermismatch
printf '{"version":"other"}' >"${FAKE_ROOT}/container/version.json"
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite version mismatch"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "pwa version.json is not" "version mismatch counted"
teardown_case
case_end

# V8: ошибки в журнале хаба — жёстко.
case_begin "verify journal errors"
setup_verify journalerr
export FAKE_JOURNAL_MIGRATION_ERROR=1
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite journal errors"; else case_ok "exit nonzero"; fi
unset FAKE_JOURNAL_MIGRATION_ERROR
assert_contains "${FAKE_ROOT}/verify.log" "hub migration_error in journal" "journal error counted"
teardown_case
case_end

# V9: процесс сессии умер между чтениями — жёстко (свой снимок).
case_begin "verify session gone between reads"
setup_verify gonesession
export VERIFY_GAP_RELEASE="${FAKE_ROOT}/state/gap-release"
bash "${DEPLOY_DIR}/verify.sh" "${SHA_PREV}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/verify.log" 2>&1 &
VPID=$!
for _ in $(seq 1 600); do grep -q "waiting 60s between reads" "${FAKE_ROOT}/verify.log" 2>/dev/null && break; sleep 0.1; done
CLI1="$(ps -o pid= --ppid "${SUP1}" 2>/dev/null | tr -s ' ' '\n' | grep -v '^$' || true)"
for cand in ${CLI1:-}; do
    case "$(tr '\0' ' ' <"/proc/${cand}/cmdline" 2>/dev/null)" in *mcp-relay*) continue;; *) kill "${cand}" 2>/dev/null || true;; esac
done
touch "${VERIFY_GAP_RELEASE}"; rc=0; wait "${VPID}" || rc=$?; unset VERIFY_GAP_RELEASE
[ "${rc}" != "0" ] && case_ok "exit nonzero" || case_fail "exit 0 despite gone session"
assert_contains "${FAKE_ROOT}/verify.log" "session process gone" "gone session counted"
teardown_case
case_end

# V10: код чужого сайта изменился — жёстко.
case_begin "verify site code"
setup_verify sitecode
export FAKE_SITE_FAIL=meeting.example.com
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite site code change"; else case_ok "exit nonzero"; fi
unset FAKE_SITE_FAIL
assert_contains "${FAKE_ROOT}/verify.log" "site meeting.example.com: 500, expected 200" "site code counted"
teardown_case
case_end

# V11: SHA не совпадает с задеплоенным — жёстко.
case_begin "verify wrong sha"
setup_verify wrongsha
if run_verify "ffffffffffff"; then case_fail "exit 0 despite sha mismatch"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "hub current is" "sha mismatch counted"
teardown_case
case_end

# V12: курсор SJ уменьшился между чтениями — жёстко.
case_begin "verify ack decreased"
setup_verify ackdec
export VERIFY_GAP_RELEASE="${FAKE_ROOT}/state/gap-release"
bash "${DEPLOY_DIR}/verify.sh" "${SHA_PREV}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/verify.log" 2>&1 &
VPID=$!
for _ in $(seq 1 600); do grep -q "waiting 60s between reads" "${FAKE_ROOT}/verify.log" 2>/dev/null && break; sleep 0.1; done
python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', 1) + b'\0'*4)" \
    >"${WORKER_DATA}/executions/${EXEC1}/sj/ack"
touch "${VERIFY_GAP_RELEASE}"; rc=0; wait "${VPID}" || rc=$?; unset VERIFY_GAP_RELEASE
[ "${rc}" != "0" ] && case_ok "exit nonzero" || case_fail "exit 0 despite ack decrease"
assert_contains "${FAKE_ROOT}/verify.log" "sj ack decreased" "ack decrease counted"
teardown_case
case_end

# V13: сессия чисто завершена владельцем между чтениями — мягкая пометка, не провал.
case_begin "verify owner-stopped session"
setup_verify ownerstop
export VERIFY_GAP_RELEASE="${FAKE_ROOT}/state/gap-release"
bash "${DEPLOY_DIR}/verify.sh" "${SHA_PREV}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/verify.log" 2>&1 &
VPID=$!
wait_for_read1() {
    for _ in $(seq 1 600); do
        grep -q "waiting 60s between reads" "${FAKE_ROOT}/verify.log" 2>/dev/null && return 0
        sleep 0.1
    done
    return 1
}
wait_for_read1 || case_fail "no read1 marker"
grep -v "^${EXEC2}$" "${FAKE_ROOT}/state/live-ids" >"${FAKE_ROOT}/state/live-ids.new" || true
mv "${FAKE_ROOT}/state/live-ids.new" "${FAKE_ROOT}/state/live-ids"
printf '%s stopped:user_stop\n' "${EXEC2}" >"${FAKE_ROOT}/state/ended-ids"
SUP2_PID="$(sup_pid_of "${EXEC2}")"
[ -n "${SUP2_PID}" ] && kill "${SUP2_PID}" 2>/dev/null || true
touch "${VERIFY_GAP_RELEASE}"; rc=0; wait "${VPID}" || rc=$?; unset VERIFY_GAP_RELEASE
[ "${rc}" = "0" ] && case_ok "exit 0" || case_fail "exit ${rc}"
assert_contains "${FAKE_ROOT}/verify.log" "session ended between reads" "owner stop reported as ended"
teardown_case
case_end

# V14: миграции не догнали ожидаемый максимум из RELEASE — жёстко.
case_begin "verify expected migration max"
setup_verify expectedmigr
mkdir -p "${FAKE_ROOT}/dist/${SHA_PREV}"
printf 'sha=%s\ndate=x\nmigrations_last=0041_expected.sql\n' "${SHA_PREV}" \
    >"${FAKE_ROOT}/dist/${SHA_PREV}/RELEASE"
if run_verify "${SHA_PREV}" --dist "${FAKE_ROOT}/dist"; then
    case_fail "exit 0 despite migration max behind RELEASE"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/verify.log" "migrations max 40, expected 41" "expected migration max compared"
teardown_case
case_end

# V15: оба чтения CSP упали — жёстко, а не «UNREADABLE == UNREADABLE».
case_begin "verify CSP reads fail"
setup_verify cspreadfail
export FAKE_FAIL_CSP_READ=1 FAKE_FAIL_CMD="canterlot.env"
if run_verify "${SHA_PREV}"; then
    case_fail "exit 0 when both CSP reads failed"
else
    case_ok "exit nonzero"
fi
unset FAKE_FAIL_CSP_READ FAKE_FAIL_CMD
assert_contains "${FAKE_ROOT}/verify.log" "cannot read web env CSP" "web env CSP read failure counted"
assert_contains "${FAKE_ROOT}/verify.log" "cannot read container CSP" "container CSP read failure counted"
teardown_case
case_end

# V16: провал запроса списка живых исполнений — жёстко, а не пустой цикл.
case_begin "verify live-execution query fails"
setup_verify livequeryfail
export FAKE_PSQL_FAIL_MATCH="select id::text from executions"
if run_verify "${SHA_PREV}"; then
    case_fail "exit 0 despite failed live-execution query"
else
    case_ok "exit nonzero"
fi
unset FAKE_PSQL_FAIL_MATCH
assert_contains "${FAKE_ROOT}/verify.log" "cannot list live executions" "query failure counted"
teardown_case
case_end

# V17: dist csp.txt не читается — жёстко, а не сравнение с пустотой.
case_begin "verify missing dist CSP"
setup_verify distcspmissing
mkdir -p "${FAKE_ROOT}/dist/${SHA_PREV}"
rm -f "${FAKE_ROOT}/dist/${SHA_PREV}/csp.txt"
printf 'sha=%s\ndate=x\nmigrations_last=0040_expected.sql\n' "${SHA_PREV}" \
    >"${FAKE_ROOT}/dist/${SHA_PREV}/RELEASE"
if run_verify "${SHA_PREV}" --dist "${FAKE_ROOT}/dist"; then
    case_fail "exit 0 with missing dist CSP"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/verify.log" "cannot read dist CSP" "dist CSP read failure counted"
teardown_case
case_end

# V18: релей убит между чтениями. Раньше его путь executions/... отсекался
# раньше ветки mcp-relay, релей не попадал в снимок и verify давал PASS.
case_begin "verify relay gone between reads"
setup_verify relaygone
export VERIFY_GAP_RELEASE="${FAKE_ROOT}/state/gap-release"
bash "${DEPLOY_DIR}/verify.sh" "${SHA_PREV}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/verify.log" 2>&1 &
VPID=$!
for _ in $(seq 1 600); do grep -q "waiting 60s between reads" "${FAKE_ROOT}/verify.log" 2>/dev/null && break; sleep 0.1; done
RELAY_PID=""
for d in /proc/[0-9]*; do
    cand="${d#/proc/}"
    if [ "$(proc_relay_exec_id "${cand}")" = "${EXEC1}" ]; then RELAY_PID="${cand}"; break; fi
done
if [ -n "${RELAY_PID}" ]; then kill "${RELAY_PID}" 2>/dev/null || true; else case_fail "no relay found for ${EXEC1}"; fi
touch "${VERIFY_GAP_RELEASE}"; rc=0; wait "${VPID}" || rc=$?; unset VERIFY_GAP_RELEASE
[ "${rc}" != "0" ] && case_ok "exit nonzero" || case_fail "exit 0 despite gone relay"
assert_contains "${FAKE_ROOT}/verify.log" "session process gone (read 2): ${EXEC1} relay" "gone relay counted"
teardown_case
case_end

# V20: настоящий формат журнала хаба: WARN/ERROR без level= (tracing fmt) считаются.
case_begin "verify counts real tracing WARN and ERROR lines of the hub"
setup_verify tracingformat
export FAKE_HUB_JOURNAL_LINES='Nov 03 12:00:05 canterlot-hub canterlot-hub[101]: 2026-11-03T12:00:05.000001Z  WARN session handshake slow kind="handshake_slow"'
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite a hub WARN line"; else case_ok "exit nonzero on WARN"; fi
assert_contains "${FAKE_ROOT}/verify.log" "new hub WARN kind not in the baseline: handshake_slow" "WARN counted"
export FAKE_HUB_JOURNAL_LINES='Nov 03 12:00:06 canterlot-hub canterlot-hub[101]: 2026-11-03T12:00:06.000001Z ERROR db_error code=57P01'
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite a hub ERROR line"; else case_ok "exit nonzero on ERROR"; fi
assert_contains "${FAKE_ROOT}/verify.log" "new hub ERROR kind not in the baseline" "ERROR counted"
# Слова WARN/ERROR внутри сообщения INFO — не уровень.
export FAKE_HUB_JOURNAL_LINES='Nov 03 12:00:07 canterlot-hub canterlot-hub[101]: 2026-11-03T12:00:07.000001Z  INFO retry policy mentions WARN and ERROR in text'
if run_verify "${SHA_PREV}"; then case_ok "INFO line mentioning WARN/ERROR is not counted"; else case_fail "INFO line counted as a level"; tail -3 "${FAKE_ROOT}/verify.log"; fi
unset FAKE_HUB_JOURNAL_LINES
teardown_case
case_end

# V21: воркер активен, но не соединился (журнал: только connecting/backoff) — жёстко.
case_begin "verify rejects an active worker that never connected"
setup_verify neverconnected
export FAKE_WORKER_JOURNAL_MODE=backoff
if run_verify "${SHA_PREV}"; then case_fail "exit 0 for a worker that never reached live"; else case_ok "exit nonzero"; fi
unset FAKE_WORKER_JOURNAL_MODE
assert_contains "${FAKE_ROOT}/verify.log" "no fresh worker connection" "missing connection evidence counted"
teardown_case
case_end

# V22: воркер пишет live, но хаб его не видит (reported_at старый или машина unavailable) — жёстко.
case_begin "verify rejects a worker the hub does not see"
setup_verify hubblind
export FAKE_MACHINE_ROW="available:86400"
if run_verify "${SHA_PREV}"; then case_fail "exit 0 with a stale reported_at"; else case_ok "exit nonzero on stale reported_at"; fi
assert_contains "${FAKE_ROOT}/verify.log" "hub does not see the worker" "stale report counted"
export FAKE_MACHINE_ROW="unavailable:1"
if run_verify "${SHA_PREV}"; then case_fail "exit 0 with an unavailable machine"; else case_ok "exit nonzero on unavailable machine"; fi
unset FAKE_MACHINE_ROW
teardown_case
case_end

# V23: журнал воркера прошлого запуска не засчитывается: evidence берётся с момента старта.
case_begin "verify takes connection evidence from the current invocation only"
setup_verify oldinvocation
export FAKE_WORKER_JOURNAL_MODE=silent
if run_verify "${SHA_PREV}"; then case_fail "exit 0 without any connection lines"; else case_ok "exit nonzero"; fi
unset FAKE_WORKER_JOURNAL_MODE
teardown_case
case_end

# V24: без локального dist ожидаемые миграции берутся из RELEASE развёрнутого
# релиза хаба: база на 999 при RELEASE 40 — жёстко (раньше сравнение молча пропадало).
case_begin "verify without dist derives expected migrations from the deployed RELEASE"
setup_verify nodistmigr
rm -rf "${FAKE_ROOT}/dist"
printf 'MIGR_MAX=999\nFAILED=0\npayload\n' >"${FAKE_ROOT}/state/db.txt"
if run_verify "${SHA_PREV}" --allow-skipped; then case_fail "exit 0 with DB at 999 and RELEASE at 40"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "migrations max 999, expected 40" "expected migration taken from the deployed RELEASE"
teardown_case
case_end

# V25: сравнение, которое нельзя выполнить (нет dist для CSP), — NOT RUN и провал,
# пока оно не разрешено явно.
case_begin "verify reports comparisons it could not run"
setup_verify notrun
rm -rf "${FAKE_ROOT}/dist"
if run_verify "${SHA_PREV}"; then case_fail "exit 0 although the dist CSP comparison could not run"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "NOT RUN: container CSP vs dist" "NOT RUN reported"
if run_verify "${SHA_PREV}" --allow-skipped; then case_ok "explicitly allowed: exit 0"; else case_fail "still failing with --allow-skipped"; tail -3 "${FAKE_ROOT}/verify.log"; fi
assert_contains "${FAKE_ROOT}/verify.log" "NOT RUN: container CSP vs dist" "NOT RUN still listed when allowed"
teardown_case
case_end

# V26: настоящий WARN-поток хаба из прода (machine_metrics_ignored execution_counter,
# ~1000 в сутки от старого хаба) не валит verify: известный вид при скорости базы
# проходит, и виды печатаются. Новый вид и рост скорости — жёстко.
mk_warn_stream() { # $1 файл, $2 число строк, $3 вид kind, $4 reason (необязательно)
    awk -v n="$2" -v kind="$3" -v reason="${4:-}" 'BEGIN {
        for (i = 0; i < n; i++) {
            printf "Nov 03 12:00:%02d canterlot-hub canterlot-hub[101]: 2026-11-03T12:00:%02d.%06dZ  WARN metrics ignored kind=\"%s\"", i % 60, i % 60, i, kind
            if (reason != "") printf " reason=\"%s\"", reason
            printf "\n"
        }
    }' >>"$1"
}
case_begin "verify tolerates the known pre-existing WARN stream"
setup_verify warnbase
mk_warn_stream "${FAKE_ROOT}/baseline.txt" 1037 machine_metrics_ignored execution_counter
mk_warn_stream "${FAKE_ROOT}/current.txt" 33 machine_metrics_ignored execution_counter
export FAKE_HUB_JOURNAL_BASELINE_FILE="${FAKE_ROOT}/baseline.txt" FAKE_HUB_JOURNAL_FILE="${FAKE_ROOT}/current.txt"
if run_verify "${SHA_PREV}"; then case_ok "exit 0 with 33 WARN of the known baseline kind"; else case_fail "known WARN stream failed verify"; grep -E 'HARD|WARN' "${FAKE_ROOT}/verify.log" | head -4; fi
assert_contains "${FAKE_ROOT}/verify.log" "machine_metrics_ignored/execution_counter" "kind printed"
assert_contains "${FAKE_ROOT}/verify.log" "within the baseline" "baseline comparison reported"
teardown_case
case_end

case_begin "verify fails on a new WARN kind next to the known stream"
setup_verify warnnew
mk_warn_stream "${FAKE_ROOT}/baseline.txt" 1037 machine_metrics_ignored execution_counter
mk_warn_stream "${FAKE_ROOT}/current.txt" 30 machine_metrics_ignored execution_counter
mk_warn_stream "${FAKE_ROOT}/current.txt" 1 handshake_slow
export FAKE_HUB_JOURNAL_BASELINE_FILE="${FAKE_ROOT}/baseline.txt" FAKE_HUB_JOURNAL_FILE="${FAKE_ROOT}/current.txt"
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite a new WARN kind"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "new hub WARN kind not in the baseline: handshake_slow" "new kind named"
teardown_case
case_end

case_begin "expected WARN kinds pass: deprecation by default, sign-in kinds by VERIFY_WARN_KINDS_OK"
setup_verify warnexpected
mk_warn_stream "${FAKE_ROOT}/baseline.txt" 1037 machine_metrics_ignored execution_counter
mk_warn_stream "${FAKE_ROOT}/current.txt" 30 machine_metrics_ignored execution_counter
mk_warn_stream "${FAKE_ROOT}/current.txt" 2 config_key_deprecated
export FAKE_HUB_JOURNAL_BASELINE_FILE="${FAKE_ROOT}/baseline.txt" FAKE_HUB_JOURNAL_FILE="${FAKE_ROOT}/current.txt"
if run_verify "${SHA_PREV}"; then case_ok "config_key_deprecated is not a new kind by default"; else case_fail "deprecation warning failed verify"; grep HARD "${FAKE_ROOT}/verify.log" | head -3; fi
assert_contains "${FAKE_ROOT}/verify.log" "2 WARN config_key_deprecated (expected, VERIFY_WARN_KINDS_OK)" "expected kind printed"
mk_warn_stream "${FAKE_ROOT}/current.txt" 4 identity_header_forwarded
if run_verify "${SHA_PREV}"; then case_fail "identity_header_forwarded passed without being listed"; else case_ok "identity_header_forwarded is new unless listed"; fi
if VERIFY_WARN_KINDS_OK="config_key_deprecated client_ip_missing identity_header_forwarded" run_verify "${SHA_PREV}"; then
    case_ok "first-release kinds pass when listed"
else
    case_fail "listed kinds failed verify"; grep HARD "${FAKE_ROOT}/verify.log" | head -3
fi
teardown_case
case_end

case_begin "sign-in WARN kinds from the internet pass by default"
setup_verify warnsignin
mk_warn_stream "${FAKE_ROOT}/baseline.txt" 1037 machine_metrics_ignored execution_counter
mk_warn_stream "${FAKE_ROOT}/current.txt" 30 machine_metrics_ignored execution_counter
mk_warn_stream "${FAKE_ROOT}/current.txt" 1 sign_in_global_lock
mk_warn_stream "${FAKE_ROOT}/current.txt" 3 client_ip_missing
export FAKE_HUB_JOURNAL_BASELINE_FILE="${FAKE_ROOT}/baseline.txt" FAKE_HUB_JOURNAL_FILE="${FAKE_ROOT}/current.txt"
if run_verify "${SHA_PREV}"; then case_ok "sign_in_global_lock and client_ip_missing are expected by default"; else case_fail "sign-in WARN kinds failed verify"; grep HARD "${FAKE_ROOT}/verify.log" | head -3; fi
assert_contains "${FAKE_ROOT}/verify.log" "1 WARN sign_in_global_lock (expected, VERIFY_WARN_KINDS_OK)" "global lock printed as expected"
assert_contains "${FAKE_ROOT}/verify.log" "3 WARN client_ip_missing (expected, VERIFY_WARN_KINDS_OK)" "client_ip_missing printed as expected"
mk_warn_stream "${FAKE_ROOT}/current.txt" 1 sign_in_failed
if run_verify "${SHA_PREV}"; then case_fail "an unexpected WARN kind passed"; else case_ok "other new kinds still fail"; fi
teardown_case
case_end

case_begin "verify fails when the known WARN stream grows"
setup_verify warnrate
mk_warn_stream "${FAKE_ROOT}/baseline.txt" 1037 machine_metrics_ignored execution_counter
mk_warn_stream "${FAKE_ROOT}/current.txt" 400 machine_metrics_ignored execution_counter
export FAKE_HUB_JOURNAL_BASELINE_FILE="${FAKE_ROOT}/baseline.txt" FAKE_HUB_JOURNAL_FILE="${FAKE_ROOT}/current.txt"
if run_verify "${SHA_PREV}"; then case_fail "exit 0 despite a WARN rate increase"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "hub WARN rate increase: machine_metrics_ignored/execution_counter" "rate increase named"
teardown_case
case_end

case_begin "verify without any baseline keeps the zero rule and unreadable baseline fails"
setup_verify warnnobase
mk_warn_stream "${FAKE_ROOT}/current.txt" 2 machine_metrics_ignored execution_counter
export FAKE_HUB_JOURNAL_FILE="${FAKE_ROOT}/current.txt"
if run_verify "${SHA_PREV}"; then case_fail "exit 0 with WARN and empty baseline"; else case_ok "exit nonzero with empty baseline"; fi
assert_contains "${FAKE_ROOT}/verify.log" "new hub WARN kind not in the baseline" "empty baseline = zero rule"
teardown_case
case_end

unset FAKE_HUB_JOURNAL_BASELINE_FILE FAKE_HUB_JOURNAL_FILE

# V27: транзиентные дочерние процессы инструментов (не супервизор, не CLI, не релей)
# не входят в снимок сессии: их исчезновение не «session process gone».
case_begin "verify ignores transient tool children of concurrent sessions"
export SESSION_TOOL_CHURN=1
setup_verify toolchurn
unset SESSION_TOOL_CHURN
if run_verify "${SHA_PREV}"; then case_ok "exit 0 while tool children come and go"; else case_fail "tool children failed verify"; grep -E 'HARD' "${FAKE_ROOT}/verify.log" | head -4; fi
assert_not_contains "${FAKE_ROOT}/verify.log" "session process gone" "no process-gone for tool children"
# роли снимка точные: ровно supervisor, CLI и relay на сессию
teardown_case
case_end

case_begin "verify still fails when the real CLI of a session disappears"
export SESSION_TOOL_CHURN=1
setup_verify clikilled
unset SESSION_TOOL_CHURN
bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" --state "${FAKE_ROOT}/snap.state" >/dev/null 2>&1 || case_fail "preflight failed"
CLI_PID="$(grep "^${EXEC1} " "${FAKE_ROOT}/snap.state" | grep -o ' cli=[0-9]*' | cut -d= -f2)"
[ -n "${CLI_PID}" ] && case_ok "snapshot recorded a CLI pid" || case_fail "snapshot has no CLI pid"
case "$(tr '\0' ' ' <"/proc/${CLI_PID}/cmdline")" in
    "opencode --session ${EXEC1}"*) case_ok "the recorded CLI is the agent process, not a tool child" ;;
    *) case_fail "recorded CLI ${CLI_PID} is not the agent process" ;;
esac
kill -KILL "${CLI_PID}" 2>/dev/null || true
command sleep 0.3
if run_verify "${SHA_PREV}" --state "${FAKE_ROOT}/snap.state"; then case_fail "exit 0 although the CLI died"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/verify.log" "session process gone" "dead CLI still detected"
teardown_case
case_end

# V28: сессия нормально завершилась между чтениями (строка хаба терминальная, исход
# normal_exit/user_stop, каталог исполнения убран): это «ended», а не провал, и
# перечитывание ack не падает. Неожиданное исчезновение живой строки — по-прежнему жёстко.
end_session_between_reads() { # $1 exec id, $2 "state:outcome" (пусто — строка остаётся running)
    local exec_id="$1" row="$2"
    wait_for_read1 || case_fail "no read1 marker"
    if [ -n "${row}" ]; then
        grep -v "^${exec_id}$" "${FAKE_ROOT}/state/live-ids" >"${FAKE_ROOT}/state/live-ids.new" || true
        mv "${FAKE_ROOT}/state/live-ids.new" "${FAKE_ROOT}/state/live-ids"
        printf '%s %s\n' "${exec_id}" "${row}" >"${FAKE_ROOT}/state/ended-ids"
    fi
    local sup
    sup="$(sup_pid_of "${exec_id}")"
    kill_session "${exec_id}"
    [ -z "${sup}" ] || command sleep 0.2
    rm -rf "${WORKER_DATA}/executions/${exec_id}"
}
for scenario in "stopped:user_stop" "stopped:normal_exit"; do
    case_begin "verify reports a session ended between reads (${scenario}) and does not fail"
    setup_verify "ended-${scenario//:/-}"
    export VERIFY_GAP_RELEASE="${FAKE_ROOT}/state/gap-release"
    bash "${DEPLOY_DIR}/verify.sh" "${SHA_PREV}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/verify.log" 2>&1 &
    VPID=$!
    end_session_between_reads "${EXEC2}" "${scenario}"
    touch "${VERIFY_GAP_RELEASE}"; rc=0; wait "${VPID}" || rc=$?; unset VERIFY_GAP_RELEASE
    [ "${rc}" = "0" ] && case_ok "exit 0" || { case_fail "exit ${rc}"; grep HARD "${FAKE_ROOT}/verify.log" | head -3; }
    assert_contains "${FAKE_ROOT}/verify.log" "session ended between reads: ${EXEC2}" "reported as ended"
    assert_not_contains "${FAKE_ROOT}/verify.log" "cannot re-read sj ack" "ack re-read is not a failure"
    teardown_case
    case_end
done

case_begin "verify fails on a crashed session and on a row that is still running"
setup_verify crashed
export VERIFY_GAP_RELEASE="${FAKE_ROOT}/state/gap-release"
bash "${DEPLOY_DIR}/verify.sh" "${SHA_PREV}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/verify.log" 2>&1 &
VPID=$!
end_session_between_reads "${EXEC2}" "failed:crashed"
touch "${VERIFY_GAP_RELEASE}"; rc=0; wait "${VPID}" || rc=$?; unset VERIFY_GAP_RELEASE
[ "${rc}" != "0" ] && case_ok "crashed: exit nonzero" || case_fail "crashed session passed"
assert_contains "${FAKE_ROOT}/verify.log" "session process gone" "crash is a process-gone failure"
teardown_case
setup_verify stillrunning
export VERIFY_GAP_RELEASE="${FAKE_ROOT}/state/gap-release"
bash "${DEPLOY_DIR}/verify.sh" "${SHA_PREV}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/verify.log" 2>&1 &
VPID=$!
end_session_between_reads "${EXEC2}" ""
touch "${VERIFY_GAP_RELEASE}"; rc=0; wait "${VPID}" || rc=$?; unset VERIFY_GAP_RELEASE
[ "${rc}" != "0" ] && case_ok "still-running row: exit nonzero" || case_fail "vanished running session passed"
assert_contains "${FAKE_ROOT}/verify.log" "session process gone" "unexpected disappearance fails"
teardown_case
case_end

# --- EXPECT релиза (решение владельца 4): требования — из dist, не из toolkit ---
write_expect() {
    printf 'format=1\ncapabilities=%s\nharnesses=%s\n' "$1" "$2" >"${FAKE_ROOT}/dist/${SHA_PREV}/EXPECT"
}
case_begin "verify takes required capabilities and harnesses from the release EXPECT"
setup_verify expect
write_expect "claude_histories_v1 path_probe_v1 pilot_v1" "claude_code codex opencode antigravity"
printf '\n[claude_code]\n[codex]\n[opencode]\n[antigravity]\nenabled = false\n' >>"${WORKER_CONFIG}"
if run_verify; then case_ok "verify PASS"; else case_fail "verify failed"; grep -E 'HARD|FATAL' "${FAKE_ROOT}/verify.log" | head -5; fi
assert_contains "${FAKE_ROOT}/verify.log" "expectations from the release ${FAKE_ROOT}/dist/${SHA_PREV}/EXPECT" "EXPECT used"
assert_contains "${FAKE_ROOT}/verify.log" "enabled in worker.toml: claude_code codex opencode" "a harness disabled in worker.toml is not required"
assert_contains "${FAKE_ROOT}/verify.log" "capabilities present: claude_histories_v1, path_probe_v1, pilot_v1 (source: ${FAKE_ROOT}/dist/${SHA_PREV}/EXPECT)" "capabilities from EXPECT"
assert_not_contains "${FAKE_ROOT}/verify.log" "WARNING: no " "no fallback warning"
teardown_case
case_end

case_begin "verify fails on a capability the release declares and the machine lacks"
setup_verify expect-missing
write_expect "claude_histories_v1 path_probe_v1 brand_new_v1" "claude_code"
printf '\n[claude_code]\n' >>"${WORKER_CONFIG}"
if run_verify; then case_fail "verify passed without brand_new_v1"; else case_ok "verify failed"; fi
assert_contains "${FAKE_ROOT}/verify.log" "capability check: machine lacks brand_new_v1" "missing capability named"
teardown_case
case_end

case_begin "a release that drops a harness needs no toolkit change"
setup_verify expect-dropped
write_expect "claude_histories_v1" "claude_code codex"
printf '\n[claude_code]\n[codex]\n[opencode]\n' >>"${WORKER_CONFIG}"
export FAKE_HARNESSES="claude_code,codex"
if run_verify; then case_ok "verify PASS without opencode"; else case_fail "verify failed"; grep -E 'HARD' "${FAKE_ROOT}/verify.log" | head -3; fi
unset FAKE_HARNESSES
assert_not_contains "${FAKE_ROOT}/verify.log" "machine lacks opencode" "opencode not required"
teardown_case
case_end

case_begin "verify fails on an enabled harness missing from the catalog"
setup_verify expect-harness
write_expect "claude_histories_v1" "claude_code codex opencode antigravity"
printf '\n[claude_code]\n[codex]\n[opencode]\n[antigravity]\n' >>"${WORKER_CONFIG}"
if run_verify; then case_fail "verify passed without antigravity"; else case_ok "verify failed"; fi
assert_contains "${FAKE_ROOT}/verify.log" "harness catalog: machine lacks antigravity" "enabled harness required"
teardown_case
case_end

case_begin "verify without EXPECT falls back to the built-in list with a warning"
setup_verify expect-absent
if run_verify; then case_ok "verify PASS"; else case_fail "verify failed"; fi
assert_contains "${FAKE_ROOT}/verify.log" "WARNING: no ${FAKE_ROOT}/dist/${SHA_PREV}/EXPECT" "fallback warned"
assert_contains "${FAKE_ROOT}/verify.log" "capabilities present: claude_histories_v1, path_probe_v1 (source: fallback)" "fallback list used"
teardown_case
case_end

case_begin "verify refuses an EXPECT of an unknown format"
setup_verify expect-format
printf 'format=2\ncapabilities=x\n' >"${FAKE_ROOT}/dist/${SHA_PREV}/EXPECT"
if run_verify; then case_fail "verify accepted format=2"; else case_ok "verify refused"; fi
assert_contains "${FAKE_ROOT}/verify.log" "unsupported" "format reported"
teardown_case
case_end

case_begin "the harness catalog may appear a few seconds after the connection"
setup_verify catalog-late
n_file="${FAKE_ROOT}/state/catalog-polls"
printf '0' >"${n_file}"
# Первые два чтения каталога пустые, потом полный (счётчик в файле: psql — отдельный процесс).
mv "${FAKE_ROOT}/fake-bin/psql" "${FAKE_ROOT}/fake-bin/psql.real"
cat >"${FAKE_ROOT}/fake-bin/psql" <<EOF
#!/usr/bin/env bash
case "\${*: -1}" in
    *"from machine_harnesses"*)
        n=\$(cat "${n_file}"); n=\$((n + 1)); printf '%s' "\${n}" >"${n_file}"
        if [ "\${n}" -le 2 ]; then printf ''; exit 0; fi ;;
esac
exec "${FAKE_ROOT}/fake-bin/psql.real" "\$@"
EOF
chmod +x "${FAKE_ROOT}/fake-bin/psql"
if run_verify; then case_ok "verify PASS after the catalog filled in"; else case_fail "verify failed"; grep HARD "${FAKE_ROOT}/verify.log" | head -3; fi
teardown_case
case_end

# --- loopback-проба Caddy (решение владельца 3): старт, останов, порт закрыт ---
case_begin "the loopback Caddy probe logs its start and stop and proves the port closed"
setup_verify probe
if run_verify; then case_ok "verify PASS"; else case_fail "verify failed"; fi
assert_contains "${FAKE_ROOT}/verify.log" "precompressed probe: starting a one-off loopback Caddy" "start announced"
assert_contains "${FAKE_ROOT}/verify.log" "probe: loopback caddy pid 4242 stopped" "stop logged"
assert_contains "${FAKE_ROOT}/verify.log" "port 18080 closed again" "port closed proven"
grep -q 'trap cleanup EXIT' "${FAKE_ROOT}/state/probe-script.sh" && grep -q 'trap "exit 1" INT TERM HUP' "${FAKE_ROOT}/state/probe-script.sh" \
    && case_ok "probe script traps EXIT and INT/TERM/HUP" || case_fail "probe script has no trap"
# shellcheck disable=SC2016 # литеральный текст скрипта пробы
grep -q 'timeout "$LIFETIME" caddy run' "${FAKE_ROOT}/state/probe-script.sh" && case_ok "probe Caddy runs under timeout" || case_fail "no timeout"
grep -q 'bind 127.0.0.1' "${FAKE_ROOT}/state/probe-script.sh" && case_ok "probe listens on loopback only" || case_fail "probe not bound to loopback"
teardown_case
case_end

for scenario in busy no-stop port-open; do
    case_begin "the loopback Caddy probe reports problems softly: ${scenario}"
    setup_verify "probe-${scenario}"
    case "${scenario}" in
        busy) export FAKE_PROBE_BUSY=1; want="SOFT-FAIL: precompressed check: probe failed" ;;
        no-stop) export FAKE_PROBE_NO_STOP=1; want="SOFT-FAIL: precompressed check: the loopback Caddy did not report its stop" ;;
        port-open) export FAKE_PROBE_PORT_OPEN=1; want="SOFT-FAIL: precompressed check: port 18080 still answers" ;;
    esac
    if run_verify; then case_ok "verify PASS (soft)"; else case_fail "verify failed (must be soft)"; fi
    unset FAKE_PROBE_BUSY FAKE_PROBE_NO_STOP FAKE_PROBE_PORT_OPEN
    assert_contains "${FAKE_ROOT}/verify.log" "${want}" "${scenario} reported softly"
    assert_not_contains "${FAKE_ROOT}/verify.log" "HARD-FAIL" "${scenario} not hard"
    teardown_case
    case_end
done

case_begin "the precompressed probe skips a dash-named JS asset that has no compressed siblings"
setup_verify probe-asset-pick
rm -f "${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js.br" \
      "${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js.zst" \
      "${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js.gz"
printf 'tiny\n' >"${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/--a.js"
printf 'console.log(2)\n' >"${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/z.js"
for ext in br zst gz; do
    printf 'precompressed\n' >"${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/z.js.${ext}"
done
if run_verify; then case_ok "verify PASS"; else case_fail "verify failed"; grep HARD "${FAKE_ROOT}/verify.log" | head -3; fi
assert_eq "$(cat "${FAKE_ROOT}/state/probe-asset" 2>/dev/null)" "_app/immutable/chunks/z.js" "probe targeted the first fully precompressed asset"
assert_contains "${FAKE_ROOT}/verify.log" "on _app/immutable/chunks/z.js" "encoding logged for the chosen asset"
assert_not_contains "${FAKE_ROOT}/verify.log" "chunks/--a.js" "asset without siblings never probed"
teardown_case
case_end

case_begin "the precompressed probe reports a missing fully precompressed asset softly"
setup_verify probe-asset-missing
rm -f "${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js.br" \
      "${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js.zst" \
      "${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js.gz"
if run_verify; then case_ok "verify PASS (soft)"; else case_fail "verify failed (must be soft)"; fi
assert_contains "${FAKE_ROOT}/verify.log" "SOFT-FAIL: precompressed check: no JS asset with br/zstd/gzip siblings in release ${SHA_PREV}" "missing precompressed asset reported softly"
assert_not_contains "${FAKE_ROOT}/verify.log" "HARD-FAIL" "no hard failure"
teardown_case
case_end

case_begin "the precompressed probe reports a missing encoding softly"
setup_verify probe-encoding-soft
export FAKE_ENCODING_FAIL=br
if run_verify; then case_ok "verify PASS (soft)"; else case_fail "verify failed (must be soft)"; fi
unset FAKE_ENCODING_FAIL
assert_contains "${FAKE_ROOT}/verify.log" "SOFT-FAIL: precompressed br: got 'none'" "missing encoding reported softly"
assert_not_contains "${FAKE_ROOT}/verify.log" "HARD-FAIL" "no hard failure"
teardown_case
case_end

printf 'verify harness: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
