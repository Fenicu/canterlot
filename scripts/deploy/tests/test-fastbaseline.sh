#!/usr/bin/env bash
# Быстрый путь: закреплённая база (дрейф, flip из ожидаемого релиза), манифест против RELEASE,
# запуск от службы под пределом времени, чекаут в обоих режимах пропуска. Стенд — как в test-fastcutover.sh.
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
    # fcut РЕЖИМ... : cutover.sh с источником-репозиторием; ssh-команды пишутся в state/ssh-cmds.log
    FAKE_SSH_LOG_CMDS=1 bash "${DEPLOY_DIR}/cutover.sh" "$@" --source "${FAST_SRC}" --dist "${FAKE_ROOT}/dist" \
        --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1
}
rehearse_skip() { fcut "${SHA_NEW}" --rehearse --skip-ct-backup "$@"; }
# Токен репетиции из её файла результата.
token_of() { result_of "${FAKE_ROOT}/cutover.result" token; }
L="${FAKE_ROOT:-}"


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
grep -E 'migrations manifest --json' "${FAKE_ROOT}/state/ssh-cmds.log" | grep -q "runuser -u '${HUB_SERVICE_USER:-canterlot}' -- timeout 30 " && case_ok "runuser -u <service account> -- timeout 30 <staged binary> migrations manifest --json" || case_fail "the manifest is not run as the service account under timeout 30"
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

printf '\nfastbaseline: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
