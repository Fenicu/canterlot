#!/usr/bin/env bash
# Репетиция cutover.sh --rehearse: весь путь записи против стенда, каждая запись —
# только WOULD-WRITE; чтения, ожидания и циклы настоящие; токен репетиции годен
# для боевого прогона того же плана, просроченный или чужой — нет.
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

run_rehearse() {
    bash "${DEPLOY_DIR}/cutover.sh" "${1:-${SHA_NEW}}" --rehearse --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" \
        --state "${FAKE_ROOT}/rehearse.state" "${@:2}" >"${FAKE_ROOT}/rehearse.log" 2>&1
}
# Мутирующие вызовы фейков в calls.log (чтения — show/is-active/inspect/exec cat — не в счёт).
mutations() {
    grep -E '^systemctl [a-z]+ (stop|start|restart|daemon-reload|mask)|^vzdump |^pg_dump |compose up|^dropdb |^createdb ' \
        "${FAKE_ROOT}/state/calls.log" || true
}
token_from() { sed -n 's/^  CONFIRM=\([0-9a-f-]*\).*/\1/p' "$1" | tail -1; }

case_begin "rehearsal runs the whole write path and writes nothing"
setup_cutover rehearse
# Прошлый дамп (репетиция проверяет чтением его) и старый чанк, которого нет в цели.
FAKE_ROOT="${FAKE_ROOT}" pg_dump -Fc -f "${HUB_BACKUP_DIR}/canterlot-pre-${SHA_PREV}-20261101T000000Z.dump" canterlot
printf 'old\n' >"${WEB_RELEASES}/${SHA_PREV}/_app/immutable/chunks/probe-old.js"
WORKER_PID_BEFORE="$(cat "${FAKE_ROOT}/state/unit-local-canterlot-worker.pid")"
HUB_PID_BEFORE="$(cat "${FAKE_ROOT}/state/unit-fakehub-canterlot-hub.pid")"
: >"${FAKE_ROOT}/state/calls.log"
if run_rehearse; then case_ok "exit 0"; else case_fail "rehearsal failed"; tail -15 "${FAKE_ROOT}/rehearse.log"; fi
assert_eq "$(mutations)" "" "no mutating command reached any fake host"
hub_is "${SHA_PREV}" && case_ok "hub current untouched" || case_fail "hub moved"
web_is "${SHA_PREV}" && case_ok "web build untouched" || case_fail "web moved"
worker_is "${SHA_PREV}" && case_ok "worker current untouched" || case_fail "worker moved"
assert_eq "$(cat "${FAKE_ROOT}/state/unit-local-canterlot-worker.pid")" "${WORKER_PID_BEFORE}" "worker never restarted"
assert_eq "$(cat "${FAKE_ROOT}/state/unit-fakehub-canterlot-hub.pid")" "${HUB_PID_BEFORE}" "hub never restarted"
assert_proc_alive "${WORKER_PID_BEFORE}" "worker control process still alive"
assert_proc_alive "${SUP1}" "session 1 supervisor alive"
[ ! -e "${WEB_ENV}.${SHA_NEW}" ] && case_ok "no canterlot.env.<target> written" || case_fail "env file written"
assert_eq "$(find "${HUB_BACKUP_DIR}" -name '*.dump' | wc -l)" "1" "no new dump written (only the old one)"
ls "${HUB_BASE}"/.reserved.* "${WORKER_BASE}"/.reserved.* >/dev/null 2>&1 && case_fail "reservation files written" || case_ok "no reservation files"
[ ! -e "${WEB_RELEASES}/${SHA_NEW}/_app/immutable/chunks/probe-old.js" ] && case_ok "old chunks not merged" || case_fail "chunks merged"
for label in "stop worker" "vzdump" "pg_dump" "hub flip" "hub restart" "write canterlot.env.${SHA_NEW}" "web build flip" "caddy recreate" "worker flip" "worker start" "merge old immutable chunks" "loopback caddy probe" "reserve hub releases" "release worker reservations"; do
    assert_contains "${FAKE_ROOT}/rehearse.log" "(${label}):" "WOULD-WRITE printed for: ${label}"
done
in_order "${FAKE_ROOT}/rehearse.log" "target check PASS for ${SHA_NEW}" "(vzdump):" \
    "re-checking read gates and the session inventory after vzdump" "(stop worker):" "(pg_dump):" \
    "dump fully readable" "(hub flip):" "(hub restart):" "(worker flip):" "(worker start):" \
    'worker journal parse found state="live"' "(write canterlot.env.${SHA_NEW}):" "(web build flip):" \
    "(caddy recreate):" "running verification" \
    && case_ok "rehearsal prints the new order: vzdump, gates again, stop, dump, hub, worker live, PWA" \
    || case_fail "rehearsal order differs"
in_order "${FAKE_ROOT}/rehearse.log" "| steps:" "|   2 fresh vzdump of CT" "|   3 read gates and the session inventory again" \
    "|   4 stop worker ${WORKER_UNIT}" "|   5 quiesced pg_dump" "|   6 hub flip" "|   7 worker flip" "|   8 PWA" \
    && case_ok "plan text lists the new order" || case_fail "plan text order differs"
assert_contains "${FAKE_ROOT}/rehearse.log" "rehearse: no down time" "rehearsal measures no down time"
N="$(grep -c 'WOULD-WRITE #' "${FAKE_ROOT}/rehearse.log" || true)"
[ "${N}" -ge 18 ] && case_ok "${N} writes printed" || case_fail "only ${N} writes printed"
assert_contains "${FAKE_ROOT}/rehearse.log" "the control-worker scan finds exactly the live unit process" "control-worker scan exercised against the live worker"
assert_contains "${FAKE_ROOT}/rehearse.log" "start-wait scenario pid-empty:3: 3 faulty polls absorbed" "empty MainPID absorbed"
assert_contains "${FAKE_ROOT}/rehearse.log" "start-wait scenario pid-zero:2: 2 faulty polls absorbed" "MainPID=0 absorbed"
assert_contains "${FAKE_ROOT}/rehearse.log" "start-wait scenario exe-enoent:3: 3 faulty polls absorbed" "readlink ENOENT absorbed"
assert_contains "${FAKE_ROOT}/rehearse.log" "start-wait scenario pid-empty:2,exe-enoent:2: 4 faulty polls absorbed" "both faults together absorbed"
assert_contains "${FAKE_ROOT}/rehearse.log" "never-appears: loop ended with a clean rc 1" "never-appearing PID ends cleanly"
assert_contains "${FAKE_ROOT}/rehearse.log" "real exec transitions" "real exec transitions exercised"
assert_contains "${FAKE_ROOT}/rehearse.log" "worker journal parse found state=\"live\"" "live wait parsed the real journal"
assert_contains "${FAKE_ROOT}/rehearse.log" "dump fully readable: canterlot-pre-${SHA_PREV}-20261101T000000Z.dump" "dump verification exercised on the newest existing dump"
REHEARSE_OLD_N="$(find "${WEB_RELEASES}/${SHA_PREV}/_app/immutable" -type f | wc -l | tr -d ' ')"
assert_contains "${FAKE_ROOT}/rehearse.log" "rehearse: chunk merge from ${SHA_PREV} would copy 1 of ${REHEARSE_OLD_N} files" "chunk merge counted"
assert_contains "${FAKE_ROOT}/rehearse.log" "maintenance: no current.new/build.new/.stage-* on any host" "maintenance ran"
assert_contains "${FAKE_ROOT}/rehearse.log" "verify PASS" "verify ran against the live state"
assert_contains "${FAKE_ROOT}/rehearse.log" "rehearse: loopback Caddy probe printed above, not started" "loopback probe only printed"
assert_contains "${FAKE_ROOT}/rehearse.log" "live: hub exe from" "retention plan ran"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "] removed " "retention removed nothing"
assert_contains "${FAKE_ROOT}/rehearse.log" "owner checklist: authenticated checks" "owner checklist printed"
assert_contains "${FAKE_ROOT}/rehearse.log" "dbip-city-lite.mmdb is 0 days old, no update" "geoip step read the live database age"
assert_contains "${FAKE_ROOT}/rehearse.log" "rehearsal PASS" "rehearsal PASS"
[ -n "$(token_from "${FAKE_ROOT}/rehearse.log")" ] && case_ok "confirm token printed" || case_fail "no token"
R="${FAKE_ROOT}/rehearse.result"
grep -q 'docker fakeweb exec -i' "${FAKE_ROOT}/state/calls.log" && case_fail "loopback probe started" || case_ok "loopback probe never started"
teardown_case
case_end

# --- V3: репетиция — надмножество dry-run: target-check в общей предпроверке ---
case_begin "rehearsal runs the target check (staged trees, dist, DB not ahead)"
setup_cutover rehearse-target
if run_rehearse; then case_ok "exit 0"; else case_fail "rehearsal failed"; tail -10 "${FAKE_ROOT}/rehearse.log"; fi
assert_contains "${FAKE_ROOT}/rehearse.log" "target check PASS for ${SHA_NEW}" "target check ran inside the rehearsal"
assert_contains "${FAKE_ROOT}/rehearse.log" "web ${SHA_NEW}: staged tree equals the dist (every file hash)" "staged web tree compared"
assert_contains "${FAKE_ROOT}/rehearse.log" "worker ${SHA_NEW}: staged tree equals the dist (every file hash)" "staged worker tree compared"
assert_contains "${FAKE_ROOT}/rehearse.log" "migrations: database at 40, target ends at 41" "DB-not-ahead compared"
teardown_case
case_end

case_begin "a staged-tree mismatch fails the rehearsal like the dry-run"
setup_cutover rehearse-staged-mismatch
printf 'tampered\n' >>"${WEB_RELEASES}/${SHA_NEW}/_app/immutable/chunks/probe.js"
: >"${FAKE_ROOT}/state/calls.log"
if run_rehearse; then case_fail "rehearsal passed with a tampered staged tree"; else case_ok "rehearsal failed"; fi
assert_contains "${FAKE_ROOT}/rehearse.log" "staged tree differs from the dist" "mismatch named"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "CONFIRM=" "no token printed"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "WOULD-WRITE #" "refused before the write path"
assert_eq "$(mutations)" "" "no mutating command"
teardown_case
case_end

case_begin "a database ahead of the target fails the rehearsal like the dry-run"
setup_cutover rehearse-db-ahead
sed -i 's/^MIGR_MAX=.*/MIGR_MAX=42/' "${FAKE_ROOT}/state/db.txt"
: >"${FAKE_ROOT}/state/calls.log"
if run_rehearse; then case_fail "rehearsal passed with the DB ahead"; else case_ok "rehearsal failed"; fi
assert_contains "${FAKE_ROOT}/rehearse.log" "database is ahead of the target release: migrations max 42 > target migrations_last 41" "DB ahead named"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "CONFIRM=" "no token printed"
assert_eq "$(mutations)" "" "no mutating command"
teardown_case
case_end

case_begin "the real run refuses a staged-tree mismatch before touching anything"
setup_cutover execute-staged-mismatch
TOKEN="$(confirm_for "${SHA_NEW}")"
printf 'tampered\n' >>"${WORKER_RELEASES}/${SHA_NEW}/RELEASE"
: >"${FAKE_ROOT}/state/calls.log"
if bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "${TOKEN}" --source "${REPO_ROOT}" \
        --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_fail "real run accepted a tampered staged tree"
else
    case_ok "real run refused"
fi
assert_contains "${FAKE_ROOT}/cutover.log" "worker ${SHA_NEW}: staged tree differs from the dist" "mismatch named"
assert_eq "$(mutations)" "" "nothing stopped, dumped or flipped"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
teardown_case
case_end

case_begin "the rehearsal token unlocks the real run of the same plan"
setup_cutover rehearse-execute
if run_rehearse; then case_ok "rehearsal exit 0"; else case_fail "rehearsal failed"; tail -10 "${FAKE_ROOT}/rehearse.log"; fi
TOKEN="$(token_from "${FAKE_ROOT}/rehearse.log")"
if bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "${TOKEN}" --source "${REPO_ROOT}" \
        --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_ok "real run with the rehearsal token: exit 0"
else
    case_fail "real run failed"; tail -10 "${FAKE_ROOT}/cutover.log"
fi
assert_contains "${FAKE_ROOT}/cutover.log" "confirm token accepted" "token accepted"
hub_is "${SHA_NEW}" && case_ok "hub on new" || case_fail "hub not new"
worker_is "${SHA_NEW}" && case_ok "worker on new" || case_fail "worker not new"
teardown_case
case_end

case_begin "a token refuses the real run after the plan changed"
setup_cutover rehearse-plan-changed
run_rehearse || true
TOKEN="$(token_from "${FAKE_ROOT}/rehearse.log")"
printf '\n' >>"${DIST_DIR}/SHA256SUMS"
if bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "${TOKEN}" --source "${REPO_ROOT}" \
        --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_fail "real run accepted a token of another plan"
else
    case_ok "real run refused"
fi
assert_contains "${FAKE_ROOT}/cutover.log" "the plan changed since the token was issued" "plan change reported"
assert_eq "$(grep -cE 'stop canterlot-worker|^vzdump ' "${FAKE_ROOT}/state/calls.log" || true)" "0" "nothing stopped or backed up"
hub_is "${SHA_PREV}" && case_ok "hub untouched" || case_fail "hub moved"
teardown_case
case_end

case_begin "an expired token refuses the real run"
setup_cutover expired
OLD="$(issue_token "${SHA_NEW}" "${FAKE_ROOT}/dist" "$(( $(date -u +%s) - 7300 ))")"
if bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "${OLD}" --source "${REPO_ROOT}" \
        --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1; then
    case_fail "expired token accepted"
else
    case_ok "expired token refused"
fi
assert_contains "${FAKE_ROOT}/cutover.log" "token expired" "expiry reported"
worker_is "${SHA_PREV}" && case_ok "worker untouched" || case_fail "worker moved"
teardown_case
case_end

case_begin "a failing rehearsal prints no token and writes nothing"
setup_cutover rehearse-fail
export FAKE_SITE_FAIL=meeting.example.com
: >"${FAKE_ROOT}/state/calls.log"
if run_rehearse; then case_fail "rehearsal passed with a broken site"; else case_ok "rehearsal failed"; fi
unset FAKE_SITE_FAIL
assert_contains "${FAKE_ROOT}/rehearse.log" "REHEARSAL FAIL" "failure reported"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "CONFIRM=" "no token printed"
assert_not_contains "${FAKE_ROOT}/rehearse.log" "STOP-AND-REPORT" "no recovery report in a rehearsal"
assert_eq "$(mutations)" "" "no mutating command after the failure either"
worker_is "${SHA_PREV}" && case_ok "worker untouched" || case_fail "worker moved"
teardown_case
case_end

case_begin "shadow rehearsal of the deployed release (what production runs at the end)"
setup_cutover shadow
# dist развёрнутого релиза (как в проде: dist/255ccb1bbe37 лежит в сборочном worktree).
cp "${DIST_DIR}/web/200.html" "${WEB_RELEASES}/${SHA_PREV}/"
make_dist_from_staged "${SHA_PREV}"
SHADOW_CSP="$(cat "${FAKE_ROOT}/dist/${SHA_PREV}/csp.txt")"
printf 'CANTERLOT_CSP="%s"\n' "${SHADOW_CSP}" | tee "${WEB_ENV}" >"${WEB_ENV}.${SHA_PREV}"
chmod 0600 "${WEB_ENV}.${SHA_PREV}"
printf '%s' "${SHADOW_CSP}" >"${FAKE_ROOT}/container/csp"
: >"${FAKE_ROOT}/state/calls.log"
if run_rehearse "${SHA_PREV}" --shadow; then case_ok "shadow rehearsal exit 0"; else case_fail "shadow rehearsal failed"; tail -15 "${FAKE_ROOT}/rehearse.log"; fi
assert_contains "${FAKE_ROOT}/rehearse.log" "shadow mode" "shadow noted"
assert_contains "${FAKE_ROOT}/rehearse.log" "rehearsal PASS" "rehearsal PASS"
assert_eq "$(mutations)" "" "no mutating command"
teardown_case
case_end

case_begin "--rehearse and --dry-run are exclusive; --confirm is for --execute only"
setup_cutover modes
bash "${DEPLOY_DIR}/cutover.sh" "${SHA_NEW}" --rehearse --dry-run >"${FAKE_ROOT}/m.log" 2>&1 && case_fail "both modes accepted" || case_ok "both modes refused"
assert_contains "${FAKE_ROOT}/m.log" "pick one" "reason printed"
bash "${DEPLOY_DIR}/cutover.sh" "${SHA_NEW}" --rehearse --confirm x >"${FAKE_ROOT}/m.log" 2>&1 && case_fail "--confirm accepted in a rehearsal" || case_ok "--confirm refused in a rehearsal"
teardown_case
case_end

printf 'rehearse harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
