#!/usr/bin/env bash
# Развилки быстрого пути cutover.sh в условиях и токене: --skip-ct-backup (без vzdump, токен
# привязан к флагу, чекауту и набору инструментов) и --fast (без токена, условия пересчитываются
# из --source), отказы до первой записи с кодом fast_path_refused и нулём резервов, расхождения
# миграций migration_mismatch, манифест против RELEASE, зависший бинарь манифеста.
# Успешный прогон --fast с теми же воротами, предпроверкой и дампом, что у боевого, — второй
# проход счастливого пути в test-cutover.sh; замок, идентичность кластера, дрейф базы и тексты
# восстановления без vzdump — test-fastguards.sh.
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
# setup_fast, fcut, rehearse_skip, no_writes_yet — fast-source-fixture.sh.
L="${FAKE_ROOT:-}"

# ------------------------------------------------------------ --skip-ct-backup с токеном
# Один стенд: репетиция с флагом печатает команду боевого прогона с флагом (бывший отдельный
# кейс), токены с флагом и без него не взаимозаменяемы, токен с флагом доводит выкат до done.
case_begin "--skip-ct-backup: the rehearsal plans no vzdump and prints the flag, the token fits only the same flag"
setup_fast skip tooling
if rehearse_skip --result "${FAKE_ROOT}/reh.result"; then case_ok "rehearsal exit 0"; else case_fail "rehearsal failed"; tail -8 "${FAKE_ROOT}/cutover.log"; fi
L="${FAKE_ROOT}/cutover.log"
assert_not_contains "${L}" "(vzdump):" "no vzdump WOULD-WRITE"
assert_contains "${L}" "vzdump skipped" "the skip is logged"
assert_contains "${L}" "|   2 vzdump of CT ${HUB_CTID} skipped" "the plan text says the vzdump is skipped"
grep '^  real run:' "${L}" | grep -q -- '--skip-ct-backup' && case_ok "the printed real run of the rehearsal keeps --skip-ct-backup" || case_fail "the printed command drops the flag"
assert_eq "$(result_of "${FAKE_ROOT}/reh.result" vzdump_skipped)" "yes" "result: vzdump_skipped=yes"
SKIP_TOKEN="$(result_of "${FAKE_ROOT}/reh.result" token)"
[ -n "${SKIP_TOKEN}" ] && case_ok "a token is issued" || case_fail "no token"
# Токен репетиции без флага не годится для боевого прогона с флагом и наоборот.
if fcut "${SHA_NEW}" --rehearse --result "${FAKE_ROOT}/reh-full.result"; then case_ok "plain rehearsal exit 0"; else case_fail "plain rehearsal failed"; fi
FULL_TOKEN="$(result_of "${FAKE_ROOT}/reh-full.result" token)"
assert_eq "$([ "${FULL_TOKEN#*-}" = "${SKIP_TOKEN#*-}" ] && echo same || echo different)" "different" "the two plans hash differently"
if fcut --execute "${SHA_NEW}" --confirm "${FULL_TOKEN}" --skip-ct-backup --result "${FAKE_ROOT}/x.result"; then case_fail "the full-plan token opened a skip-vzdump run"; else case_ok "the full-plan token is refused with --skip-ct-backup"; fi
assert_eq "$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "token_plan_changed" "refused as plan changed"
if fcut --execute "${SHA_NEW}" --confirm "${SKIP_TOKEN}" --result "${FAKE_ROOT}/x.result"; then case_fail "the skip-vzdump token opened a full run"; else case_ok "the skip-vzdump token is refused without the flag"; fi
worker_stopped_ever && case_fail "worker stopped by a refused run" || case_ok "refused runs touched nothing"
# Боевой прогон по токену репетиции с флагом.
if fcut --execute "${SHA_NEW}" --confirm "${SKIP_TOKEN}" --skip-ct-backup --result "${FAKE_ROOT}/exe.result"; then case_ok "execute exit 0"; else case_fail "execute failed"; tail -8 "${FAKE_ROOT}/cutover.log"; fi
C="${FAKE_ROOT}/state/calls.log"
grep -q '^vzdump ' "${C}" && case_fail "vzdump ran" || case_ok "vzdump never ran"
grep -q '^pg_dump ' "${C}" && case_ok "pg_dump ran" || case_fail "no pg_dump"
assert_contains "${L}" "dump fully readable" "the quiesced dump was read back in full"
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" status)" "done" "result: done"
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" vzdump_done)/$(result_of "${FAKE_ROOT}/exe.result" vzdump_skipped)" "no/yes" "result: vzdump_done=no, vzdump_skipped=yes"
case "$(result_of "${FAKE_ROOT}/exe.result" vzdump_skip_reason)" in *"equals the live database"*) case_ok "result: the skip reason names the migrations" ;; *) case_fail "result: reason [$(result_of "${FAKE_ROOT}/exe.result" vzdump_skip_reason)]" ;; esac
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" vzdump_notes)$(result_of "${FAKE_ROOT}/exe.result" vzdump_archive)" "" "result: no vzdump notes or archive"
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" confirm)" "token" "result: confirmed by a token"
assert_not_contains "${L}" "notes pre-${SHA_NEW}-" "no vzdump notes anywhere in the log"
assert_contains "${L}" "restore points: pg_dump canterlot-pre-${SHA_NEW}-" "the restore point line names only the pg_dump"
hub_is "${SHA_NEW}" && worker_is "${SHA_NEW}" && web_is "${SHA_NEW}" && case_ok "all on the target" || case_fail "not all on the target"
finish_fast
case_end

# ------------------------------------------------------------ --skip-ct-backup при новой миграции
case_begin "--skip-ct-backup is refused when a migration is new (rehearsal and real run)"
setup_fast skipmig migration
if rehearse_skip --result "${FAKE_ROOT}/reh.result"; then case_fail "rehearsal accepted --skip-ct-backup with a new migration"; else case_ok "rehearsal refuses"; fi
assert_contains "${FAKE_ROOT}/cutover.log" "crates/canterlot-hub/migrations/9999_fixture.sql" "the new migration is named"
assert_eq "$(result_of "${FAKE_ROOT}/reh.result" fail_codes)" "fast_path_refused" "fail code fast_path_refused"
assert_eq "$(result_of "${FAKE_ROOT}/reh.result" token)" "" "no token"
fcut "${SHA_NEW}" --rehearse --result "${FAKE_ROOT}/reh-full.result" || true
if fcut --execute "${SHA_NEW}" --confirm "$(result_of "${FAKE_ROOT}/reh-full.result" token)" --skip-ct-backup --result "${FAKE_ROOT}/x.result"; then
    case_fail "the real run accepted --skip-ct-backup with a new migration"; else case_ok "the real run refuses"; fi
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)" "refused" "result: refused before any write (the plan token no longer fits, or the eligibility fails)"
worker_stopped_ever && case_fail "worker stopped" || case_ok "worker untouched"
finish_fast
case_end

# ------------------------------------------------------------ --fast: отказы до первой записи
refused_fast() {
    # refused_fast ПОЯСНЕНИЕ ОБРАЗЕЦ_ПРИЧИНЫ — прогон из cut_args в $@ после --
    local why="$1" reason="$2" R="${FAKE_ROOT}/x.result"
    shift 2
    if fcut --execute "${SHA_NEW}" --fast --result "${R}" "$@"; then case_fail "${why}: exit 0"; else case_ok "${why}: exit nonzero"; fi
    assert_eq "$(result_of "${R}" status)/$(result_of "${R}" phase)" "refused/eligibility" "${why}: refused before any write"
    assert_eq "$(result_of "${R}" fail_codes)" "fast_path_refused" "${why}: fail code fast_path_refused"
    assert_contains "${FAKE_ROOT}/cutover.log" "${reason}" "${why}: reason printed (${reason})"
    assert_eq "$(result_of "${R}" vzdump_done)/$(result_of "${R}" worker_stopped)" "no/no" "${why}: nothing was done"
    worker_stopped_ever && case_fail "${why}: worker stopped" || case_ok "${why}: worker untouched"
    grep -qE '^(vzdump|pg_dump) ' "${FAKE_ROOT}/state/calls.log" && case_fail "${why}: vzdump or pg_dump ran" || case_ok "${why}: no vzdump, no pg_dump"
}
case_begin "--fast is refused when deploy tooling changed"
setup_fast fasttool tooling
refused_fast "tooling" "scripts/deploy/tests/note.txt"
finish_fast
case_end
case_begin "--fast is refused when a migration is new"
setup_fast fastmig migration
refused_fast "migration" "crates/canterlot-hub/migrations/9999_fixture.sql"
finish_fast
case_end
case_begin "--fast is refused when release.sh changed"
setup_fast fastrel release-sh
refused_fast "release.sh" "scripts/release.sh"
finish_fast
case_end
case_begin "--fast is refused when the source does not know the live release"
setup_fast fastunknown other
SHALLOW="${FAKE_ROOT}/shallow-src"
git clone -q --depth 1 "file://${FAST_SRC}" "${SHALLOW}"
FAST_SRC_REAL="${FAST_SRC}"
FAST_SRC="${SHALLOW}"
refused_fast "unknown revision" "could not be classified"
FAST_SRC="${FAST_SRC_REAL}"
finish_fast
case_end
# Манифест целевого хаба против базы: версия, которой в базе нет, — полный путь (отказ до записи).
check_pending() {
    local why="$1"
    if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "${why}: exit 0"; else case_ok "${why}: exit nonzero"; fi
    assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)/$(result_of "${FAKE_ROOT}/x.result" phase)/$(result_of "${FAKE_ROOT}/x.result" fail_codes)" \
        "refused/eligibility/fast_path_refused" "${why}: refused with the pre-write status"
    assert_contains "${FAKE_ROOT}/cutover.log" "the target hub binary embeds migrations the database has not applied" "${why}: reason printed"
    no_writes_yet "${why}"
}
case_begin "--fast is refused before any write when the database lacks a migration the binary embeds"
setup_fast fastdb other
printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
check_pending "database at 40, manifest at 41"
# --skip-ct-backup: репетиция тоже отказывает, токена нет; dry-run тоже.
if fcut "${SHA_NEW}" --rehearse --skip-ct-backup --result "${FAKE_ROOT}/reh.result"; then case_fail "the skip rehearsal passed"; else case_ok "the skip rehearsal fails"; fi
assert_eq "$(result_of "${FAKE_ROOT}/reh.result" token)/$(result_of "${FAKE_ROOT}/reh.result" fail_codes)" "/fast_path_refused" "rehearsal: no token, fast_path_refused"
if fcut "${SHA_NEW}" --dry-run --skip-ct-backup --result "${FAKE_ROOT}/dry.result"; then case_fail "the skip dry-run passed"; else case_ok "the skip dry-run fails"; fi
assert_eq "$(result_of "${FAKE_ROOT}/dry.result" token)/$(result_of "${FAKE_ROOT}/dry.result" fail_codes)" "/fast_path_refused" "dry-run: no token, fast_path_refused"
finish_fast
case_end

case_begin "--fast is refused when a version below the maximum is missing from the database"
setup_fast fastgap other
for v in $(seq 1 41); do [ "${v}" = 20 ] || printf '%s c%s 1\n' "${v}" "${v}"; done >"${FAKE_ROOT}/state/db-migrations.txt"
check_pending "version 20 missing, maximum 41"
finish_fast
case_end

# Расхождения, которые нельзя лечить полным путём: ручной разбор, код migration_mismatch.
check_mismatch() {
    local why="$1" want="$2"
    if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "${why}: exit 0"; else case_ok "${why}: exit nonzero"; fi
    assert_eq "$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "migration_mismatch" "${why}: hard stop code migration_mismatch (not the fallback code)"
    assert_contains "${FAKE_ROOT}/cutover.log" "${want}" "${why}: reason printed"
    no_writes_yet "${why}"
}
case_begin "--fast hard-stops on a checksum mismatch, an unknown applied version and a failed row"
setup_fast fastbad other
for v in $(seq 1 41); do if [ "${v}" = 7 ]; then printf '7 e7 1\n'; else printf '%s c%s 1\n' "${v}" "${v}"; fi; done >"${FAKE_ROOT}/state/db-migrations.txt"
check_mismatch "checksum of version 7 differs" "checksum differs from the target binary: 7"
for v in $(seq 1 41); do printf '%s c%s 1\n' "${v}" "${v}"; done >"${FAKE_ROOT}/state/db-migrations.txt"
printf '42 c42 1\n' >>"${FAKE_ROOT}/state/db-migrations.txt"
check_mismatch "the database has version 42, the binary does not" "migrations the target binary lacks: 42"
for v in $(seq 1 40); do printf '%s c%s 1\n' "${v}" "${v}"; done >"${FAKE_ROOT}/state/db-migrations.txt"
printf '41 c41 0\n' >>"${FAKE_ROOT}/state/db-migrations.txt"
check_mismatch "version 41 failed" "failed migration rows in the database: 41"
finish_fast
case_end

case_begin "--fast refuses a database that is not the one the running hub is connected to"
setup_fast fastcluster other
export FAKE_HUB_CONNS=0
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "exit 0 on the wrong cluster"; else case_ok "exit nonzero"; fi
unset FAKE_HUB_CONNS
assert_eq "$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "db_identity" "fail code db_identity"
assert_contains "${FAKE_ROOT}/cutover.log" "no connection of the running hub" "reason printed"
no_writes_yet "wrong cluster"
finish_fast
case_end

case_begin "--fast refuses a checkout that is not at the target, and a target whose source id is not the commit"
setup_fast fastbound other
git -C "${FAST_SRC}" checkout -q "${FAST_LIVE_FULL}"
refused_fast "checkout at the live commit" "not at the target"
finish_fast
case_end

case_begin "--fast cannot be combined with a token, a rehearsal or a dry-run"
setup_fast fastargs other
if fcut --execute "${SHA_NEW}" --fast --confirm 1234567890-aaaaaaaaaaaa-bbbbbbbb; then case_fail "--fast with --confirm accepted"; else case_ok "--fast with --confirm refused"; fi
assert_contains "${FAKE_ROOT}/cutover.log" "--fast replaces --confirm" "reason printed"
if fcut "${SHA_NEW}" --rehearse --fast; then case_fail "--fast with --rehearse accepted"; else case_ok "--fast with --rehearse refused"; fi
if fcut "${SHA_NEW}" --dry-run --fast; then case_fail "--fast with --dry-run accepted"; else case_ok "--fast with --dry-run refused"; fi
worker_stopped_ever && case_fail "worker stopped" || case_ok "worker untouched"
finish_fast
case_end

# ------------------------------------------------------------ артефакт и манифест согласованы до записи
# (из бывшего test-fastbaseline.sh)
case_begin "a manifest that contradicts RELEASE migrations_last stops before any write"
setup_fast manifestrel other
sed -i 's/^migrations_last=.*/migrations_last=0042_contradiction.sql/' "${DIST_DIR}/RELEASE"
dist_resync
stage_cutover_dist
for mode in "--execute ${SHA_NEW} --fast" "${SHA_NEW} --rehearse --skip-ct-backup" "${SHA_NEW} --dry-run --skip-ct-backup"; do
    # shellcheck disable=SC2086 # режим — несколько слов
    if fcut ${mode} --result "${FAKE_ROOT}/x.result"; then case_fail "${mode}: exit 0"; else case_ok "${mode}: refused"; fi
    assert_eq "$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "manifest_release_mismatch" "${mode}: invalid artifact code (no fallback to the full path)"
    assert_contains "${FAKE_ROOT}/cutover.log" "42" "${mode}: the RELEASE value is named"
done
assert_eq "$(result_of "${FAKE_ROOT}/x.result" token)" "" "the dry-run gives no token"
no_writes_yet "contradictory migrations_last"
finish_fast
case_end

case_begin "a staged binary that hangs on the manifest is refused at the timeout"
setup_fast manifesthang other
: >"${FAKE_ROOT}/state/manifest-hang"
export MANIFEST_TIMEOUT=1
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "exit 0 with a hanging manifest"; else case_ok "exit nonzero"; fi
unset MANIFEST_TIMEOUT
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)/$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "refused/fast_path_refused" "refused before any write, fallback code"
no_writes_yet "hanging manifest"
finish_fast
case_end

# ------------------------------------------------------------ одно и то же состояние git в обоих режимах пропуска
case_begin "--skip-ct-backup binds the checkout, the toolkit and the token plan to the target"
setup_fast skipgit tooling
if fcut "${SHA_NEW}" --rehearse --skip-ct-backup --result "${FAKE_ROOT}/reh.result"; then case_ok "the rehearsal passes on the target checkout"; else case_fail "rehearsal failed"; tail -5 "${FAKE_ROOT}/cutover.log"; fi
SKIP_TOKEN="$(result_of "${FAKE_ROOT}/reh.result" token)"
OTHER_SRC="${FAKE_ROOT}/other-checkout"
git clone -q "${FAST_SRC}" "${OTHER_SRC}"
git -C "${OTHER_SRC}" checkout -q "${FAST_LIVE_FULL}"
if bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "${SKIP_TOKEN}" --skip-ct-backup --source "${OTHER_SRC}" \
        --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" --result "${FAKE_ROOT}/x.result" >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_fail "a token run with another --source was accepted"
else
    case_ok "a token run with another --source is refused"
fi
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)/$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "refused/token_plan_changed" "the checkout identity is part of the plan"
no_writes_yet "another --source"
if bash "${DEPLOY_DIR}/cutover.sh" "${SHA_NEW}" --rehearse --skip-ct-backup --source "${OTHER_SRC}" --dist "${FAKE_ROOT}/dist" \
        --state "${FAKE_ROOT}/snap.state" --result "${FAKE_ROOT}/y.result" >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_fail "a skip rehearsal on a checkout that is not at the target passed"
else
    case_ok "a skip rehearsal on a checkout that is not at the target fails"
fi
assert_contains "${FAKE_ROOT}/cutover.log" "not at the target" "reason: the checkout is not at the target"
finish_fast
case_end

case_begin "the command printed by a successful skip dry-run runs verbatim to done"
setup_fast dryprint tooling
fcut "${SHA_NEW}" --dry-run --skip-ct-backup --result "${FAKE_ROOT}/dry.result" && case_ok "the skip dry-run passes" || { case_fail "the skip dry-run failed"; tail -5 "${FAKE_ROOT}/cutover.log"; }
PRINTED="$(grep -m1 '^  real run:' "${FAKE_ROOT}/cutover.log" | sed 's/^[[:space:]]*real run:[[:space:]]*//')"
[ -n "${PRINTED}" ] && case_ok "a real-run command was printed" || case_fail "no real-run command printed"
case "${PRINTED}" in *"--skip-ct-backup"*) case_ok "the printed command keeps --skip-ct-backup" ;; *) case_fail "the printed command drops the flag: ${PRINTED}" ;; esac
RC=0
( cd "${REPO_ROOT}" && eval "${PRINTED} --state ${FAKE_ROOT}/snap.state --result ${FAKE_ROOT}/exe.result" ) >"${FAKE_ROOT}/printed.log" 2>&1 || RC=$?
assert_eq "${RC}" "0" "the printed command exits 0"
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" status)/$(result_of "${FAKE_ROOT}/exe.result" vzdump_skipped)" "done/yes" "the printed command completed the cutover without a vzdump"
finish_fast
case_end

printf '\nfastcutover: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
