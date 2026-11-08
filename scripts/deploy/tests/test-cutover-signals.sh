#!/usr/bin/env bash
# Тесты cutover.sh (сигналы и гонки) под новую политику: никакого автоматического отката БД и хаба.
# До flip: любая неудача/сигнал — рестарт воркера на его предыдущем релизе.
# С flip: СТОП и доклад, без действий (воркер остановлен и безопасен).
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

# shellcheck source-path=SCRIPTDIR
# shellcheck source=cutover-fixture.sh
source "${HARNESS_DIR}/cutover-fixture.sh"

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
# STOP-AND-REPORT до доказанного живого воркера не снимает резервы: они защищают
# цель и предыдущий релиз для ручного отката. Снимает их следующий успешный прогон,
# когда они станут старше 2 ч (см. retain).
for hostbase in "${HUB_BASE}" "${WEB_SITES_BASE}" "${WORKER_BASE}"; do
    if [ -f "${hostbase}/.reserved.${SHA_NEW}" ] && [ -f "${hostbase}/.reserved.${SHA_PREV}" ]; then
        case_ok "reservations kept after STOP-AND-REPORT (${hostbase##*/})"
    else
        case_fail "reservations released after STOP-AND-REPORT (${hostbase##*/})"
    fi
done
grep -F -- "pid=" "${HUB_BASE}/.reserved.${SHA_NEW}" >/dev/null \
    && case_ok "the kept marker records its run pid" || case_fail "kept marker has no run pid"
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

# --- Q. сигнал во время verify: СТОП-и-доклад, воркер остаётся работать ---
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
assert_contains "${FAKE_ROOT}/cutover.log" "leaving hub, PWA and worker running" "worker left running after proof"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker stays running" || case_fail "worker was stopped after it was proven live"
assert_proc_alive "${SUP1}" "session 1 alive"
assert_proc_alive "${SUP2}" "session 2 alive"
teardown_case

case_end

# --- R. TERM после старта воркера: СТОП-и-доклад, воркер остаётся работать ---
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
assert_contains "${FAKE_ROOT}/cutover.log" "leaving hub, PWA and worker running" "worker left running after proof"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker stays running" || case_fail "worker was stopped after it was proven live"
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

# --- T. сигнал после flip при activating/auto-restart: воркер не гасим ---
for state in activating auto-restart; do
    case_begin "signal during verify keeps the worker unit ${state}"
    setup_cutover "verify-${state}"
    run_cutover_bg
    wait_for_log "${FAKE_ROOT}/cutover.log" "running verification" || case_fail "no verify marker"
    printf '%s' "${state}" >"${FAKE_ROOT}/state/unit-local-canterlot-worker.active"
    kill -TERM "${CUTOVER_PID}" || case_fail "kill failed"
    rc=0; wait "${CUTOVER_PID}" || rc=$?
    [ "${rc}" = "1" ] && case_ok "exit 1" || case_fail "exit ${rc}"
    hub_is "${SHA_NEW}" && case_ok "hub stays new" || case_fail "hub moved"
    worker_is "${SHA_NEW}" && case_ok "worker was on the new release" || case_fail "worker not new"
    STOP_STATE="$(cat "${FAKE_ROOT}/state/unit-local-canterlot-worker.active" 2>/dev/null || echo none)"
    STOP_PID="$(FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" show canterlot-worker -p MainPID --value)"
    [ "${STOP_STATE}" = "${state}" ] && case_ok "worker unit left as it was (${state})" || case_fail "worker state ${STOP_STATE}"
    [ "${STOP_PID}" != "0" ] && case_ok "worker keeps its MainPID ${STOP_PID}" || case_fail "worker lost its MainPID"
    restore_logged && case_fail "database restore ran" || case_ok "no dropdb/createdb/pg_restore restore"
    assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "stop-and-report printed"
    assert_contains "${FAKE_ROOT}/cutover.log" "leaving hub, PWA and worker running" "worker left running after proof"
    assert_not_contains "${FAKE_ROOT}/cutover.log" "worker verified stopped" "the handler did not stop the worker"
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

printf 'cutover-signals harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
