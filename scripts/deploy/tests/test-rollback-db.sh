#!/usr/bin/env bash
# Тесты rollback-db.sh: ручной откат базы. Никогда не вызывается из cutover.sh.
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

setup_rollback() {
    setup_case "rollback-$1"
    write_configs
    deploy_release hub "${SHA_PREV}"
    deploy_release hub "${SHA_NEW}"
    ln -sfn "releases/${SHA_NEW}" "${HUB_CURRENT}"
    deploy_release worker "${SHA_PREV}"
    export FAKE_READYZ_MODE=byhub FAKE_READYZ_NEW_SHA="${SHA_NEW}"
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-worker >/dev/null
    FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-hub >/dev/null
    printf 'MIGR_MAX=41\nFAILED=0\npayload-new\n' >"${FAKE_ROOT}/state/db.txt"
    # Дамп этого прогона (как после pg_dump до flip): TOC + состояние 40.
    printf 'TOC-LINE-01\nTOC-LINE-02 _sqlx_migrations table\nTOC-LINE-03\nTOC-LINE-04\nTOC-LINE-05\n' >"${FAKE_ROOT}/hub/backups/can-pre.dump"
    printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >>"${FAKE_ROOT}/hub/backups/can-pre.dump"
}

# R1: активный воркер — отказ до каких-либо действий.
case_begin "rollback refuses with active worker"
setup_rollback active
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with active worker"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "stop it first" "refusal logged"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R2: счастливый путь — печатанная команда запускается напрямую (режим +x),
# restore, старт старого хаба, воркер не тронут.
case_begin "rollback happy path"
setup_rollback happy
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
if ( cd "${REPO_ROOT}" && scripts/deploy/rollback-db.sh \
        "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" ) >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/rb.log"
fi
[ "$(readlink "${HUB_CURRENT}" | sed 's|.*/||')" = "${SHA_PREV}" ] && case_ok "hub flipped back" || case_fail "hub not flipped back"
diff "${FAKE_ROOT}/hub/backups/can-pre.dump" "${FAKE_ROOT}/state/db.txt" >/dev/null \
    && case_ok "db restored from dump" || case_fail "db differs from dump"
FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-hub \
    && case_ok "old hub active" || case_fail "hub not active"
assert_contains "${FAKE_ROOT}/rb.log" "old hub readyz ok" "readyz loudly ok"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker started (must stay stopped)" || case_ok "worker untouched"
teardown_case
case_end

# R3: упавший pg_restore — ничего не стартует, громко.
case_begin "rollback refuses to start after failed restore"
setup_rollback restorefail
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
export FAKE_FAIL_PG_RESTORE=1
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 despite failed restore"
else
    case_ok "exit nonzero"
fi
unset FAKE_FAIL_PG_RESTORE
assert_contains "${FAKE_ROOT}/rb.log" "refusing to start anything" "refusal logged"
grep -q "systemctl fakehub start canterlot-hub" "${FAKE_ROOT}/state/calls.log" \
    && case_fail "hub started on partial DB" || case_ok "hub not started"
teardown_case
case_end

# R4: нет дампа — ранний отказ, хаб не тронут.
case_begin "rollback missing dump"
setup_rollback nodigump
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/no-such.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with missing dump"
else
    case_ok "exit nonzero"
fi
FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-hub \
    && case_ok "hub still active" || case_fail "hub touched"
teardown_case
case_end

# R5/R6: activating и auto-restart — не «остановлен», отказ до действий.
for state in activating auto-restart; do
    case_begin "rollback refuses worker ${state}"
    setup_rollback "state-${state}"
    printf '%s' "${state}" >"${FAKE_ROOT}/state/unit-local-canterlot-worker.active"
    : >"${FAKE_ROOT}/state/calls.log"
    if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
        case_fail "exit 0 with worker ${state}"
    else
        case_ok "exit nonzero"
    fi
    grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
    grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
    assert_contains "${FAKE_ROOT}/rb.log" "activating/" "state refusal logged"
    teardown_case
    case_end
done

# R7: запрос состояния юнита упал — отказ, а не «наверное остановлен».
case_begin "rollback refuses when unit state is unreadable"
setup_rollback showfail
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
export FAKE_FAIL_SYSTEMCTL_SHOW=1
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 when systemctl show failed"
else
    case_ok "exit nonzero"
fi
unset FAKE_FAIL_SYSTEMCTL_SHOW
assert_contains "${FAKE_ROOT}/rb.log" "cannot query canterlot-worker state" "show failure logged"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R8: юнит неактивен, но процесс из бинаря воркера жив — отказ.
case_begin "rollback refuses orphan worker process"
setup_rollback orphanproc
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
setsid "${WORKER_RELEASES}/${SHA_PREV}/bin/canterlot-worker" 1000 >/dev/null 2>&1 <&- &
HARNESS_PIDS+=("$!")
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with a live worker-binary process"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "still runs" "orphan process refusal logged"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R9: юнит неактивен, MainPID остался от умершего процесса — это «не жив»,
# откат разрешён (иначе застрявший pid-файл блокировал бы ручное восстановление).
case_begin "rollback allows a stale dead MainPID"
setup_rollback stalepid
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
bash -c 'sleep 1000 & pid=$!; kill "${pid}"; wait "${pid}" 2>/dev/null || true; printf "%s" "${pid}"' \
    >"${FAKE_ROOT}/state/unit-local-canterlot-worker.pid"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "exit 0 with a dead MainPID"
else
    case_fail "exit nonzero with a dead MainPID"; tail -3 "${FAKE_ROOT}/rb.log"
fi
assert_contains "${FAKE_ROOT}/rb.log" "is not alive; treating the worker as stopped" "dead MainPID reported"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_ok "restore proceeded" || case_fail "restore did not run"
teardown_case
case_end

# R10: юнит неактивен, но MainPID указывает на живой процесс — отказ.
case_begin "rollback refuses a live MainPID"
setup_rollback livepid
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
cp /bin/sleep "${FAKE_ROOT}/fake-bin/live-mainpid"
chmod +x "${FAKE_ROOT}/fake-bin/live-mainpid"
setsid "${FAKE_ROOT}/fake-bin/live-mainpid" 1000 >/dev/null 2>&1 <&- &
LIVE_PID=$!
HARNESS_PIDS+=("${LIVE_PID}")
printf '%s' "${LIVE_PID}" >"${FAKE_ROOT}/state/unit-local-canterlot-worker.pid"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with a live MainPID"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "still has live MainPID" "live MainPID refusal logged"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R11: argv[1]=run — это воркер, даже если в аргументах есть слово supervise.
case_begin "rollback refuses orphan run with supervise in its path"
setup_rollback orphanrun
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
setsid "${WORKER_RELEASES}/${SHA_PREV}/bin/canterlot-worker" \
    run --config /tmp/supervise/worker.toml >/dev/null 2>&1 <&- &
RUN_PID=$!
HARNESS_PIDS+=("${RUN_PID}")
for _ in $(seq 1 50); do kill -0 "${RUN_PID}" 2>/dev/null && break; sleep 0.02; done
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with an orphan run process"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "argv[1]=run" "run subcommand reported"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R12: supervise и mcp-relay из того же бинаря — штатные процессы сессий,
# откат не блокируют (production relay запускает current_exe воркера).
case_begin "rollback allows supervise and mcp-relay processes"
setup_rollback sessionprocs
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
BIN="${WORKER_RELEASES}/${SHA_PREV}/bin/canterlot-worker"
setsid "${BIN}" supervise --exec-dir /tmp/fake-exec >/dev/null 2>&1 <&- &
SUP_X=$!
HARNESS_PIDS+=("${SUP_X}")
setsid "${BIN}" mcp-relay --socket /tmp/fake-relay.sock >/dev/null 2>&1 <&- &
RELAY_X=$!
HARNESS_PIDS+=("${RELAY_X}")
for _ in $(seq 1 50); do
    kill -0 "${SUP_X}" 2>/dev/null && kill -0 "${RELAY_X}" 2>/dev/null && break
    sleep 0.02
done
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "exit 0 despite session processes"
else
    case_fail "exit nonzero with only supervise/mcp-relay"; tail -3 "${FAKE_ROOT}/rb.log"
fi
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_ok "restore proceeded" || case_fail "restore did not run"
teardown_case
case_end

# R13: целевой релиз не существует — отказ до остановки хаба.
case_begin "rollback refuses a missing target release"
setup_rollback notarget
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to cccccccccccc >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with a missing target release"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "missing or incomplete" "missing release reported"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-hub \
    && case_ok "hub still active" || case_fail "hub not active"
teardown_case
case_end

# R13b: каталог релиза есть, но неполный (нет SHA256SUMS) — отказ до стопа.
case_begin "rollback refuses an incomplete target release"
setup_rollback incompleterelease
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
rm -f "${HUB_RELEASES}/${SHA_PREV}/SHA256SUMS"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with an incomplete target release"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "missing or incomplete" "incomplete release reported"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R14: недоделанный flip оставил current.new — отказ до остановки хаба.
case_begin "rollback refuses leftover current.new"
setup_rollback currentnew
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
ln -s "releases/${SHA_PREV}" "${HUB_BASE}/current.new"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with leftover current.new"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "leftover" "leftover current.new reported"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R15: flip упал уже после остановки — хаб снова поднят на текущем релизе,
# база не восстановлена, отчёт громкий.
case_begin "rollback restarts hub after failed flip"
setup_rollback flipfail
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
export FAKE_FAIL_CMD="mv -T"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 despite failed flip"
else
    case_ok "exit nonzero"
fi
unset FAKE_FAIL_CMD
assert_contains "${FAKE_ROOT}/rb.log" "flip to ${SHA_PREV} FAILED" "flip failure reported"
assert_contains "${FAKE_ROOT}/rb.log" "starting the hub again on its current release" "hub restart announced"
assert_contains "${FAKE_ROOT}/rb.log" "no database restore was attempted" "database untouched reported"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-hub \
    && case_ok "hub restarted on current release" || case_fail "hub left stopped"
teardown_case
case_end

# R16: битый дамп (нет _sqlx_migrations) — отказ до остановки хаба.
case_begin "rollback refuses a dump without migrations"
setup_rollback baddump
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
printf 'TOC-LINE-01\nTOC-LINE-02\n' >"${FAKE_ROOT}/hub/backups/bad.dump"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/bad.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with a dump without migrations"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "dump verification failed before stopping the hub" "dump failure reported before stop"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R17: огромный TOC не должен давать SIGPIPE в проверке (pipefail ловил 141).
case_begin "rollback verifies a huge dump TOC"
setup_rollback hugetoc
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
{
    printf 'TOC-LINE-01 _sqlx_migrations table\n'
    awk 'BEGIN { for (i = 0; i < 100000; i++) printf "TOC-FILLER-%06d\n", i }'
    printf 'MIGR_MAX=40\nFAILED=0\npayload-huge\n'
} >"${FAKE_ROOT}/hub/backups/huge.dump"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/huge.dump" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "exit 0 with a huge TOC"
else
    case_fail "exit nonzero on a huge TOC"; tail -3 "${FAKE_ROOT}/rb.log"
fi
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_ok "restore proceeded" || case_fail "restore did not run"
teardown_case
case_end

# R18: осиротевший воркер, чей бинарь удалён после запуска: /proc/PID/exe
# кончается на " (deleted)" и не совпадает с точным путём релиза. Отказ.
case_begin "rollback refuses an orphan whose exe was unlinked"
setup_rollback unlinkexe
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
BIN="${WORKER_RELEASES}/${SHA_PREV}/bin/canterlot-worker"
setsid "${BIN}" run >/dev/null 2>&1 <&- &
UNLINK_PID=$!
HARNESS_PIDS+=("${UNLINK_PID}")
for _ in $(seq 1 50); do kill -0 "${UNLINK_PID}" 2>/dev/null && break; sleep 0.02; done
rm -f "${BIN}"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with an unlinked orphan worker"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "still runs" "unlinked orphan refusal logged"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R19: воркер-процесс вне дерева релизов (чужой путь, но тот же бинарь по имени):
# отказ по basename exe/argv[0], а не только по точному пути релиза.
case_begin "rollback refuses a worker binary outside the release tree"
setup_rollback outsidetree
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
mkdir -p "${FAKE_ROOT}/outside"
cp "${FAKE_WORKER_BIN}" "${FAKE_ROOT}/outside/canterlot-worker"
chmod +x "${FAKE_ROOT}/outside/canterlot-worker"
setsid "${FAKE_ROOT}/outside/canterlot-worker" run >/dev/null 2>&1 <&- &
OUTSIDE_PID=$!
HARNESS_PIDS+=("${OUTSIDE_PID}")
for _ in $(seq 1 50); do kill -0 "${OUTSIDE_PID}" 2>/dev/null && break; sleep 0.02; done
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with a worker outside the release tree"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "still runs" "outside-tree worker refusal logged"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R20: воркер появился уже после ранней проверки (гонка с auto-restart/руками):
# последняя сверка ровно перед dropdb обязана остановить восстановление.
case_begin "rollback re-checks the worker right before dropdb"
setup_rollback racerestore
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
mkdir -p "${FAKE_ROOT}/outside"
cp "${FAKE_WORKER_BIN}" "${FAKE_ROOT}/outside/canterlot-worker"
chmod +x "${FAKE_ROOT}/outside/canterlot-worker"
export FAKE_SPAWN_WORKER_ON_HUB_STOP="${FAKE_ROOT}/outside/canterlot-worker"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with a worker that appeared before dropdb"
else
    case_ok "exit nonzero"
fi
unset FAKE_SPAWN_WORKER_ON_HUB_STOP
assert_contains "${FAKE_ROOT}/rb.log" "immediately before dropdb" "last-moment check ran"
assert_contains "${FAKE_ROOT}/rb.log" "still runs" "last-moment refusal logged"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
teardown_case
case_end

# R21: юнит с локально установленным файлом (~/.config/systemd/user) защищён
# drop-in'ом на время restore (mask тут невозможен), старт во время restore
# блокируется, а после выхода drop-in снят и юнит снова запускается.
case_begin "rollback guards the locally installed worker unit during restore"
setup_rollback maskunit
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
cat >"${FAKE_ROOT}/dropdb-hook.sh" <<EOF
#!/usr/bin/env bash
if FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" --user start canterlot-worker >"${FAKE_ROOT}/state/hook-start.out" 2>&1; then
    echo 0 >"${FAKE_ROOT}/state/hook-start.rc"
else
    echo 1 >"${FAKE_ROOT}/state/hook-start.rc"
fi
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" --user is-active --quiet canterlot-worker && echo active >"${FAKE_ROOT}/state/hook-active" || echo inactive >"${FAKE_ROOT}/state/hook-active"
EOF
export FAKE_DROPDB_HOOK="${FAKE_ROOT}/dropdb-hook.sh"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "exit 0 with a locally installed unit file"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/rb.log"
fi
unset FAKE_DROPDB_HOOK
[ -e "${FAKE_ROOT}/state/hook-start.rc" ] && case_ok "the start attempt during the restore happened" || case_fail "dropdb hook never ran"
assert_eq "$(cat "${FAKE_ROOT}/state/hook-start.rc" 2>/dev/null)" "1" "start is refused during the restore"
assert_eq "$(cat "${FAKE_ROOT}/state/hook-active" 2>/dev/null)" "inactive" "worker stays inactive during the restore"
GUARD_LINE="$(grep -n 'daemon-reload' "${FAKE_ROOT}/state/calls.log" | head -1 | cut -d: -f1)"
DROP_LINE="$(grep -n '^dropdb ' "${FAKE_ROOT}/state/calls.log" | head -1 | cut -d: -f1)"
if [ -n "${GUARD_LINE}" ] && [ -n "${DROP_LINE}" ] && [ "${GUARD_LINE}" -lt "${DROP_LINE}" ]; then
    case_ok "guard loaded (daemon-reload) before dropdb"
else
    case_fail "guard/dropdb order wrong (reload=${GUARD_LINE:-none}, dropdb=${DROP_LINE:-none})"
fi
[ ! -e "${WORKER_UNIT_DIR}/canterlot-worker.service.d/zz-rollback-db-guard.conf" ] \
    && case_ok "guard drop-in removed on exit" || case_fail "guard drop-in left behind"
[ -f "${WORKER_UNIT_DIR}/canterlot-worker.service" ] && case_ok "the installed unit file is untouched" || case_fail "unit file damaged"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" --user start canterlot-worker \
    && case_ok "the owner can start the worker after the rollback" || case_fail "worker still blocked after exit"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" --user is-active --quiet canterlot-worker \
    && case_ok "worker active after the owner's start" || case_fail "worker not active after the owner's start"
teardown_case
case_end

# R21b: guard, который systemd не загрузил (нет daemon-reload / неверный drop-in),
# обнаруживается проверкой свойств ДО остановки хаба: выход с ошибкой, drop-in убран.
case_begin "rollback refuses when the guard is not loaded"
setup_rollback guardnotloaded
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
export FAKE_SYSTEMCTL_IGNORE_RELOAD=1
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with an unloaded guard"
else
    case_ok "exit nonzero"
fi
unset FAKE_SYSTEMCTL_IGNORE_RELOAD
assert_contains "${FAKE_ROOT}/rb.log" "guard" "guard failure reported"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
[ ! -e "${WORKER_UNIT_DIR}/canterlot-worker.service.d/zz-rollback-db-guard.conf" ] \
    && case_ok "guard drop-in removed" || case_fail "guard drop-in left behind"
teardown_case
case_end

# R22: --to с shell-метасимволами отклоняется до любого действия.
case_begin "rollback rejects an invalid target release id"
setup_rollback badtargetid
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
: >"${FAKE_ROOT}/state/calls.log"
if ( cd "${FAKE_ROOT}" && bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "';>PWN;#xxxx" ) >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 despite an invalid target release id"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "invalid release id" "invalid release id reported"
[ ! -e "${FAKE_ROOT}/PWN" ] && case_ok "no injected file created" || case_fail "shell injection executed"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
teardown_case
case_end

# R23: цель исчезла между ранней проверкой и flip: flip отказывает под lock,
# хаб поднимается на прежнем релизе, база нетронута, dangling current нет.
case_begin "rollback target removed before the flip is refused inside the lock"
setup_rollback targetgone
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
cat >"${FAKE_ROOT}/rm-hook.sh" <<EOF
#!/usr/bin/env bash
[ ! -e "${FAKE_ROOT}/state/rm-ran" ] || exit 0
: >"${FAKE_ROOT}/state/rm-ran"
rm -rf "${HUB_RELEASES}/${SHA_PREV}"
EOF
chmod +x "${FAKE_ROOT}/rm-hook.sh"
: >"${FAKE_ROOT}/state/calls.log"
export FAKE_SSH_AFTER_CMD="SHA256SUMS" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/rm-hook.sh"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with a vanished target"
else
    case_ok "exit nonzero"
fi
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
[ -e "${FAKE_ROOT}/state/rm-ran" ] && case_ok "the competitor removed the target" || case_fail "hook never ran"
[ -e "$(readlink -m "${HUB_CURRENT}")" ] && case_ok "current resolves" || case_fail "current dangles"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-hub \
    && case_ok "hub restarted on its current release" || case_fail "hub left stopped"
teardown_case
case_end

# R24: retain между проверкой цели и flip не может её удалить (резерв).
case_begin "retention during rollback cannot remove the reserved target"
setup_rollback reservetarget
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
deploy_release web "${SHA_PREV}"
printf '{"version":"%s"}' "${SHA_PREV}" >"${FAKE_ROOT}/container/version.json"
ln -sfn "releases/${SHA_NEW}" "${HUB_CURRENT}"
touch -d '2020-01-01 00:00:00' "${HUB_RELEASES}/${SHA_PREV}"
cat >"${FAKE_ROOT}/retain-hook.sh" <<EOF
#!/usr/bin/env bash
[ ! -e "${FAKE_ROOT}/state/retain-ran" ] || exit 0
: >"${FAKE_ROOT}/state/retain-ran"
RETAIN_COUNT=1 bash "${DEPLOY_DIR}/retain.sh" --apply >"${FAKE_ROOT}/retain-hook.log" 2>&1
EOF
chmod +x "${FAKE_ROOT}/retain-hook.sh"
export FAKE_SSH_AFTER_CMD="SHA256SUMS" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/retain-hook.sh"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "rollback completed"
else
    case_fail "rollback failed"; tail -3 "${FAKE_ROOT}/rb.log"
fi
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
[ -e "${FAKE_ROOT}/state/retain-ran" ] && case_ok "the competing retention ran" || case_fail "retention hook never ran"
assert_contains "${FAKE_ROOT}/retain-hook.log" "reserved by a running operation" "retention saw the reservation"
[ "$(readlink "${HUB_CURRENT}" | sed 's|.*/||')" = "${SHA_PREV}" ] && case_ok "hub flipped back" || case_fail "hub not flipped back"
ls "${HUB_BASE}"/.reserved.* >/dev/null 2>&1 && case_fail "reservation left behind" || case_ok "reservation released on exit"
teardown_case
case_end

# R25: без --to текущий релиз хаба тоже проверяется до остановки хаба:
# current на несуществующий релиз раньше доходил до dropdb.
case_begin "rollback without --to validates the current release before stopping the hub"
setup_rollback nomissingcurrent
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
ln -sfn "releases/cccccccccccc" "${HUB_CURRENT}"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_fail "exit 0 with current pointing to a missing release"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/rb.log" "missing or incomplete" "missing current release reported"
grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb ran" || case_ok "no dropdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "hub stopped" || case_ok "hub untouched"
[ -s "${FAKE_ROOT}/state/db.txt" ] && case_ok "database untouched" || case_fail "database gone"
teardown_case
case_end

# R25b: без --to и с целым текущим релизом откат делается на нём без flip.
case_begin "rollback without --to restores onto the current release"
setup_rollback currentok
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" stop canterlot-worker >/dev/null
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/rb.log"
fi
[ "$(readlink "${HUB_CURRENT}" | sed 's|.*/||')" = "${SHA_NEW}" ] && case_ok "current unchanged (no flip)" || case_fail "current moved"
diff "${FAKE_ROOT}/hub/backups/can-pre.dump" "${FAKE_ROOT}/state/db.txt" >/dev/null \
    && case_ok "db restored from dump" || case_fail "db differs from dump"
teardown_case
case_end

printf 'rollback harness: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
