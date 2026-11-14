#!/usr/bin/env bash
# Гарантии cutover.sh, найденные внешним ревью: счёт жёстких отказов, коды отказа
# дочерних скриптов, сверка сессий после vzdump в обе стороны, блок STOP по фактам,
# сигналы во время итогового отчёта, токен только у чистой репетиции.
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

R_FILE() { printf '%s/cutover.result' "${FAKE_ROOT}"; }

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

printf 'cutover-guarantees harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
