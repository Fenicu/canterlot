#!/usr/bin/env bash
# Тесты инвариантов, на которых обожглись реальные выкаты 1.16.0, 1.16.1 и 1.17.0:
# скан управляющего воркера не ловит обёртки со словом canterlot-worker в пути;
# ожидание live читает голое state="live", а не time="…"; записанный процесс
# обязан жить с тем же тиком, новые транзиентные дети допустимы; ожидание не
# обрывается set -e на первом неудачном poll.
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

# Быстрый sleep для ожиданий; команды ниже используют его же.
sleep() { case "${1:-}" in 0.05|0.2|0.3|1) command sleep 0.05;; *) command sleep "$@";; esac; }
export -f sleep

source_deploy_lib() {
    # shellcheck source=/dev/null
    source "${DEPLOY_DIR}/lib.sh"
}

# I1: путь обёртки содержит "canterlot-worker", но её basename — нет; скан не
# должен принять её за управляющий воркер (ложное срабатывание 1.16.0).
case_begin "control-worker scan ignores wrapper paths"
setup_case "inv-wrapper"
source_deploy_lib
mkdir -p "${FAKE_ROOT}/canterlot-worker/wrapper-bin"
cp /bin/sleep "${FAKE_ROOT}/canterlot-worker/wrapper-bin/not-a-worker"
chmod +x "${FAKE_ROOT}/canterlot-worker/wrapper-bin/not-a-worker"
setsid "${FAKE_ROOT}/canterlot-worker/wrapper-bin/not-a-worker" 100 >/dev/null 2>&1 <&- &
WRAPPER_PID=$!
sleep 0.2
if WRAPPER_OUT="$(find_control_worker)"; then
    case_fail "wrapper matched the scan: ${WRAPPER_OUT}"
else
    case_ok "wrapper path is not a control worker"
fi
[ "$(readlink "/proc/${WRAPPER_PID}/exe")" = "${FAKE_ROOT}/canterlot-worker/wrapper-bin/not-a-worker" ] \
    && case_ok "wrapper process really runs from the canterlot-worker path" \
    || case_fail "wrapper process missing"
# Настоящий управляющий воркер (exe basename canterlot-worker, argv[1]=run) находится.
mkdir -p "${FAKE_ROOT}/worker/opt/canterlot/releases/aaaaaaaaaaaa/bin"
cp "${FAKE_WORKER_BIN}" "${FAKE_ROOT}/worker/opt/canterlot/releases/aaaaaaaaaaaa/bin/canterlot-worker"
setsid bash -c "exec -a '${FAKE_ROOT}/worker/opt/canterlot/releases/aaaaaaaaaaaa/bin/canterlot-worker' '${FAKE_ROOT}/worker/opt/canterlot/releases/aaaaaaaaaaaa/bin/canterlot-worker' run" >/dev/null 2>&1 <&- &
sleep 0.2
if REAL_OUT="$(find_control_worker)"; then
    case_ok "real control worker found (${REAL_OUT})"
else
    case_fail "real control worker not found"
fi
REAL_PID="$(printf '%s' "${REAL_OUT}" | sed -n 's/^worker process \([0-9]*\).*/\1/p')"
[ -n "${REAL_PID}" ] && kill "${REAL_PID}" 2>/dev/null || true
command sleep 0.2
# Подкоманды supervise/mcp-relay по-прежнему пропускаются.
setsid bash -c "exec -a '${FAKE_ROOT}/worker/opt/canterlot/releases/aaaaaaaaaaaa/bin/canterlot-worker' '${FAKE_ROOT}/worker/opt/canterlot/releases/aaaaaaaaaaaa/bin/canterlot-worker' supervise --exec-dir '${FAKE_ROOT}/worker/data/executions/x'" >/dev/null 2>&1 <&- &
sleep 0.2
if SUP_OUT="$(find_control_worker)"; then
    case_fail "supervise was reported as a control worker: ${SUP_OUT}"
else
    case_ok "supervise is skipped, not a control worker"
fi
# Клиент аренды (`lease run` вокруг сборки) тоже не управляющий воркер (выкат 1.23.2).
setsid bash -c "exec -a '${FAKE_ROOT}/worker/opt/canterlot/releases/aaaaaaaaaaaa/bin/canterlot-worker' '${FAKE_ROOT}/worker/opt/canterlot/releases/aaaaaaaaaaaa/bin/canterlot-worker' lease run --kind gate -- sleep 100" >/dev/null 2>&1 <&- &
sleep 0.2
if LEASE_OUT="$(find_control_worker)"; then
    case_fail "lease client was reported as a control worker: ${LEASE_OUT}"
else
    case_ok "lease client is skipped, not a control worker"
fi
teardown_case
case_end

# I2: time="…" без state="live" не удовлетворяет ожидание; голое state="live"
# удовлетворяет, а первый неудачный poll под set -e не обрывает цикл (1.16.0).
case_begin "live wait parses the bare state=live journal line"
setup_case "inv-live"
source_deploy_lib
export FAKE_LIVE_SEQUENCE=3
if live_from_journal "@0" 20; then
    case_ok "waited through connecting/time lines until state=live"
else
    case_fail "did not reach live after time= lines"
fi
# Тот же вызов в скрипте с set -e и БЕЗ условия if: первый неудачный poll не
# должен обрывать ожидание (ровно баг 1.17.0 `[ … ] && break`).
rm -f "${FAKE_ROOT}/state/live-seq"
SET_OUT="$(FAKE_LIVE_SEQUENCE=3 bash -c 'set -euo pipefail; source "$1/lib.sh"; live_from_journal "@0" 20; printf OK' _ "${DEPLOY_DIR}" 2>&1)"
if [ "${SET_OUT}" = "OK" ]; then
    case_ok "live wait does not abort under set -e on a non-live first poll"
else
    case_fail "set -e aborted the live wait: [${SET_OUT}]"
fi
rm -f "${FAKE_ROOT}/state/live-seq"
export FAKE_LIVE_SEQUENCE=999
if LAST_STATE="$(live_from_journal "@0" 1)"; then
    case_fail "time= lines alone satisfied the live wait"
else
    case_ok "time= lines alone do not satisfy the live wait (last: ${LAST_STATE})"
fi
teardown_case
case_end

# I3: записанные процессы живы с теми же тиками; новый транзиентный ребёнок
# (в снимке его нет) допустим и печатается; исчезновение записанного PID — дрейф
# (1.17.0: равенство 15 строк падало на лишнем CLI-ребёнке).
case_begin "snapshot requires recorded PIDs, allows new transient children"
setup_case "inv-drift"
source_deploy_lib
EXEC_ID="$(new_exec_id)"
spawn_session "${EXEC_ID}" aaaaaaaaaaaa 100
SUP_PID="$(sup_pid_of "${EXEC_ID}")"
[ -n "${SUP_PID}" ] && case_ok "supervisor started" || case_fail "no supervisor"
SUP_TICK="$(proc_start_tick "${SUP_PID}")"
CLI_LINE="$(session_cli_pid "${SUP_PID}")"
CLI_PID="${CLI_LINE%% *}"
CLI_TICK="${CLI_LINE##* }"
RELAY_PID="$(relay_pids_for_exec "${EXEC_ID}" | head -1)"
RELAY_TICK="$(proc_start_tick "${RELAY_PID}")"
SNAP="${FAKE_ROOT}/snap.state"
# Записан только супервизор: CLI/релей для снимка — «новые», это допустимо.
printf '%s\n' "${EXEC_ID} sup=${SUP_PID}:${SUP_TICK} cli=none relay=none sup_exe=unknown" >"${SNAP}"
if snapshot_drift "${SNAP}" "unit" >"${FAKE_ROOT}/drift1.log" 2>&1; then
    case_ok "new CLI/relay children do not fail the invariant"
else
    case_fail "new children failed the invariant"; cat "${FAKE_ROOT}/drift1.log"
fi
assert_contains "${FAKE_ROOT}/drift1.log" "new transient CLI child" "new child is logged"
# Полный снимок проходит.
printf '%s\n' "${EXEC_ID} sup=${SUP_PID}:${SUP_TICK} cli=${CLI_PID}:${CLI_TICK} relay=${RELAY_PID}:${RELAY_TICK} sup_exe=unknown" >"${SNAP}"
if snapshot_drift "${SNAP}" "unit" >/dev/null 2>&1; then case_ok "full snapshot passes"; else case_fail "full snapshot failed"; fi
# Записанный CLI исчез — дрейф.
kill "${CLI_PID}" 2>/dev/null || true
command sleep 0.3
if snapshot_drift "${SNAP}" "unit" >"${FAKE_ROOT}/drift2.log" 2>&1; then
    case_fail "a recorded PID that disappeared passed"
else
    case_ok "a recorded PID that disappeared fails"
fi
assert_contains "${FAKE_ROOT}/drift2.log" "recorded session process gone" "disappearance reported"
# Записанный супервизор исчез — тоже дрейф.
printf '%s\n' "${EXEC_ID} sup=${SUP_PID}:${SUP_TICK} cli=none relay=none sup_exe=unknown" >"${SNAP}"
kill "${SUP_PID}" 2>/dev/null || true
command sleep 0.3
if snapshot_drift "${SNAP}" "unit" >/dev/null 2>&1; then
    case_fail "a recorded supervisor that disappeared passed"
else
    case_ok "a recorded supervisor that disappeared fails"
fi
teardown_case
case_end

printf 'invariants harness: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
