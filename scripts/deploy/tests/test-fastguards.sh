#!/usr/bin/env bash
# Замок прогона, идентичность кластера, чужое состояние веба и восстановление по
# записанным фактам для быстрого пути. Стенд и помощники — как в test-fastcutover.sh.
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

# Стенд на настоящем git-источнике; база уже на последней миграции релиза (0041), как
# при выпуске без новых миграций.
setup_fast() {
    local name="$1"
    shift
    make_fast_source "$@"
    setup_cutover "${name}"
    printf 'MIGR_MAX=41\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
    cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/state/db-pre.txt"
    # Развёрнутый релиз уже на 0041: так выглядит выпуск без новых миграций.
    sed -i 's/^migrations_last=.*/migrations_last=0041_fake_harness.sql/' \
        "${HUB_RELEASES}/${SHA_PREV}/RELEASE" "${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
    # Развёрнутый релиз хранит полный хэш исходника, как настоящая сборка.
    printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${HUB_RELEASES}/${SHA_PREV}/RELEASE"
    printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
    seed_identity_sidecar
    : >"${FAKE_ROOT}/state/calls.log"
}
finish_fast() { drop_fast_source; teardown_case; }
fcut() {
    # fcut РЕЖИМ... : cutover.sh с источником-репозиторием; ssh-команды пишутся в state/ssh-cmds.log
    FAKE_SSH_LOG_CMDS=1 bash "${DEPLOY_DIR}/cutover.sh" "$@" --source "${FAST_SRC}" --dist "${FAKE_ROOT}/dist" \
        --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1
}
rehearse_skip() { fcut "${SHA_NEW}" --rehearse --skip-ct-backup "$@"; }
# Токен репетиции из её файла результата.
token_of() { result_of "${FAKE_ROOT}/cutover.result" token; }


# Отказ быстрого пути до первой записи: ни маркеров резерва, ни остановок, ни дампов.
no_writes_yet() {
    local why="$1"
    worker_stopped_ever && case_fail "${why}: worker stopped" || case_ok "${why}: worker untouched"
    grep -qE '^(vzdump|pg_dump) ' "${FAKE_ROOT}/state/calls.log" && case_fail "${why}: vzdump or pg_dump ran" || case_ok "${why}: no vzdump, no pg_dump"
    # Маркеры резерва снимаются при выходе, поэтому по остатку их не видно: проверяем сами команды ssh.
    if grep -q '\.reserved\.' "${FAKE_ROOT}/state/ssh-cmds.log" 2>/dev/null; then
        case_fail "${why}: a reservation command was sent to a host"
    else
        case_ok "${why}: zero reservation writes (no reserve command reached any host)"
    fi
}

# ------------------------------------------------------------ один замок на весь прогон
case_begin "a second mutating run fails fast while another holds the deploy lock"
setup_fast lockheld other
LOCKF="${WORKER_BASE}/.cutover.lock"
RC=0
flock "${LOCKF}" bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --fast --source "${FAST_SRC}" --dist "${FAKE_ROOT}/dist" \
    --state "${FAKE_ROOT}/snap.state" --result "${FAKE_ROOT}/x.result" >"${FAKE_ROOT}/cutover.log" 2>&1 || RC=$?
[ "${RC}" != "0" ] && case_ok "the cutover exits nonzero" || case_fail "the cutover ran under a held lock"
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)/$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "refused/deploy_locked" "clear code deploy_locked"
assert_contains "${FAKE_ROOT}/cutover.log" "another cutover or rollback holds" "reason printed"
no_writes_yet "held lock"
printf 'TOC-LINE-02 _sqlx_migrations table\n' >"${HUB_BACKUP_DIR}/canterlot-pre-manual.dump"
RC=0
flock "${LOCKF}" bash "${DEPLOY_DIR}/rollback-db.sh" "${HUB_BACKUP_DIR}/canterlot-pre-manual.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rollback.log" 2>&1 || RC=$?
[ "${RC}" != "0" ] && case_ok "rollback-db.sh exits nonzero under a held lock" || case_fail "rollback-db.sh ran under a held lock"
assert_contains "${FAKE_ROOT}/rollback.log" "deploy_locked" "rollback-db.sh names the code"
grep -qE '^(dropdb|createdb) ' "${FAKE_ROOT}/state/calls.log" && case_fail "the rollback touched the database" || case_ok "the rollback touched nothing"
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" && case_ok "with the lock free the run completes" || case_fail "the run failed with a free lock"
finish_fast
case_end

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

case_begin "the dump carries the cluster identity and the printed restore command expects it"
setup_fast idrecord other
export FAKE_READYZ_NEW=0
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
unset FAKE_READYZ_NEW
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" pg_system_identifier)/$(result_of "${FAKE_ROOT}/exe.result" pg_database_oid)" "7000000000000000001/16384" "result: identity recorded"
IDFILE="$(result_of "${FAKE_ROOT}/exe.result" dump).identity"
[ -f "${IDFILE}" ] && grep -q '^system_identifier=7000000000000000001$' "${IDFILE}" && grep -q '^database_oid=16384$' "${IDFILE}" && case_ok "the identity file sits next to the dump" || case_fail "no identity file next to the dump"
STOP_BLOCK="$(result_of "${FAKE_ROOT}/exe.result" stop_block)"
assert_contains "${STOP_BLOCK}" "--expect-system-id 7000000000000000001 --expect-db-oid 16384" "the printed restore command carries the expected identity"
# Кластер с другим идентификатором: откат отказывает ДО остановки хаба и до любого dropdb;
# воркер при этом жив, значит отказ именно по идентичности, а не по охраннику воркера.
: >"${FAKE_ROOT}/state/calls.log"
export FAKE_PG_SYSID=7999999999999999999
RECOVERY_CMD="$(grep -m1 'scripts/deploy/rollback-db.sh' "${FAKE_ROOT}/cutover.log" | sed 's/^[[:space:]]*//')"
RC=0
( cd "${REPO_ROOT}" && eval "${RECOVERY_CMD}" ) >"${FAKE_ROOT}/rollback.log" 2>&1 || RC=$?
unset FAKE_PG_SYSID
[ "${RC}" != "0" ] && case_ok "the printed restore refuses a foreign cluster" || case_fail "the restore ran on a foreign cluster"
assert_contains "${FAKE_ROOT}/rollback.log" "system identifier" "refused on the system identifier"
assert_not_contains "${FAKE_ROOT}/rollback.log" "(before any change)" "not on the worker guard (it was never reached)"
grep -qE '^(dropdb|createdb) ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb or createdb ran" || case_ok "no dropdb, no createdb"
grep -q 'stop canterlot-hub' "${FAKE_ROOT}/state/calls.log" && case_fail "the hub was stopped" || case_ok "the hub was not stopped"
# Тот же кластер, другой oid базы.
export FAKE_PG_DBOID=99999
RC=0
( cd "${REPO_ROOT}" && eval "${RECOVERY_CMD}" ) >"${FAKE_ROOT}/rollback.log" 2>&1 || RC=$?
unset FAKE_PG_DBOID
[ "${RC}" != "0" ] && case_ok "a different database oid is refused too" || case_fail "the restore ran against another database oid"
assert_contains "${FAKE_ROOT}/rollback.log" "database oid" "refused on the database oid"
finish_fast
case_end

# ------------------------------------------------------------ быстрый путь без источника идентичности
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

case_begin "the full path keeps working without an identity source"
setup_fast noidfull migration
rm -f "${HUB_BACKUP_DIR}/canterlot-pre-seed-20261101T000000Z.dump.identity"  # lint: local-temp
printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
sed -i 's/^migrations_last=.*/migrations_last=0040_monthly_limits.sql/' "${HUB_RELEASES}/${SHA_PREV}/RELEASE" "${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
fcut "${SHA_NEW}" --rehearse --result "${FAKE_ROOT}/reh.result" || true
TOK="$(result_of "${FAKE_ROOT}/reh.result" token)"
fcut --execute "${SHA_NEW}" --confirm "${TOK}" --result "${FAKE_ROOT}/exe.result" || true
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" status)" "done" "the full run is done"
[ -f "$(result_of "${FAKE_ROOT}/exe.result" dump).identity" ] && case_ok "it records the identity for the next run" || case_fail "no identity recorded"
finish_fast
case_end

# ------------------------------------------------------------ откат переживает частичное восстановление
# Печатаемая команда отката после быстрого прогона со СТОПом: общий подготовительный шаг.
printed_rollback() {
    export FAKE_READYZ_NEW=0
    fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
    unset FAKE_READYZ_NEW
    RECOVERY_CMD="$(grep -m1 'scripts/deploy/rollback-db.sh' "${FAKE_ROOT}/cutover.log" | sed 's/^[[:space:]]*//')"
    PHASE_FILE="$(result_of "${FAKE_ROOT}/exe.result" dump).restore-phase"
    printf 'MIGR_MAX=41\nFAILED=0\nchanged-after-the-dump\n' >"${FAKE_ROOT}/state/db.txt"
}
run_printed() { RC=0; ( cd "${REPO_ROOT}" && eval "${RECOVERY_CMD}" ) >"${FAKE_ROOT}/rollback.log" 2>&1 || RC=$?; }

case_begin "pg_restore fails after createdb: the same printed command is rerun and restores"
setup_fast rbcreated other
printed_rollback
export FAKE_FAIL_PG_RESTORE=1
run_printed
unset FAKE_FAIL_PG_RESTORE
[ "${RC}" != "0" ] && case_ok "the first run fails" || case_fail "the first run did not fail"
assert_eq "$(cut -d' ' -f1-2 "${PHASE_FILE}" 2>/dev/null)" "created 16500" "the phase file records the replacement oid"
run_printed
assert_eq "${RC}" "0" "the rerun of the same printed command succeeds"
[ "${RC}" = "0" ] || tail -5 "${FAKE_ROOT}/rollback.log"
grep -q 'payload-predump' "${FAKE_ROOT}/state/db.txt" && case_ok "the data is restored from the dump" || case_fail "the data was not restored"
FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" is-active --quiet canterlot-hub && case_ok "the hub is started" || case_fail "the hub is not running"
assert_contains "${FAKE_ROOT}/rollback.log" "replacement database oid 16500" "the recorded replacement oid was accepted"
assert_eq "$(cut -d' ' -f1 "${PHASE_FILE}")" "restored" "the phase ends as restored"
finish_fast
case_end

case_begin "a failure between dropdb and createdb: the rerun continues from dropped"
setup_fast rbdropped other
printed_rollback
export FAKE_FAIL_CREATEDB=1
run_printed
unset FAKE_FAIL_CREATEDB
[ "${RC}" != "0" ] && case_ok "the first run fails" || case_fail "the first run did not fail"
assert_eq "$(cut -d' ' -f1 "${PHASE_FILE}" 2>/dev/null)" "dropped" "the phase file says dropped"
[ ! -f "${FAKE_ROOT}/state/db.txt" ] && case_ok "the database is really missing" || case_fail "the database exists"
run_printed
assert_eq "${RC}" "0" "the rerun succeeds with the database missing"
[ "${RC}" = "0" ] || tail -5 "${FAKE_ROOT}/rollback.log"
grep -q 'payload-predump' "${FAKE_ROOT}/state/db.txt" && case_ok "the data is restored from the dump" || case_fail "the data was not restored"
finish_fast
case_end

case_begin "an unrecorded foreign oid is still refused, with or without a phase record"
setup_fast rbforeign other
printed_rollback
export FAKE_PG_DBOID=77777
: >"${FAKE_ROOT}/state/calls.log"
run_printed
[ "${RC}" != "0" ] && case_ok "no phase record: refused" || case_fail "the foreign oid was accepted"
assert_contains "${FAKE_ROOT}/rollback.log" "database oid" "refused on the oid"
printf 'created 16500\n' >"${PHASE_FILE}"
run_printed
[ "${RC}" != "0" ] && case_ok "a phase record for another oid does not excuse 77777" || case_fail "the foreign oid was accepted under a phase record"
unset FAKE_PG_DBOID
grep -qE '^(dropdb|createdb) ' "${FAKE_ROOT}/state/calls.log" && case_fail "dropdb or createdb ran" || case_ok "the database was never touched"
finish_fast
case_end

# ------------------------------------------------------------ восстановление по записанным фактам
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

case_begin "the printed rollback of a fast run runs verbatim and restores the data"
setup_fast recrollback other
export FAKE_READYZ_NEW=0
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
unset FAKE_READYZ_NEW
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" status)" "stop_and_report" "result: stop_and_report"
printf 'MIGR_MAX=41\nFAILED=0\nchanged-after-the-dump\n' >"${FAKE_ROOT}/state/db.txt"
RECOVERY_CMD="$(grep -m1 'scripts/deploy/rollback-db.sh' "${FAKE_ROOT}/cutover.log" | sed 's/^[[:space:]]*//')"
if [ -n "${RECOVERY_CMD}" ] && ( cd "${REPO_ROOT}" && eval "${RECOVERY_CMD}" ) >"${FAKE_ROOT}/recovery.log" 2>&1; then case_ok "the printed recovery command runs verbatim"; else case_fail "the printed recovery command failed"; tail -3 "${FAKE_ROOT}/recovery.log" 2>/dev/null; fi
grep -q 'payload-predump' "${FAKE_ROOT}/state/db.txt" && case_ok "the restored data is the dump's" || case_fail "the data was not restored from the dump"
hub_is "${SHA_PREV}" && case_ok "recovery flipped the hub back" || case_fail "the hub was not flipped back"
finish_fast
case_end

printf '\nfastguards: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
