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
    # Развёрнутый релиз хранит полный хэш исходника, как настоящая сборка.
    printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${HUB_RELEASES}/${SHA_PREV}/RELEASE"
    printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
    : >"${FAKE_ROOT}/state/calls.log"
}
finish_fast() { drop_fast_source; teardown_case; }
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

# ------------------------------------------------------------ --fast
case_begin "--fast: no token, no vzdump, every gate and preflight still runs, the pg_dump is verified"
setup_fast fast other
cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/db-before.txt"
if fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result"; then case_ok "exit 0"; else case_fail "failed"; tail -10 "${FAKE_ROOT}/cutover.log"; fi
L="${FAKE_ROOT}/cutover.log"
C="${FAKE_ROOT}/state/calls.log"
assert_contains "${L}" "fast path: no confirm token" "the fast path is logged"
assert_contains "${L}" "allowlisted application code" "the recomputed conditions are logged"
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
# Отказ быстрого пути до первой записи: ни маркеров резерва, ни остановок, ни дампов.
no_writes_yet() {
    local why="$1"
    worker_stopped_ever && case_fail "${why}: worker stopped" || case_ok "${why}: worker untouched"
    grep -qE '^(vzdump|pg_dump) ' "${FAKE_ROOT}/state/calls.log" && case_fail "${why}: vzdump or pg_dump ran" || case_ok "${why}: no vzdump, no pg_dump"
    if ls "${HUB_BASE}"/.reserved.* "${WEB_SITES_BASE}"/.reserved.* "${WORKER_BASE}"/.reserved.* >/dev/null 2>&1; then
        case_fail "${why}: reservation markers were written"
    else
        case_ok "${why}: zero reservation writes"
    fi
}
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

case_begin "dump and restore name the same explicit socket as the guard"
setup_fast fastsock other
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
grep -E '^pg_dump ' "${FAKE_ROOT}/state/calls.log" | grep -q -- " -h ${HUB_PG_SOCKET} " && case_ok "pg_dump carries -h <socket>" || case_fail "pg_dump has no explicit -h ${HUB_PG_SOCKET}"
finish_fast
case_end

case_begin "the printed real-run command of a skip-backup rehearsal keeps --skip-ct-backup"
setup_fast fastprint tooling
fcut "${SHA_NEW}" --rehearse --skip-ct-backup --result "${FAKE_ROOT}/reh.result" || true
grep '^  real run:' "${FAKE_ROOT}/cutover.log" | grep -q -- '--skip-ct-backup' && case_ok "the real run line carries --skip-ct-backup" || case_fail "the printed command drops the flag"
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

# ------------------------------------------------------------ базовое состояние закреплено
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
printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
sed -i 's/^migrations_last=.*/migrations_last=0040_monthly_limits.sql/' "${HUB_RELEASES}/${SHA_PREV}/RELEASE" "${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
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

# ------------------------------------------------------------ артефакт и манифест согласованы до записи
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

case_begin "the staged manifest runs as the hub service account under a timeout; a hanging binary refuses"
setup_fast manifestuser other
export FAKE_SSH_LOG_CMDS=1
fcut --execute "${SHA_NEW}" --fast --result "${FAKE_ROOT}/exe.result" || true
unset FAKE_SSH_LOG_CMDS
grep -E 'migrations manifest --json' "${FAKE_ROOT}/state/ssh-cmds.log" | grep -q "runuser -u ${HUB_SERVICE_USER:-canterlot} -- timeout 30 " && case_ok "runuser -u <service account> -- timeout 30 <staged binary> migrations manifest --json" || case_fail "the manifest is not run as the service account under timeout 30"
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
cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/db-before-restore.txt"
printf 'TOC-LINE-02 _sqlx_migrations table\n' >"${HUB_BACKUP_DIR}/canterlot-pre-manual.dump"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${HUB_BACKUP_DIR}/canterlot-pre-manual.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rollback.log" 2>&1; then case_fail "the restore ran on the wrong cluster"; else case_ok "rollback-db.sh refuses the wrong cluster"; fi
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

printf '\nfastcutover: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
