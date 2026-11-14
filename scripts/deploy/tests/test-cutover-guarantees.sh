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

# --- мягкие ошибки дочерних скриптов не дают токена ---
case_begin "child verify soft failures fail the rehearsal and issue no token"
setup_cutover soft-child-rehearse
# Развёрнутый билд без br-соседа: проба precompressed в verify не находит ассет.
rm -f "${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe.js.br"
rc=0; run_rehearse || rc=$?
[ "${rc}" != "0" ] && case_ok "rehearsal exit ${rc}" || case_fail "rehearsal exit 0 despite verify soft failures"
assert_contains "${FAKE_ROOT}/rehearse.log" "SOFT-FAIL: precompressed check" "verify soft failure printed"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "CONFIRM=" "no token printed"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "rehearsal PASS" "no PASS line"
assert_eq "$(result_of "${FAKE_ROOT}/rehearse.result" status)/$(result_of "${FAKE_ROOT}/rehearse.result" token)" "fail/" "result file: fail without a token"
[ "$(result_of "${FAKE_ROOT}/rehearse.result" soft_failures)" -ge 1 ] && case_ok "result file: soft failures counted" || case_fail "result file: soft_failures 0"
teardown_case
case_end

case_begin "child preflight soft failures fail the dry-run and issue no token"
setup_cutover soft-child-dryrun
RELAY1="$(ps -eo pid=,args= | awk -v id="${EXEC1}" '$0 ~ "mcp-relay" && $0 ~ id {print $1; exit}')"
kill "${RELAY1}" && case_ok "relay of session 1 stopped" || case_fail "setup: no relay"
rc=0
bash "${DEPLOY_DIR}/cutover.sh" "${SHA_NEW}" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" \
    >"${FAKE_ROOT}/dry.log" 2>&1 || rc=$?
[ "${rc}" != "0" ] && case_ok "dry-run exit ${rc}" || case_fail "dry-run exit 0 despite a preflight soft failure"
assert_contains "${FAKE_ROOT}/dry.log" "SOFT-FAIL: no mcp-relay for live execution" "preflight soft failure printed"
assert_not_contains "${FAKE_ROOT}/dry.log" "CONFIRM=" "no token printed"
assert_eq "$(result_of "${FAKE_ROOT}/dry.result" status)/$(result_of "${FAKE_ROOT}/dry.result" token)" "fail/" "result file: fail without a token"
teardown_case
case_end

# --- сверка сессий после vzdump в обе стороны ---
# Строка running без супервизора, появившаяся во время vzdump: отказ до стопа воркера.
case_begin "a running row without a supervisor added during vzdump aborts before the worker stop"
setup_cutover inv-row-nosup
GHOST="$(new_exec_id)"
cat >"${FAKE_ROOT}/ghost-row.sh" <<HOOK
#!/usr/bin/env bash
printf '%s\n' "${GHOST}" >>"${FAKE_ROOT}/state/live-ids"
HOOK
export FAKE_SSH_AFTER_CMD="--notes-template" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/ghost-row.sh"
run_cutover && case_fail "exit 0 with a running row without a supervisor" || case_ok "exit nonzero"
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
grep -q '^vzdump ' "${FAKE_ROOT}/state/calls.log" && case_ok "vzdump ran" || case_fail "vzdump never ran"
worker_stopped_ever && case_fail "worker was stopped" || case_ok "worker never stopped"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub flipped"
assert_contains "${FAKE_ROOT}/cutover.log" "live hub execution ${GHOST} has no supervisor process" "the row is named"
assert_eq "$(result_of "$(R_FILE)" status)/$(result_of "$(R_FILE)" fail_codes)" "failed_before_flip/session_inventory" "result file: before the flip, code session_inventory"
teardown_case
case_end

# Неосвобождённая запись ownership без строки и без супервизора во время vzdump: отказ.
case_begin "an orphan unreleased ownership record added during vzdump aborts before the worker stop"
setup_cutover inv-orphan-own
ORPHAN="$(new_exec_id)"
KEEP_EXEC1="${EXEC1}"; EXEC1="${ORPHAN}"; write_ownership; EXEC1="${KEEP_EXEC1}"
mv "${WORKER_DATA}/ownership/snapshot.toml" "${FAKE_ROOT}/orphan-ownership.toml"
write_ownership
cat >"${FAKE_ROOT}/orphan-own.sh" <<HOOK
#!/usr/bin/env bash
cp "${FAKE_ROOT}/orphan-ownership.toml" "${WORKER_DATA}/ownership/snapshot.toml"
HOOK
export FAKE_SSH_AFTER_CMD="--notes-template" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/orphan-own.sh"
run_cutover && case_fail "exit 0 with an orphan ownership record" || case_ok "exit nonzero"
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
worker_stopped_ever && case_fail "worker was stopped" || case_ok "worker never stopped"
assert_contains "${FAKE_ROOT}/cutover.log" "orphan unreleased ownership for ${ORPHAN}" "the record is named"
assert_eq "$(result_of "$(R_FILE)" fail_codes)" "session_inventory" "result file: code session_inventory"
teardown_case
case_end

# Новая полноценная сессия во время vzdump (своя строка running, супервизор, CLI,
# релей; бинарь нового релиза): входит в снимок тиков, spec_compat — и против её бинаря.
case_begin "a valid session started during vzdump enters the snapshot and the spec checks"
setup_cutover inv-new-valid
NEWID="$(new_exec_id)"
NEWREL=cccccccccccc
NEWBIN="${WORKER_RELEASES}/${NEWREL}/bin"
mkdir -p "${NEWBIN}" "${WORKER_DATA}/executions/${NEWID}/sj"
cp "${FAKE_WORKER_BIN}" "${NEWBIN}/canterlot-worker"
cp /bin/sleep "${NEWBIN}/sleep-cli"
cp "${REPO_ROOT}/crates/canterlot-worker/tests/it/fixtures/spec/1dbdd180ffce.toml" "${WORKER_DATA}/executions/${NEWID}/spec.toml"
python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', 300) + b'\0'*4)" >"${WORKER_DATA}/executions/${NEWID}/sj/ack"
python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()' "${WORKER_SOCKET_DIR}/${NEWID}.sock"
cat >"${FAKE_ROOT}/new-session.sh" <<HOOK
#!/usr/bin/env bash
setsid bash -c "
    ( exec -a 'opencode --session ${NEWID}' '${NEWBIN}/sleep-cli' 1000 ) &
    ( exec -a '${NEWBIN}/canterlot-worker' '${NEWBIN}/canterlot-worker' mcp-relay --socket '${WORKER_DATA}/executions/${NEWID}/mcp.sock' ) &
    exec -a '${NEWBIN}/canterlot-worker' '${NEWBIN}/canterlot-worker' supervise --exec-dir '${WORKER_DATA}/executions/${NEWID}'
" >/dev/null 2>&1 <&- &
command sleep 0.5
printf '%s\n' "${NEWID}" >>"${FAKE_ROOT}/state/live-ids"
HOOK
export FAKE_SSH_AFTER_CMD="--notes-template" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/new-session.sh"
run_cutover && case_ok "cutover done" || { case_fail "cutover failed"; tail -15 "${FAKE_ROOT}/cutover.log"; }
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
grep -q "^${NEWID} sup=" "${FAKE_ROOT}/snap.state" && case_ok "the new session is in the tick snapshot" || case_fail "the new session is not in the snapshot"
grep -q "^${EXEC1} sup=" "${FAKE_ROOT}/snap.state" && grep -q "^${EXEC2} sup=" "${FAKE_ROOT}/snap.state" \
    && case_ok "the recorded sessions stay in the snapshot" || case_fail "recorded sessions lost from the snapshot"
assert_contains "${FAKE_ROOT}/cutover.log" "session ${NEWID} started after the preflight: reconciled and added to the tick snapshot" "the new session is reported"
assert_contains "${FAKE_ROOT}/cutover.log" "spec_compat against ${NEWBIN}/canterlot-worker" "spec_compat ran against the new supervisor binary"
assert_contains "${FAKE_ROOT}/cutover.log" "spec ${NEWID}:" "the new spec shape was checked"
assert_eq "$(result_of "$(R_FILE)" status)" "done" "result file: done"
NEWSUP="$(sup_pid_of "${NEWID}")"
assert_proc_alive "${NEWSUP}" "the new session survived the cutover"
teardown_case
case_end

printf 'cutover-guarantees harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
