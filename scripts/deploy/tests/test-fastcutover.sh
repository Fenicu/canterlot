#!/usr/bin/env bash
# cutover.sh без vzdump (--skip-ct-backup) и без токена (--fast): условия пересчитываются
# из --source, все ворота и предпроверки боевого прогона остаются, quiesced pg_dump с
# проверкой чтения — точка отката. Любое сомнение — отказ до первой записи.
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
    : >"${FAKE_ROOT}/state/calls.log"
}
finish_fast() { teardown_case; drop_fast_source; }
fcut() {
    # fcut РЕЖИМ... : cutover.sh с источником-репозиторием
    bash "${DEPLOY_DIR}/cutover.sh" "$@" --source "${FAST_SRC}" --dist "${FAKE_ROOT}/dist" \
        --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1
}
rehearse_skip() { fcut "${SHA_NEW}" --rehearse --skip-ct-backup "$@"; }
# Токен репетиции из её файла результата.
token_of() { result_of "${FAKE_ROOT}/cutover.result" token; }
L="${FAKE_ROOT:-}"

# ------------------------------------------------------------ --skip-ct-backup с токеном
case_begin "--skip-ct-backup: the rehearsal plans no vzdump, the token fits only the same flag"
setup_fast skip tooling
if rehearse_skip --result "${FAKE_ROOT}/reh.result"; then case_ok "rehearsal exit 0"; else case_fail "rehearsal failed"; tail -8 "${FAKE_ROOT}/cutover.log"; fi
L="${FAKE_ROOT}/cutover.log"
assert_not_contains "${L}" "(vzdump):" "no vzdump WOULD-WRITE"
assert_contains "${L}" "vzdump skipped" "the skip is logged"
assert_contains "${L}" "|   2 vzdump of CT ${HUB_CTID} skipped" "the plan text says the vzdump is skipped"
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
case "$(result_of "${FAKE_ROOT}/exe.result" vzdump_skip_reason)" in *"no migration"*) case_ok "result: the skip reason names the migrations" ;; *) case_fail "result: reason [$(result_of "${FAKE_ROOT}/exe.result" vzdump_skip_reason)]" ;; esac
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
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)/$(result_of "${FAKE_ROOT}/x.result" fail_codes)" "refused/fast_path_refused" "result: refused before any write"
worker_stopped_ever && case_fail "worker stopped" || case_ok "worker untouched"
finish_fast
case_end

# ------------------------------------------------------------ --fast
case_begin "--fast: no token, no vzdump, every gate and preflight still runs, the pg_dump is verified"
setup_fast fast other
cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/db-before.txt"
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result"; then case_ok "exit 0"; else case_fail "failed"; tail -10 "${FAKE_ROOT}/cutover.log"; fi
L="${FAKE_ROOT}/cutover.log"
C="${FAKE_ROOT}/state/calls.log"
assert_contains "${L}" "fast path: no confirm token" "the fast path is logged"
assert_contains "${L}" "no migration and no deploy tooling change" "the recomputed conditions are logged"
assert_not_contains "${L}" "confirm token accepted" "no token was involved"
grep -q '^vzdump ' "${C}" && case_fail "vzdump ran" || case_ok "vzdump never ran"
assert_contains "${L}" "common preflight: preflight.sh and target-check.sh for ${SHA_NEW}" "the common preflight ran"
assert_contains "${L}" "target check PASS for ${SHA_NEW}" "the target check ran"
grep -q '^cargo test .*spec_compat::' "${C}" && case_ok "spec_compat ran in the preflight" || case_fail "spec_compat did not run"
assert_contains "${L}" "gate 5: worker KillMode, no vzdump, no CT lock" "preflight gate 5 ran"
READ_GATES="$(grep -c 'read gates for target' "${L}" || true)"
[ "${READ_GATES}" -ge 2 ] && case_ok "read gates ran before the writes and again before the stop (${READ_GATES}x)" || case_fail "read gates ran ${READ_GATES}x"
assert_contains "${L}" "the session inventory" "the session inventory was re-checked"
assert_contains "${L}" "control-worker scan before the worker stop: exactly the unit process" "the control-worker scan ran"
assert_contains "${L}" "dump fully readable" "the quiesced dump was read back in full"
grep -q '^pg_dump ' "${C}" && grep -q '^pg_restore --list' "${C}" && case_ok "pg_dump taken and verified with pg_restore --list" || case_fail "pg_dump or its check missing"
in_order "${C}" "stop canterlot-worker" "pg_dump -Fc" "pg_restore --list" "restart canterlot-hub" "start canterlot-worker" "compose up -d caddy" \
    && case_ok "stop -> pg_dump -> verify -> hub -> worker -> PWA" || case_fail "commands out of order"
assert_contains "${L}" "verify PASS" "verify ran"
R="${FAKE_ROOT}/exe.result"
assert_eq "$(result_of "${R}" status)" "done" "result: done"
assert_eq "$(result_of "${R}" vzdump_done)/$(result_of "${R}" vzdump_skipped)/$(result_of "${R}" confirm)" "no/yes/fast" "result: vzdump_done=no, vzdump_skipped=yes, confirm=fast"
[ -n "$(result_of "${R}" vzdump_skip_reason)" ] && case_ok "result: the skip reason is recorded" || case_fail "result: no skip reason"
assert_eq "$(result_of "${R}" token)" "" "result: no token"
[ -n "$(result_of "${R}" dump)" ] && case_ok "result: the dump is named" || case_fail "result: no dump"
assert_eq "$(result_of "${R}" dump_verified)" "yes" "result: dump_verified=yes"
hub_is "${SHA_NEW}" && worker_is "${SHA_NEW}" && web_is "${SHA_NEW}" && case_ok "all on the target" || case_fail "not all on the target"
finish_fast
case_end

# ------------------------------------------------------------ --fast: отказы до первой записи
refused_fast() {
    # refused_fast ПОЯСНЕНИЕ ОБРАЗЕЦ_ПРИЧИНЫ — прогон из cut_args в $@ после --
    local why="$1" reason="$2" R="${FAKE_ROOT}/x.result"
    shift 2
    if fcut --execute "${SHA_NEW}" --fast --result "${R}" "$@"; then case_fail "${why}: exit 0"; else case_ok "${why}: exit nonzero"; fi
    assert_eq "$(result_of "${R}" status)/$(result_of "${R}" phase)" "refused/token" "${why}: refused before any write"
    assert_eq "$(result_of "${R}" fail_codes)" "fast_path_refused" "${why}: fail code fast_path_refused"
    assert_contains "${FAKE_ROOT}/cutover.log" "${reason}" "${why}: reason printed (${reason})"
    assert_eq "$(result_of "${R}" vzdump_done)/$(result_of "${R}" worker_stopped)" "no/no" "${why}: nothing was done"
    worker_stopped_ever && case_fail "${why}: worker stopped" || case_ok "${why}: worker untouched"
    grep -qE '^(vzdump|pg_dump) ' "${FAKE_ROOT}/state/calls.log" && case_fail "${why}: vzdump or pg_dump ran" || case_ok "${why}: no vzdump, no pg_dump"
}
case_begin "--fast is refused when deploy tooling changed"
setup_fast fasttool tooling
refused_fast "tooling" "scripts/deploy/lib.sh"
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
EMPTY="${FAKE_ROOT}/empty-repo"
git init -q "${EMPTY}"
FAST_SRC_REAL="${FAST_SRC}"
FAST_SRC="${EMPTY}"
refused_fast "unknown revision" "is not a commit of"
FAST_SRC="${FAST_SRC_REAL}"
finish_fast
case_end
case_begin "--fast is refused when the database is behind the release's migrations"
setup_fast fastdb other
printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/x.result"; then case_fail "exit 0 with a pending migration"; else case_ok "exit nonzero"; fi
assert_contains "${FAKE_ROOT}/cutover.log" "migrations max 40 differs from the release's 41" "the DB disagreement is the reason"
assert_eq "$(result_of "${FAKE_ROOT}/x.result" status)" "refused" "result: refused"
worker_stopped_ever && case_fail "worker stopped" || case_ok "worker untouched"
grep -q '^vzdump ' "${FAKE_ROOT}/state/calls.log" && case_fail "vzdump ran" || case_ok "no vzdump either (it is skipped)"
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

# ------------------------------------------------------------ STOP после flip: тексты без vzdump
stop_after_flip() {
    export FAKE_READYZ_NEW=0
    fcut --execute "${SHA_NEW}" "$@" --result "${FAKE_ROOT}/exe.result" || true
    unset FAKE_READYZ_NEW
    STOP_BLOCK="$(result_of "${FAKE_ROOT}/exe.result" stop_block)"
}
case_begin "STOP-AND-REPORT after a fast run names no vzdump that does not exist"
setup_fast stopfast other
stop_after_flip --fast
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
finish_fast
case_end

case_begin "STOP-AND-REPORT after a full run still names its vzdump"
setup_fast stopfull migration
# Выпуск с миграцией: база и развёрнутый релиз на 0040, цель на 0041.
printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
sed -i 's/^migrations_last=.*/migrations_last=0040_monthly_limits.sql/' \
    "${HUB_RELEASES}/${SHA_PREV}/RELEASE" "${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
fcut "${SHA_NEW}" --rehearse --result "${FAKE_ROOT}/reh.result" || true
TOK="$(result_of "${FAKE_ROOT}/reh.result" token)"
stop_after_flip --confirm "${TOK}"
assert_eq "$(result_of "${FAKE_ROOT}/exe.result" vzdump_done)/$(result_of "${FAKE_ROOT}/exe.result" vzdump_skipped)" "yes/no" "result: vzdump_done=yes, vzdump_skipped=no"
assert_contains "${STOP_BLOCK}" "this run's vzdump = notes pre-${SHA_NEW}-" "live state: the vzdump is named"
assert_contains "${STOP_BLOCK}" "The vzdump above is a separate" "recovery step 2 keeps the vzdump fallback"
finish_fast
case_end

printf '\nfastcutover: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
