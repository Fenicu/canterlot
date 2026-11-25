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

# shellcheck source-path=SCRIPTDIR
# shellcheck source=cutover-fixture.sh
source "${HARNESS_DIR}/cutover-fixture.sh"

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
assert_contains "${FAKE_ROOT}/cutover.log" "malformed --confirm token" "wrong token reported"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" status)" "refused" "result file: refused (bad token)"
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
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" status)" "refused" "result file: refused (gates)"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" worker_stopped)" "no" "result file: worker not stopped"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" fail_codes)" "vzdump_busy" "result file: the CT backup lock coded vzdump_busy"
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
assert_contains "${FAKE_ROOT}/cutover.log" "cannot query CT 101 lock state" "status query failure reported"
teardown_case
case_end

# --- A2c. PROXMOX_NODE расходится с настоящей нодой CT — отказ до любых изменений ---
case_begin "PROXMOX_NODE disagrees with the real node"
setup_cutover pvewrongnode
export PROXMOX_NODE=pve-node2
if run_cutover; then case_fail "exit 0 despite a wrong PROXMOX_NODE"; else case_ok "exit nonzero"; fi
export PROXMOX_NODE=pve-node1
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker still active (never stopped)" || case_fail "worker not active"
assert_contains "${FAKE_ROOT}/cutover.log" "PROXMOX_NODE=pve-node2 but CT 101 runs on node pve-node1" "both names reported"
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

# Сессии, изменившиеся вокруг vzdump, стопа и старта воркера (бывшие B, C, O2, O2h, O3), —
# в test-cutover-guarantees.sh, рядом с остальной сверкой сессий.

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
# vzdump идёт до стопа: воркер не останавливался, восстанавливать нечего.
worker_stopped_ever && case_fail "worker was stopped before/after a failed vzdump" || case_ok "worker never stopped (vzdump runs while everything runs)"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker still active" || case_fail "worker not active"
assert_contains "${FAKE_ROOT}/cutover.log" "nothing was stopped or flipped" "nothing-to-recover path named"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" status)/$(result_of "${FAKE_ROOT}/cutover.result" recovery)" "failed_before_flip/nothing_to_recover" "result file: vzdump failure"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- E. pg_dump падает ---
case_begin "pg_dump fails"
setup_cutover pgdumpfail
export FAKE_FAIL_PG_DUMP=1
if run_cutover; then case_fail "exit 0 despite pg_dump failure"; else case_ok "exit nonzero"; fi
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" status)" "failed_before_flip" "result file: failed_before_flip"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" recovery)" "worker_restarted" "result file: worker restarted"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" vzdump_done)/$(result_of "${FAKE_ROOT}/cutover.result" worker_stopped)/$(result_of "${FAKE_ROOT}/cutover.result" dump_verified)/$(result_of "${FAKE_ROOT}/cutover.result" hub_flipped)" "yes/yes/no/no" "result file: facts"
[ -n "$(result_of "${FAKE_ROOT}/cutover.result" vzdump_notes)" ] && case_ok "result file: vzdump restore point named" || case_fail "result file: no vzdump notes"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" dump)" "" "result file: no unverified dump named"
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
# Бинарь пропадает ПОСЛЕ target-check (он бы поймал это раньше): проверяется
# собственная проверка cutover перед первым изменением.
cat >"${FAKE_ROOT}/rm-hub-bin.sh" <<EOF
#!/usr/bin/env bash
rm -f "${HUB_RELEASES}/${SHA_NEW}/bin/canterlot-hub"
EOF
export FAKE_SSH_AFTER_CMD="${SHA_NEW}/bin'" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/rm-hub-bin.sh"
: >"${FAKE_ROOT}/state/calls.log"
if run_cutover; then case_fail "exit 0 with incomplete hub target"; else case_ok "exit nonzero"; fi
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
assert_contains "${FAKE_ROOT}/cutover.log" "target check PASS for ${SHA_NEW}" "the target check passed before the binary vanished"
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
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" status)" "stop_and_report" "result file: stop_and_report"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" hub_flipped)/$(result_of "${FAKE_ROOT}/cutover.result" worker_live)" "yes/no" "result file: facts"
STOP_BLOCK="$(result_of "${FAKE_ROOT}/cutover.result" stop_block)"
[ -n "${STOP_BLOCK}" ] && grep -q '^STOP-AND-REPORT (phase' "${STOP_BLOCK}" \
    && case_ok "result file: stop block file holds the report" || case_fail "result file: no stop block at [${STOP_BLOCK}]"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" dump)" "$(find "${HUB_BACKUP_DIR}" -name "canterlot-pre-*.dump" | head -1)" "result file: the verified dump named"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker started (must stay stopped)" || case_ok "worker stays stopped"
assert_proc_alive "${SUP1}" "session 1 alive"
assert_proc_alive "${SUP2}" "session 2 alive"
# Блок STOP полного прогона называет свой vzdump (бывший кейс test-fastcutover.sh «STOP-AND-REPORT
# after a full run still names its vzdump»; без vzdump — test-fastguards.sh).
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" vzdump_done)/$(result_of "${FAKE_ROOT}/cutover.result" vzdump_skipped)" "yes/no" "result file: vzdump_done=yes, vzdump_skipped=no"
assert_contains "${STOP_BLOCK}" "this run's vzdump = notes pre-${SHA_NEW}-" "live state: the vzdump is named"
assert_contains "${STOP_BLOCK}" "The vzdump above is a separate" "recovery step 2 keeps the vzdump fallback"
# Идентичность кластера записана рядом с дампом и вшита в команду отката (бывший кейс
# test-fastguards.sh: запись одна на оба пути).
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" pg_system_identifier)/$(result_of "${FAKE_ROOT}/cutover.result" pg_database_oid)" "7000000000000000001/16384" "result file: identity recorded"
RUN_DUMP_PATH="$(result_of "${FAKE_ROOT}/cutover.result" dump)"
PHASE_FILE="${RUN_DUMP_PATH}.restore-phase"
[ -f "${RUN_DUMP_PATH}.identity" ] && grep -q '^system_identifier=7000000000000000001$' "${RUN_DUMP_PATH}.identity" \
    && grep -q '^database_oid=16384$' "${RUN_DUMP_PATH}.identity" && case_ok "the identity file sits next to the dump" || case_fail "no identity file next to the dump"
assert_contains "${STOP_BLOCK}" "--expect-system-id 7000000000000000001 --expect-db-oid 16384" "the printed restore command carries the expected identity"
# Напечатанная команда восстановления должна работать дословно: режим +x и
# никаких скрытых подстановок, которых нет в отчёте.
RECOVERY_CMD="$(printed_rollback_cmd)"
# Сначала отказы, которые базу не трогают: другой кластер, другая база, чужой oid и с записью фазы.
: >"${FAKE_ROOT}/state/calls.log"
export FAKE_PG_SYSID=7999999999999999999
run_printed
unset FAKE_PG_SYSID
[ "${RC}" != "0" ] && case_ok "the printed restore refuses a foreign cluster" || case_fail "the restore ran on a foreign cluster"
assert_contains "${FAKE_ROOT}/rollback.log" "system identifier" "refused on the system identifier"
assert_not_contains "${FAKE_ROOT}/rollback.log" "(before any change)" "not on the worker guard (it was never reached)"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "the hub was stopped" || case_ok "the hub was not stopped"
export FAKE_PG_DBOID=77777
run_printed
[ "${RC}" != "0" ] && case_ok "another database oid, no phase record: refused" || case_fail "the restore ran against another database oid"
assert_contains "${FAKE_ROOT}/rollback.log" "database oid" "refused on the database oid"
printf 'created 16500\n' >"${PHASE_FILE}"
run_printed
[ "${RC}" != "0" ] && case_ok "a phase record for another oid does not excuse 77777" || case_fail "the foreign oid was accepted under a phase record"
unset FAKE_PG_DBOID
rm -f "${PHASE_FILE}"  # lint: local-temp
grep -qE '^(dropdb|createdb) ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb or createdb ran" || case_ok "no dropdb, no createdb on any refusal"
run_printed
[ "${RC}" = "0" ] && case_ok "printed recovery command runs verbatim" || { case_fail "printed recovery command failed"; tail -3 "${FAKE_ROOT}/rollback.log" 2>/dev/null; }
hub_is "${SHA_PREV}" && case_ok "recovery flipped hub back" || case_fail "recovery did not flip hub"
[ "$(grep '^MIGR_MAX=' "${FAKE_ROOT}/state/db.txt" | cut -d= -f2)" = "40" ] \
    && case_ok "recovery restored the dump" || case_fail "recovery did not restore the dump"
# Частичное восстановление: та же напечатанная команда, повторённая, доводит дело до конца
# (бывшие кейсы test-fastguards.sh; фаза пишется рядом с дампом при любом пути).
printf 'MIGR_MAX=41\nFAILED=0\nchanged-after-the-dump\n' >"${FAKE_ROOT}/state/db.txt"
export FAKE_FAIL_PG_RESTORE=1
run_printed
unset FAKE_FAIL_PG_RESTORE
[ "${RC}" != "0" ] && case_ok "pg_restore after createdb fails: the run fails" || case_fail "the run with a failing pg_restore did not fail"
assert_eq "$(cut -d' ' -f1-2 "${PHASE_FILE}" 2>/dev/null)" "created 16500" "the phase file records the replacement oid"
run_printed
assert_eq "${RC}" "0" "the rerun of the same printed command succeeds"
[ "${RC}" = "0" ] || tail -5 "${FAKE_ROOT}/rollback.log"
grep -q 'payload-predump' "${FAKE_ROOT}/state/db.txt" && case_ok "the data is restored from the dump" || case_fail "the data was not restored"
FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-hub && case_ok "the hub is started" || case_fail "the hub is not running"
assert_contains "${FAKE_ROOT}/rollback.log" "replacement database oid 16500" "the recorded replacement oid was accepted"
assert_eq "$(cut -d' ' -f1 "${PHASE_FILE}")" "restored" "the phase ends as restored"
printf 'MIGR_MAX=41\nFAILED=0\nchanged-after-the-dump\n' >"${FAKE_ROOT}/state/db.txt"
export FAKE_FAIL_CREATEDB=1
run_printed
unset FAKE_FAIL_CREATEDB
[ "${RC}" != "0" ] && case_ok "a failure between dropdb and createdb: the run fails" || case_fail "the run with a failing createdb did not fail"
assert_eq "$(cut -d' ' -f1 "${PHASE_FILE}" 2>/dev/null)" "dropped" "the phase file says dropped"
[ ! -f "${FAKE_ROOT}/state/db.txt" ] && case_ok "the database is really missing" || case_fail "the database exists"
run_printed
assert_eq "${RC}" "0" "the rerun continues from dropped and succeeds"
[ "${RC}" = "0" ] || tail -5 "${FAKE_ROOT}/rollback.log"
grep -q 'payload-predump' "${FAKE_ROOT}/state/db.txt" && case_ok "the data is restored from the dump after the dropped phase" || case_fail "the data was not restored after the dropped phase"
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
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" status)" "done" "result file: done despite the soft PWA failure"
# Фейк роняет каждый recreate: и flip, и восстановление env — две мягкие ошибки PWA.
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" soft_by_kind)" "pwa:2" "result file: the soft failures counted as pwa"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" web_release)" "${SHA_PREV}" "result file: PWA stayed on the previous release"
in_order "${FAKE_ROOT}/cutover.log" 'worker state="live" after start' "PWA step (all failures soft" "PWA failure (soft" \
    && case_ok "PWA step ran after the worker was live" || case_fail "PWA step not after the worker was live"
assert_eq "$(grep -c 'stop canterlot-worker' "${FAKE_ROOT}/state/calls.log")" "1" "worker stopped exactly once (before the dump)"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker keeps running after the PWA failure" || case_fail "worker stopped by the PWA failure"
assert_contains "${FAKE_ROOT}/cutover.log" "verify PASS" "verify passed after PWA fallback"
[ "$(cat "${WEB_ENV}")" = "$(cat "${WEB_ENV}.${SHA_PREV}")" ] && case_ok "env restored to prev" || case_fail "env not restored"
assert_proc_alive "${SUP1}" "session 1 alive"
teardown_case
case_end

# --- J2. dist csp.txt пропал: отказ ДО любого flip, ничего не тронуто ---
case_begin "missing dist csp refuses before any flip"
setup_cutover pwacspmissing
# csp.txt пропадает сразу после target-check (последнее его чтение — CSP контейнера).
cat >"${FAKE_ROOT}/rm-csp.sh" <<EOF
#!/usr/bin/env bash
[ ! -e "${FAKE_ROOT}/state/rm-csp-ran" ] || exit 0
: >"${FAKE_ROOT}/state/rm-csp-ran"
rm -f "${FAKE_ROOT}/dist/${SHA_NEW}/csp.txt"
EOF
TOKEN="$(confirm_for "${SHA_NEW}")"
export FAKE_SSH_AFTER_CMD="printenv CANTERLOT_CSP" FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/rm-csp.sh"
bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "${TOKEN}" --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" \
    --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1 && J2_RC=0 || J2_RC=$?
unset FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_HOOK
[ "${J2_RC}" != "0" ] && case_ok "exit nonzero" || case_fail "exit 0 without a dist CSP"
assert_contains "${FAKE_ROOT}/cutover.log" "target check PASS for ${SHA_NEW}" "the target check passed before csp.txt vanished"
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
printf "default-src 'self'; script-src 'sha256-STALE'; connect-src wss://hub.example.com" >"${FAKE_ROOT}/dist/${SHA_NEW}/csp.txt"
dist_resync
if run_cutover; then case_fail "exit 0 with a stale CSP hash"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/cutover.log" "target CSP is not derived from the dist build" "the common preflight's target check refused it"
pgdump_logged && case_fail "pg_dump ran" || case_ok "pg_dump never ran"
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

# Гарантии успешного боевого прогона, общие для полного пути и --fast: оба прохода счастливого
# пути (L и L2) проверяют их этой функцией. $1 — файл результата прогона.
check_done_run() {
    local R="$1" C="${FAKE_ROOT}/state/calls.log" L="${FAKE_ROOT}/cutover.log" gates
    # Итог прогона: все три цели на новом, база на последней миграции, восстановления не было.
    hub_is "${SHA_NEW}" && case_ok "hub on new" || case_fail "hub not new"
    worker_is "${SHA_NEW}" && case_ok "worker on new" || case_fail "worker not new"
    web_is "${SHA_NEW}" && case_ok "web on new" || case_fail "web not new"
    db_migr_is 41 0 && case_ok "db on the last migration, none failed" || case_fail "db not on the last migration"
    restore_logged && case_fail "database restore ran on a done run" || case_ok "no dropdb/createdb/pg_restore restore"
    [ "$(compose_up_count)" = "1" ] && case_ok "exactly one caddy recreate" || case_fail "recreate count wrong"
    grep -q "compose run" "${C}" && case_fail "compose run used" || case_ok "no compose run"
    grep -q '^pg_dump -Fc -Z zstd:3 -f ' "${C}" && case_ok "pg_dump uses zstd:3" || case_fail "pg_dump is not zstd:3"
    grep -q '^pg_restore --list' "${C}" && case_ok "the dump verified with pg_restore --list" || case_fail "no pg_restore --list"
    assert_contains "${L}" "dump fully readable" "the quiesced dump was read back in full"
    assert_contains "${L}" "verify PASS" "verify passed inside cutover"
    assert_proc_alive "${SUP1}" "session 1 supervisor alive"
    assert_proc_alive "${SUP2}" "session 2 supervisor alive"
    gates="$(grep -c 'read gates for target' "${L}" || true)"
    [ "${gates}" -ge 2 ] && case_ok "read gates ran before the writes and again before the stop (${gates}x)" || case_fail "read gates ran ${gates}x"
    assert_contains "${L}" "the session inventory" "the session inventory was re-checked"
    assert_contains "${L}" "control-worker scan before the worker stop: exactly the unit process" "the control-worker scan ran before the stop"
    in_order "${C}" "stop canterlot-worker" "pg_dump -Fc" "pg_restore --list" "restart canterlot-hub" "start canterlot-worker" "compose up -d caddy" \
        && case_ok "commands: worker stop -> pg_dump -> dump verify -> hub restart -> worker start -> caddy recreate" \
        || case_fail "commands out of order"
    assert_eq "$(result_of "${R}" status)/$(result_of "${R}" mode)" "done/execute" "result file: done, mode execute"
    [ -n "$(result_of "${R}" dump)" ] && case_ok "result file: dump named" || case_fail "result file: no dump"
    assert_eq "$(result_of "${R}" dump_verified)" "yes" "result file: dump_verified=yes"
    assert_eq "$(result_of "${R}" token)" "" "result file: no token from a real run"
    assert_eq "$(result_of "${R}" log)" "${L}" "result file: log path"
    assert_eq "$(grep -c 'stop canterlot-worker' "${C}")" "1" "worker stopped once"
    assert_eq "$(grep -c 'start canterlot-worker' "${C}")" "1" "worker started once"
}

# --- L. счастливый путь: порядок, токен, ожидание старта, слияние чанков, обслуживание,
# проба, чек-лист; всё новое, verify PASS, один recreate ---
# Тот же прогон проверяет порог размера дампа по умолчанию (бывший L7): zstd-дамп 65 МБ проходит.
case_begin "cutover happy: order, token, chunk merge, maintenance, probe, checklist, default dump size gate"
setup_cutover happy
printf 'old tab chunk\n' >"${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/old-tab.js"
# Окно activating: два пустых MainPID и один 0 перед настоящим (урок 1.18.1).
export FAKE_MAINPID_FAULTS="empty empty zero"
export FAKE_DUMP_SIZE_BYTES=$((65 * 1024 * 1024))
unset DUMP_MIN_BYTES
# calls.log чистится после стенда: в журнале остаются только команды самого прогона.
: >"${FAKE_ROOT}/state/calls.log"
if run_cutover; then case_ok "exit 0"; else case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/cutover.log"; fi
unset FAKE_MAINPID_FAULTS FAKE_DUMP_SIZE_BYTES
export DUMP_MIN_BYTES=1
C="${FAKE_ROOT}/state/calls.log"
L="${FAKE_ROOT}/cutover.log"
R="${FAKE_ROOT}/cutover.result"
check_done_run "${R}"
assert_contains "${L}" "dump fully readable" "65 MB dump accepted by the default gate"
# Стенд без записи идентичности (ни HUB_PG_SYSTEM_ID, ни .identity у прежнего дампа): полный путь
# идёт и записывает её для следующего прогона (бывший кейс test-fastguards.sh).
[ -f "$(result_of "${R}" dump).identity" ] && case_ok "the full path without an identity source records one next to its dump" || case_fail "no identity recorded"
# Новые шаги боевого прогона видны в журнале.
assert_contains "${L}" "confirm token accepted: release ${SHA_NEW}" "token checked before anything"
assert_contains "${L}" "vzdump done: archive ct/130/" "vzdump success and archive id parsed"
assert_contains "${L}" "NRestarts=0" "hub NRestarts checked"
assert_contains "${L}" "worker process " "worker process found despite empty/zero MainPID polls"
[ ! -f "${FAKE_ROOT}/state/mainpid-faults" ] && case_ok "all injected MainPID faults were consumed by the wait" || case_fail "faults left: $(cat "${FAKE_ROOT}/state/mainpid-faults")"
IMMUTABLE_N="$(find "${WEB_RELEASES}/${SHA_PREV}/_app/immutable" -type f | wc -l | tr -d ' ')"
assert_contains "${L}" "chunk merge: every one of ${IMMUTABLE_N} immutable files of ${SHA_PREV} is present in ${SHA_NEW}" "old chunks merged"
[ -f "${WEB_RELEASES}/${SHA_NEW}/_app/immutable/chunks/old-tab.js" ] && case_ok "old tab chunk served from the new release" || case_fail "old chunk missing"
assert_eq "$(cat "${WEB_RELEASES}/${SHA_NEW}/_app/version.json")" "{\"version\":\"${SHA_NEW}\"}" "version.json not clobbered by the merge"
assert_contains "${L}" "every dist web file of ${SHA_NEW} is byte-identical" "dist files intact after the merge"
assert_contains "${L}" "maintenance: 2 live executions, each checked" "maintenance: sockets and exec dirs"
assert_contains "${L}" "maintenance: no current.new/build.new/.stage-* on any host" "maintenance: no leftovers"
assert_contains "${L}" "maintenance: canterlot.env.${SHA_NEW} is 0600 and equal to the live env" "maintenance: env record"
assert_contains "${L}" "probe: loopback caddy pid 4242 stopped" "loopback probe stop logged"
assert_contains "${L}" "port 18080 closed again" "loopback port proven closed"
assert_contains "${L}" "harness catalog has claude_code, codex, opencode" "post-start harness catalog checked"
assert_contains "${L}" "owner checklist: authenticated checks" "authenticated checks printed for the owner"
assert_contains "${L}" "check-proxy.sh https://hub.example.com --positive" "checklist names the self-contained sign-in check"
assert_contains "${L}" "dbip-city-lite.mmdb is 0 days old, no update" "geoip step ran after verify and kept the fresh database"
assert_not_contains "${L}" "SOFT-FAIL" "no soft failures"
# Порядок: vzdump при работающем всём, повторные ворота, стоп, дамп, хаб, воркер, PWA.
in_order "${C}" "vzdump ${HUB_CTID} --mode snapshot" "stop canterlot-worker" "pg_dump -Fc" "pg_restore --list" \
    "restart canterlot-hub" "start canterlot-worker" "compose up -d caddy" \
    && case_ok "commands: vzdump -> worker stop -> pg_dump -> dump verify -> hub restart -> worker start -> caddy recreate" \
    || case_fail "commands out of order"
in_order "${L}" "control-worker scan before vzdump: exactly the unit process" "vzdump done:" \
    "re-checking read gates and the session inventory after vzdump" \
    "recorded session PIDs and ticks intact after vzdump" "control-worker scan before the worker stop: exactly the unit process" \
    "stopping worker" "dump fully readable" "hub restarted once" 'worker state="live" after start' \
    "PWA step (all failures soft" "speech:" "chunk merge: " "running verification" \
    && case_ok "log: the new order, gates and inventory re-run between vzdump and the stop" \
    || case_fail "log out of order"
REGATES="$(sed -n '/vzdump done:/,/stopping worker/p' "${L}" | grep -c 'read gates for target' || true)"
[ "${REGATES}" -ge 1 ] && case_ok "read gates ran again between vzdump and the stop" || case_fail "no read gates between vzdump and the stop"
assert_contains "${L}" "downtime: worker " "worker downtime printed"
grep -qE 'downtime: worker [0-9]+ s \(stop -> state=live\), hub [0-9]+ s \(restart -> readyz ok\)' "${L}" \
    && case_ok "downtime line names both components" || case_fail "downtime line malformed: $(grep 'downtime:' "${L}")"
assert_contains "${L}" "hub interruption: " "hub interruption measured"
assert_eq "$(result_of "${R}" soft_failures)/$(result_of "${R}" speech_soft_failures)" "0/0" "result file: no soft failures"
[[ "$(result_of "${R}" worker_downtime_s)" =~ ^[0-9]+$ ]] && case_ok "result file: worker downtime" || case_fail "result file: worker downtime missing"
[[ "$(result_of "${R}" hub_interruption_s)" =~ ^[0-9]+$ ]] && case_ok "result file: hub interruption" || case_fail "result file: hub interruption missing"
assert_eq "$(result_of "${R}" vzdump_notes)" "$(sed -n 's/.*notes pre-\([^ ,)]*\).*/pre-\1/p' "${L}" | head -1)" "result file: vzdump notes"
teardown_case
case_end

# --- L2. второй проход счастливого пути: cutover.sh --fast (без токена и vzdump) на стенде с
# git-источником проходит те же ворота, предпроверку, инвентарь, дамп с проверкой чтения, порядок
# и verify (check_done_run), плюс свои развилки: условия пересчитаны, vzdump пропущен. До него —
# замок прогона: второй выкат и откат базы под чужим замком отказывают, ничего не тронув.
# (Бывшие кейсы test-fastcutover.sh «--fast: no token, no vzdump…» и «dump and restore name the
# same explicit socket», test-fastguards.sh «a second mutating run fails fast…»,
# test-fastbaseline.sh «the staged manifest runs as the hub service account…».)
case_begin "second pass of the happy path on the fast path: the same gates and checks, no token, no vzdump; a held deploy lock refuses first"
setup_fast fasthappy other
LOCKF="${WORKER_BASE}/.cutover.lock"
RC=0
flock "${LOCKF}" bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --fast --source "${FAST_SRC}" --dist "${FAKE_ROOT}/dist" \
    --state "${FAKE_ROOT}/snap.state" --result "${FAKE_ROOT}/x.result" >"${FAKE_ROOT}/cutover.log" 2>&1 || RC=$?
[ "${RC}" != "0" ] && case_ok "the cutover exits nonzero under a held lock" || case_fail "the cutover ran under a held lock"
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)/$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "refused/deploy_locked" "clear code deploy_locked"
assert_contains "${FAKE_ROOT}/cutover.log" "another cutover or rollback holds" "reason printed"
no_writes_yet "held lock"
printf 'TOC-LINE-02 _sqlx_migrations table\n' >"${HUB_BACKUP_DIR}/canterlot-pre-manual.dump"
RC=0
flock "${LOCKF}" bash "${DEPLOY_DIR}/rollback-db.sh" "${HUB_BACKUP_DIR}/canterlot-pre-manual.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rollback.log" 2>&1 || RC=$?
[ "${RC}" != "0" ] && case_ok "rollback-db.sh exits nonzero under a held lock" || case_fail "rollback-db.sh ran under a held lock"
assert_contains "${FAKE_ROOT}/rollback.log" "deploy_locked" "rollback-db.sh names the code"
grep -qE '^(dropdb|createdb) ' "${FAKE_ROOT}/state/calls.log" && case_fail "the rollback touched the database" || case_ok "the rollback touched nothing"
rm -f "${HUB_BACKUP_DIR}/canterlot-pre-manual.dump"  # lint: local-temp
# Замок свободен: тот же выкат проходит. Журналы — только этого прогона.
: >"${FAKE_ROOT}/state/calls.log"
: >"${FAKE_ROOT}/state/ssh-cmds.log"
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result"; then case_ok "exit 0 with the lock free"; else case_fail "exit nonzero"; tail -10 "${FAKE_ROOT}/cutover.log"; fi
C="${FAKE_ROOT}/state/calls.log"
L="${FAKE_ROOT}/cutover.log"
R="${FAKE_ROOT}/exe.result"
check_done_run "${R}"
assert_contains "${L}" "fast path: no confirm token" "the fast path is logged"
assert_contains "${L}" "allowlisted application code" "the recomputed conditions are logged"
assert_not_contains "${L}" "confirm token accepted" "no token was involved"
grep -q '^vzdump ' "${C}" && case_fail "vzdump ran" || case_ok "vzdump never ran"
assert_contains "${L}" "common preflight: preflight.sh and target-check.sh for ${SHA_NEW}" "the common preflight ran"
assert_contains "${L}" "target check PASS for ${SHA_NEW}" "the target check ran"
grep -q '^cargo test .*spec_compat::' "${C}" && case_ok "spec_compat ran in the preflight" || case_fail "spec_compat did not run"
assert_contains "${L}" "gate 5: worker KillMode, no vzdump, no CT lock" "preflight gate 5 ran"
assert_eq "$(result_of "${R}" vzdump_done)/$(result_of "${R}" vzdump_skipped)/$(result_of "${R}" confirm)" "no/yes/fast" "result: vzdump_done=no, vzdump_skipped=yes, confirm=fast"
[ -n "$(result_of "${R}" vzdump_skip_reason)" ] && case_ok "result: the skip reason is recorded" || case_fail "result: no skip reason"
grep -E '^pg_dump ' "${C}" | grep -q -- " -h ${HUB_PG_SOCKET} " && case_ok "pg_dump carries -h <socket>" || case_fail "pg_dump has no explicit -h ${HUB_PG_SOCKET}"
grep -E 'migrations manifest --json' "${FAKE_ROOT}/state/ssh-cmds.log" | grep -q "runuser -u '${HUB_SERVICE_USER:-canterlot}' -- timeout 30 " \
    && case_ok "runuser -u <service account> -- timeout 30 <staged binary> migrations manifest --json" || case_fail "the manifest is not run as the service account under timeout 30"
finish_fast
case_end

# --- L3. NRestarts воркера после старта не 0: STOP-and-report ---
case_begin "worker NRestarts after the start stops and reports"
setup_cutover nrestarts
# Служба диктовки установлена: отчёт называет её состояние и ручной шаг (бывший кейс
# test-speech.sh «stop report includes the speech unit»).
install_fake_speech "${SHA_PREV}"
install_fake_speech "${SHA_NEW}"
install_speech_unit
export FAKE_WORKER_NRESTARTS=2
if run_cutover; then case_fail "exit 0 with NRestarts=2"; else case_ok "exit nonzero"; fi
unset FAKE_WORKER_NRESTARTS
assert_contains "${FAKE_ROOT}/cutover.log" "worker NRestarts is '2', expected 0" "NRestarts reported"
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "post-flip stop-and-report"
assert_contains "${FAKE_ROOT}/cutover.log" "speech unit        = canterlot-speech: load=loaded active=active enabled=enabled config=present binary=present" "speech unit state in the report"
assert_contains "${FAKE_ROOT}/cutover.log" "restart canterlot-speech if the release has bin/canterlot-speech" "manual speech step after a hub flip back"
assert_contains "${FAKE_ROOT}/cutover.log" "remove [dictation] from" "[dictation] rollback rule printed"
assert_contains "${FAKE_ROOT}/cutover.log" "0091_dictation_terms" "dictation migration follows the DB restore row"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_fail "worker running (not proven good: must stop)" || case_ok "worker stays stopped"
teardown_case
case_end

# --- L3b. проверка после доказанно живого воркера: воркер не гасим ---
case_begin "a verifying-phase failure leaves hub, PWA and worker running"
setup_cutover verify-keeps-running
export FAKE_SITE_FAIL=meeting.example.com
if run_cutover; then case_fail "exit 0 despite a broken site"; else case_ok "exit nonzero"; fi
unset FAKE_SITE_FAIL
assert_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT (phase verifying)" "stop-and-report in the verifying phase"
assert_contains "${FAKE_ROOT}/cutover.log" "site meeting.example.com: 500, expected 200" "failure named"
assert_contains "${FAKE_ROOT}/cutover.log" "leaving hub, PWA and worker running" "kept the deployment running"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" hard_failures)" "2" "result file: both verify reads counted"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" fail_codes)" "verify_site" "result file: the verify failure coded"
hub_is "${SHA_NEW}" && case_ok "hub stays new" || case_fail "hub moved"
web_is "${SHA_NEW}" && case_ok "PWA stays new" || case_fail "web moved"
worker_is "${SHA_NEW}" && case_ok "worker stays new" || case_fail "worker moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker left running" || case_fail "worker was stopped after it was proven live"
assert_not_contains "${FAKE_ROOT}/cutover.log" "retain done" "retention skipped"
teardown_case
case_end

# --- L3c. precompressed и остатки на хостах — мягкие ошибки: выкат доходит до конца ---
# Один прогон на оба мягких сбоя (бывшие L3c и L6): каждый записан, ни один не останавливает выкат.
case_begin "a missing precompressed encoding and leftovers on a host are soft and the cutover completes"
setup_cutover encoding-soft
export FAKE_ENCODING_FAIL=zstd
mkdir -p "${WEB_RELEASES}/.stage-cccccccccccc"
if run_cutover; then case_ok "exit 0 (precompressed and maintenance are soft)"; else case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/cutover.log"; fi
unset FAKE_ENCODING_FAIL
assert_contains "${FAKE_ROOT}/cutover.log" "SOFT-FAIL: precompressed zstd: got 'none'" "encoding failure recorded softly"
assert_contains "${FAKE_ROOT}/cutover.log" "SOFT-FAIL: maintenance: leftovers on hosts: web: releases/.stage-cccccccccccc" "leftover named by the maintenance check"
[[ "$(result_of "${FAKE_ROOT}/cutover.result" soft_by_kind)" =~ (^|,)verify:[1-9][0-9]*(,|$) ]] \
    && case_ok "result file: verify.sh soft failures counted as verify" || case_fail "result file: soft_by_kind [$(result_of "${FAKE_ROOT}/cutover.result" soft_by_kind)]"
assert_contains "${FAKE_ROOT}/cutover.log" "cutover done:" "cutover finished"
assert_not_contains "${FAKE_ROOT}/cutover.log" "STOP-AND-REPORT" "no stop-and-report"
hub_is "${SHA_NEW}" && case_ok "hub on new" || case_fail "hub not new"
web_is "${SHA_NEW}" && case_ok "PWA on new" || case_fail "web not new"
worker_is "${SHA_NEW}" && case_ok "worker on new" || case_fail "worker not new"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker \
    && case_ok "worker running" || case_fail "worker stopped"
teardown_case
case_end

# --- L4. vzdump без «Backup job finished successfully»: до flip, только рестарт ---
case_begin "vzdump without its success line restarts the worker before any flip"
setup_cutover vzdump-nosuccess
export FAKE_VZDUMP_NO_SUCCESS=1
if run_cutover; then case_fail "exit 0"; else case_ok "exit nonzero"; fi
unset FAKE_VZDUMP_NO_SUCCESS
assert_contains "${FAKE_ROOT}/cutover.log" "vzdump did not report 'Backup job finished successfully'" "missing success line caught"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
worker_is "${SHA_PREV}" && case_ok "worker on prev" || case_fail "worker moved"
teardown_case
case_end

# --- L5. подозрительно маленький дамп: до flip ---
case_begin "a suspiciously small dump refuses the hub flip"
setup_cutover small-dump
export DUMP_MIN_BYTES=100000
if run_cutover; then case_fail "exit 0"; else case_ok "exit nonzero"; fi
export DUMP_MIN_BYTES=1
assert_contains "${FAKE_ROOT}/cutover.log" "suspiciously small" "small dump caught"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
teardown_case
case_end

printf 'cutover harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
