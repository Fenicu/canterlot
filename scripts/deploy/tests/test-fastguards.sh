#!/usr/bin/env bash
# Развилки быстрого пути в охране и восстановлении: чужое состояние веба, идентичность кластера
# и источник идентичности, закреплённая база (дрейф во время предпроверки и перед стопом, flip
# вперёд только из ожидаемого релиза), тексты восстановления без vzdump (STOP после flip, сбой
# чтения дампа до flip, forward-only после живого воркера) и дословный откат быстрого прогона.
# Замок прогона — во втором проходе счастливого пути test-cutover.sh; запись идентичности рядом
# с дампом, отказ отката на чужом кластере и повтор той же команды после частичного
# восстановления — там же, в кейсе readyz боевого прогона: команда отката одна для обоих путей.
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
# setup_fast, fcut, no_writes_yet, printed_rollback_cmd, run_printed — fast-source-fixture.sh.

# Чужое состояние веба: проверка ожидаемого прежнего релиза идёт ДО замены живого env, а
# при отказе откатывать нечего — чужое не перезаписывается.
case_begin "a foreign web release appearing before the env swap is left alone"
setup_fast webforeign other
mkdir -p "${WEB_RELEASES}/cccccccccccc/_app"
printf '#!/usr/bin/env bash\nln -sfn releases/cccccccccccc "%s"\nprintf %s > "%s"\n' "${WEB_BUILD_LINK}" "'CANTERLOT_CSP=\"FOREIGN\"\n'" "${WEB_ENV}" >"${FAKE_ROOT}/foreign-hook.sh"
export FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/foreign-hook.sh" FAKE_SSH_AFTER_CMD="cat > '${WEB_ENV}.${SHA_NEW}'"
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
unset FAKE_SSH_AFTER_HOOK FAKE_SSH_AFTER_CMD
assert_contains "${FAKE_ROOT}/cutover.log" "web link changed after approval" "the foreign web release is named"
assert_eq "$(readlink "${WEB_BUILD_LINK}")" "releases/cccccccccccc" "the foreign build link is untouched (not flipped back)"
grep -q FOREIGN "${WEB_ENV}" && case_ok "the foreign live env is untouched" || case_fail "the live env was overwritten"
assert_not_contains "${FAKE_ROOT}/cutover.log" "PWA build flipped back" "no flip back of what was not changed"
assert_not_contains "${FAKE_ROOT}/cutover.log" "WOULD-WRITE" "(sanity: a real run)"
hub_is "${SHA_NEW}" && worker_is "${SHA_NEW}" && case_ok "hub and worker are on the target (the PWA failure is soft)" || case_fail "hub or worker not on the target"
finish_fast
case_end

# ------------------------------------------------------------ идентичность кластера
case_begin "the guard compares the cluster system identifier, not only role and database name"
setup_fast idguard other
export HUB_PG_SYSTEM_ID=7000000000000000001 FAKE_PG_SYSID=7999999999999999999
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "exit 0 on another cluster with the same role and database"; else case_ok "exit nonzero"; fi
unset HUB_PG_SYSTEM_ID FAKE_PG_SYSID
assert_eq "$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "db_identity" "the identity check failed (not another guard)"
assert_contains "${FAKE_ROOT}/cutover.log" "system identifier 7999999999999999999" "the foreign system identifier is named"
assert_not_contains "${FAKE_ROOT}/cutover.log" "no connection of the running hub" "the hub-connection check passed: the wrong cluster is populated by the same role"
no_writes_yet "populated wrong cluster"
finish_fast
case_end

# ------------------------------------------------------------ быстрый путь без источника идентичности
# Полный путь без источника идентичности работает и записывает её: кейс счастливого пути
# test-cutover.sh (его стенд без записи идентичности).
case_begin "skip eligibility refuses the fast path when there is no identity source (pin or sidecar)"
setup_fast noid other
rm -f "${HUB_BACKUP_DIR}/canterlot-pre-seed-20261101T000000Z.dump.identity"  # lint: local-temp
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "the fast path ran without an identity source"; else case_ok "exit nonzero"; fi
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)/$(result_of "${FAKE_ROOT}/x.result" phase)/$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "refused/eligibility/fast_path_refused" "refused before any write with the fallback code"
assert_contains "${FAKE_ROOT}/cutover.log" "no identity source" "reason: no pin and no readable sidecar"
assert_not_contains "${FAKE_ROOT}/cutover.log" "connection(s) of the running hub" "the same-role connection count is not a substitute"
no_writes_yet "no identity source"
if fcut "${SHA_NEW}" --rehearse --skip-ct-backup --result "${FAKE_ROOT}/reh.result"; then case_fail "the skip rehearsal passed without an identity source"; else case_ok "the skip rehearsal refuses too"; fi
export HUB_PG_SYSTEM_ID=7000000000000000001
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" && case_ok "a pinned HUB_PG_SYSTEM_ID is an identity source" || case_fail "the pinned run failed"
unset HUB_PG_SYSTEM_ID
finish_fast
case_end

# ------------------------------------------------------------ базовое состояние закреплено
# (из бывшего test-fastbaseline.sh)
# Крючок, который «вручную» переключает ссылку веба на другой релиз.
make_drift_hook() {
    mkdir -p "${WEB_RELEASES}/cccccccccccc/_app"
    printf '#!/usr/bin/env bash\nln -sfn releases/cccccccccccc "%s"\n' "${WEB_BUILD_LINK}" >"${FAKE_ROOT}/drift-hook.sh"
}
case_begin "the baseline drifting during the preflight refuses the fast path before any write"
setup_fast driftpre other
make_drift_hook
export FAKE_CARGO_HOOK="${FAKE_ROOT}/drift-hook.sh"
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "exit 0 after a drift"; else case_ok "exit nonzero"; fi
unset FAKE_CARGO_HOOK
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)/$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "refused/fast_path_refused" "refused with the fallback code"
assert_contains "${FAKE_ROOT}/cutover.log" "web link" "the drifted link is named"
no_writes_yet "drift during the preflight"
finish_fast
case_end

case_begin "the baseline drifting during the preflight stops a full run too (baseline_drift)"
setup_fast driftfull migration
fast_to_full_migration
fcut "${SHA_NEW}" --rehearse --result "${FAKE_ROOT}/reh.result" || true
TOK="$(result_of "${FAKE_ROOT}/reh.result" token)"
make_drift_hook
export FAKE_CARGO_HOOK="${FAKE_ROOT}/drift-hook.sh"
if fcut --execute "${SHA_NEW}" --confirm "${TOK}" --result "${FAKE_ROOT}/x.result"; then case_fail "exit 0 after a drift"; else case_ok "exit nonzero"; fi
unset FAKE_CARGO_HOOK
assert_eq "$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "baseline_drift" "fail code baseline_drift"
no_writes_yet "full run drift"
finish_fast
case_end

case_begin "the baseline drifting after the regate and before the stop leaves the worker running"
setup_fast driftstop other
make_drift_hook
export FAKE_SSH_AFTER_HOOK="${FAKE_ROOT}/drift-hook.sh" FAKE_SSH_AFTER_CMD="select distinct conversation_id" FAKE_SSH_AFTER_NTH=2
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "exit 0 after a drift"; else case_ok "exit nonzero"; fi
unset FAKE_SSH_AFTER_HOOK FAKE_SSH_AFTER_CMD FAKE_SSH_AFTER_NTH
assert_eq "$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "baseline_drift" "fail code baseline_drift"
assert_contains "${FAKE_ROOT}/cutover.log" "changed after it was approved" "reason printed"
worker_stopped_ever && case_fail "worker stopped" || case_ok "worker never stopped"
grep -qE '^pg_dump ' "${FAKE_ROOT}/state/calls.log" && case_fail "pg_dump ran" || case_ok "no pg_dump"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
finish_fast
case_end

case_begin "the forward flip scripts refuse when the link is no longer the approved release"
setup_fast flipold other
FLIPDIR="${FAKE_ROOT}/flipdir"
mkdir -p "${FLIPDIR}/releases/old/bin" "${FLIPDIR}/releases/new/bin" "${FLIPDIR}/releases/other"
printf 'x' >"${FLIPDIR}/releases/new/RELEASE"; printf 'x' >"${FLIPDIR}/releases/new/SHA256SUMS"
printf '#!/bin/sh\n' >"${FLIPDIR}/releases/new/bin/canterlot-hub"; chmod +x "${FLIPDIR}/releases/new/bin/canterlot-hub"
mkdir -p "${FLIPDIR}/releases/new/_app"; printf '#!/bin/sh\n' >"${FLIPDIR}/releases/new/bin/canterlot-worker"
flip_run() { bash -c "$(bash -c 'source "$1/lib.sh"; printf "%s" "${!2}"' _ "${DEPLOY_DIR}" "$1")" -- "${@:2}"; }
ln -sfn releases/old "${FLIPDIR}/current"; ln -sfn releases/old "${FLIPDIR}/build"
flip_run HUB_FLIP_FWD_SCRIPT "${FLIPDIR}" new "${FLIPDIR}/current" other >/dev/null 2>&1 && case_fail "hub flip ignored the expected-old" || case_ok "hub flip refuses a link that is not the approved release"
assert_eq "$(readlink "${FLIPDIR}/current")" "releases/old" "hub link untouched"
flip_run HUB_FLIP_FWD_SCRIPT "${FLIPDIR}" new "${FLIPDIR}/current" old >/dev/null 2>&1 && case_ok "hub flip proceeds from the approved release" || case_fail "hub flip refused the approved release"
assert_eq "$(readlink "${FLIPDIR}/current")" "releases/new" "hub link moved"
ln -sfn releases/other "${FLIPDIR}/current"
flip_run WORKER_FLIP_FWD_SCRIPT "${FLIPDIR}" new old >/dev/null 2>&1 && case_fail "worker flip ignored the expected-old" || case_ok "worker flip refuses a link that is not the approved release"
ln -sfn releases/old "${FLIPDIR}/current"
flip_run WORKER_FLIP_FWD_SCRIPT "${FLIPDIR}" new old >/dev/null 2>&1 && case_ok "worker flip proceeds from the approved release" || case_fail "worker flip refused the approved release"
ln -sfn releases/other "${FLIPDIR}/build"
flip_run WEB_FLIP_FWD_SCRIPT "${FLIPDIR}" new "${FLIPDIR}/build" old >/dev/null 2>&1 && case_fail "web flip ignored the expected-old" || case_ok "web flip refuses a link that is not the approved release"
assert_eq "$(readlink "${FLIPDIR}/build")" "releases/other" "web link untouched"
finish_fast
case_end

# ------------------------------------------------------------ восстановление по записанным фактам
# STOP после flip хаба в быстром прогоне: тексты не называют несуществующий vzdump, а напечатанная
# команда отката дословно возвращает данные дампа и прежний хаб (бывшие «STOP-AND-REPORT after
# a fast run» и «the printed rollback of a fast run»: один прогон).
case_begin "STOP-AND-REPORT after a fast run names no vzdump, and its printed rollback runs verbatim"
setup_fast stopfast other
export FAKE_READYZ_NEW=0
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
unset FAKE_READYZ_NEW
STOP_BLOCK="$(result_of "${FAKE_ROOT}/exe.result" stop_block)"
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" status)" "stop_and_report" "result: stop_and_report"
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" vzdump_done)/$(result_of "${FAKE_ROOT}/exe.result" vzdump_skipped)" "no/yes" "result: vzdump_done=no, vzdump_skipped=yes"
[ -f "${STOP_BLOCK}" ] && case_ok "the stop block file exists" || case_fail "no stop block"
assert_contains "${STOP_BLOCK}" "vzdump done=no (skipped:" "facts: the vzdump is reported as skipped"
assert_contains "${STOP_BLOCK}" "this run's vzdump = none (skipped:" "live state: no vzdump named"
assert_not_contains "${STOP_BLOCK}" "notes pre-${SHA_NEW}-" "no vzdump notes in the block"
assert_not_contains "${STOP_BLOCK}" "The vzdump above" "recovery step 2 does not point at a vzdump"
assert_not_contains "${STOP_BLOCK}" "earlier whole-CT restore point" "no whole-CT restore point mentioned"
assert_contains "${STOP_BLOCK}" "rollback-db.sh" "the quiesced dump restore command is still printed"
assert_contains "${STOP_BLOCK}" "${HUB_BACKUP_DIR}/canterlot-pre-${SHA_NEW}-" "the dump path is printed"
printf 'MIGR_MAX=41\nFAILED=0\nchanged-after-the-dump\n' >"${FAKE_ROOT}/state/db.txt"
RECOVERY_CMD="$(printed_rollback_cmd)"
[ -n "${RECOVERY_CMD}" ] && run_printed || RC=1
[ "${RC}" = "0" ] && case_ok "the printed recovery command runs verbatim" || { case_fail "the printed recovery command failed"; tail -3 "${FAKE_ROOT}/rollback.log" 2>/dev/null; }
grep -q 'payload-predump' "${FAKE_ROOT}/state/db.txt" && case_ok "the restored data is the dump's" || case_fail "the data was not restored from the dump"
hub_is "${SHA_PREV}" && case_ok "recovery flipped the hub back" || case_fail "the hub was not flipped back"
finish_fast
case_end

case_begin "a skip-mode dump read-back failure before the flip: the planned dump is not a restore point"
setup_fast recdumpfail other
export FAKE_DUMP_CORRUPT=1
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
unset FAKE_DUMP_CORRUPT
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" status)" "failed_before_flip" "result: failed_before_flip"
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" dump_verified)/$(result_of "${FAKE_ROOT}/exe.result" dump)" "no/" "result: no verified dump named"
STOP_BLOCK="$(result_of "${FAKE_ROOT}/exe.result" stop_block)"
[ -f "${STOP_BLOCK}" ] && case_ok "the recovery report exists" || case_fail "no recovery report"
assert_contains "${STOP_BLOCK}" "this run's dump   = none usable" "the dump line says there is no usable dump"
assert_contains "${STOP_BLOCK}" "this run's vzdump = none (skipped:" "the vzdump line says skipped"
assert_not_contains "${STOP_BLOCK}" "verified readable before the hub flip)" "no claim of a verified dump"
assert_not_contains "${STOP_BLOCK}" "only restore point" "no sentence that calls an unverified dump the only restore point"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-worker && case_ok "the worker is back on its previous release" || case_fail "the worker is not running"
finish_fast
case_end

case_begin "a wrong cluster fails the dump before the flip and the manual restore refuses it"
setup_fast recwrongcluster other
export FAKE_PG_SOCKET=/run/another-cluster
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" status)" "failed_before_flip" "the dump on another cluster stops the run before the flip"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" --user stop canterlot-worker >/dev/null 2>&1 || true
# Фейковый юнит воркера после восстановления живёт процессом стенда (argv[1]=1000): гасим его, чтобы
# откат дошёл до проверки кластера, а не остановился на охраннике воркера.
for p in /proc/[0-9]*; do
    if [ "$(readlink "${p}/exe" 2>/dev/null | cut -c1-${#FAKE_ROOT})" = "${FAKE_ROOT}" ] && [ "$(tr '\0' ' ' <"${p}/cmdline" 2>/dev/null | awk '{print $2}')" = "1000" ]; then
        kill -KILL "${p#/proc/}" 2>/dev/null || true
    fi
done
command sleep 0.3
cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/db-before-restore.txt"
printf 'TOC-LINE-02 _sqlx_migrations table\n' >"${HUB_BACKUP_DIR}/canterlot-pre-manual.dump"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${HUB_BACKUP_DIR}/canterlot-pre-manual.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rollback.log" 2>&1; then case_fail "the restore ran on the wrong cluster"; else case_ok "rollback-db.sh refuses the wrong cluster"; fi
assert_not_contains "${FAKE_ROOT}/rollback.log" "worker is active" "(the worker guard was not what stopped it)"
assert_contains "${FAKE_ROOT}/rollback.log" "wrong cluster" "the failure is the cluster mismatch of the restore commands"
cmp -s "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/db-before-restore.txt" && case_ok "the database was not touched" || case_fail "the database changed"
unset FAKE_PG_SOCKET
finish_fast
case_end

case_begin "forward-only recovery after the worker is live (skip mode) names no vzdump"
setup_fast recforward other
export FAKE_SITE_FAIL=meeting.example.com
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
unset FAKE_SITE_FAIL
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" status)" "stop_and_report" "result: stop_and_report after the worker is live"
STOP_BLOCK="$(result_of "${FAKE_ROOT}/exe.result" stop_block)"
assert_contains "${STOP_BLOCK}" "recovery is forward-only" "forward-only recovery"
assert_contains "${STOP_BLOCK}" "this run's vzdump = none (skipped:" "no vzdump named"
assert_not_contains "${STOP_BLOCK}" "notes pre-${SHA_NEW}-" "no vzdump notes"
assert_not_contains "${STOP_BLOCK}" "The vzdump above" "no vzdump fallback text"
hub_is "${SHA_NEW}" && worker_is "${SHA_NEW}" && case_ok "hub and worker stay new" || case_fail "not on new"
finish_fast
case_end

printf '\nfastguards: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
