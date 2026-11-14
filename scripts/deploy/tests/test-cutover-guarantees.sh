#!/usr/bin/env bash
# Гарантии cutover.sh, найденные внешним ревью: счёт жёстких отказов, коды отказа
# дочерних скриптов, сверка сессий после vzdump в обе стороны, блок STOP по фактам,
# сигналы во время итогового отчёта, токен только у чистой репетиции.
# shellcheck disable=SC2015,SC2119,SC2120 # assert pattern; run_rehearse takes optional flags
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=cutover-fixture.sh
source "${HARNESS_DIR}/cutover-fixture.sh"

R_FILE() { printf '%s/cutover.result' "${FAKE_ROOT}"; }
run_rehearse() {
    bash "${DEPLOY_DIR}/cutover.sh" "${SHA_NEW}" --rehearse --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" \
        --state "${FAKE_ROOT}/rehearse.state" "$@" >"${FAKE_ROOT}/rehearse.log" 2>&1
}

# --- оба помощника жёсткого отказа считают его в hard_failures ---
case_begin "cutover_hard counts the hard failure"
setup_cutover hardcount
export FAKE_FAIL_VZDUMP=1
run_cutover && case_fail "exit 0 despite a failed vzdump" || case_ok "exit nonzero"
unset FAKE_FAIL_VZDUMP
assert_contains "${FAKE_ROOT}/cutover.log" "HARD-FAIL: vzdump failed" "hard failure printed"
assert_eq "$(result_of "$(R_FILE)" hard_failures)" "1" "result file: hard_failures=1"
teardown_case
case_end

case_begin "cutover_hard_after counts the hard failure"
setup_cutover hardcount-after
cat >"${FAKE_ROOT}/kill-hook.sh" <<EOF
#!/usr/bin/env bash
kill ${SUP2}
for _ in \$(seq 1 50); do kill -0 ${SUP2} 2>/dev/null || exit 0; command sleep 0.05; done
EOF
export FAKE_SSH_AFTER_CMD="--notes-template" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/kill-hook.sh"
run_cutover && case_fail "exit 0 despite a lost session" || case_ok "exit nonzero"
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
assert_contains "${FAKE_ROOT}/cutover.log" "HARD-FAIL: the session inventory changed after vzdump" "hard failure printed"
assert_eq "$(result_of "$(R_FILE)" hard_failures)" "1" "result file: hard_failures=1"
worker_stopped_ever && case_fail "worker was stopped" || case_ok "worker never stopped"
teardown_case
case_end

# --- дочерний скрипт, умерший не своим путём, даёт unknown, даже если успел записать код ---
case_begin "a child crash after a transient code is coded unknown"
setup_cutover child-crash
EXEC3="$(new_exec_id)"
spawn_session "${EXEC3}" "${SHA_PREV}" 300
printf '%s starting\n' "${EXEC3}" >"${FAKE_ROOT}/state/exec-states"
install_exe_readlink_fault
FAKE_READLINK_FAIL_EXE=1 run_rehearse && case_fail "rehearsal passed with a crashed preflight" || case_ok "rehearsal failed"
assert_contains "${FAKE_ROOT}/rehearse.log" "whose hub row is still starting" "preflight recorded the transient code first"
assert_contains "${FAKE_ROOT}/state/calls.log" "readlink-fault" "the injected exe read failure fired"
CODES="$(result_of "${FAKE_ROOT}/rehearse.result" fail_codes)"
[[ ",${CODES}," == *",unknown,"* ]] && case_ok "result file: unknown among the codes (${CODES})" || case_fail "result file: no unknown in [${CODES}]"
[[ ",${CODES}," == *",session_starting,"* ]] && case_ok "result file: the recorded transient code kept" || case_fail "result file: session_starting lost in [${CODES}]"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "CONFIRM=" "no token"
teardown_case
case_end

case_begin "a child that fails through its handled path keeps its own codes only"
setup_cutover child-handled
EXEC3="$(new_exec_id)"
spawn_session "${EXEC3}" "${SHA_PREV}" 300
printf '%s starting\n' "${EXEC3}" >"${FAKE_ROOT}/state/exec-states"
export FAKE_CT_LOCK=1
run_rehearse && case_fail "rehearsal passed" || case_ok "rehearsal failed"
unset FAKE_CT_LOCK
assert_eq "$(result_of "${FAKE_ROOT}/rehearse.result" fail_codes)" "session_starting,vzdump_busy" "result file: only the transient codes of the handled refusal"
teardown_case
case_end

printf 'cutover-guarantees harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
