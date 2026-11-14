#!/usr/bin/env bash
# Тесты --dry-run: все проверки против задеплоенного релиза, печать плана,
# доказуемо ни одной записи (ни stop/flip/dump/restore/docker up/копий файлов,
# ни state-файлов, ни /tmp-мусора).
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

cargo() {
    echo "cargo $* [CANTERLOT_OLD_RELEASE_BIN=${CANTERLOT_OLD_RELEASE_BIN:-unset} TMPDIR=${TMPDIR}]" >>"${FAKE_ROOT}/state/calls.log"
    return 0
}
export -f cargo
sleep() { case "${1:-}" in 1|60) command sleep 0.1;; *) command sleep "$@";; esac; }
export -f sleep

EXEC1=""
EXEC2=""
# Цель выката — релиз, которого на хостах ещё нет в current/build; развёрнутый bbbb — база.
TARGET="cccccccccccc"
DEPLOYED="bbbbbbbbbbbb"

write_ownership() {
    mkdir -p "${WORKER_DATA}/ownership"
    python3 - "${WORKER_DATA}/ownership/snapshot.toml" "${EXEC1}" <<'EOF'
import sys
path, live = sys.argv[1], sys.argv[2]
def varint(n):
    out = b""
    while True:
        b = n & 0x7F; n >>= 7
        out += bytes([b | (0x80 if n else 0)])
        if not n: return out
def rec(exec_id, phase):
    b = varint(1 << 3 | 2) + varint(len(exec_id)) + exec_id.encode() + varint(14 << 3 | 0) + varint(phase)
    return "[" + ", ".join(str(x) for x in b) + "]"
dead = "deadbeef-dead-4ead-beef-deadbeefdead"
open(path, "w").write("seq = 0\nrecords = [%s, %s]\n" % (rec(live, 0), rec(dead, 3)))
EOF
}


# dist целевого релиза с настоящим составом build/ релиза 1.3.0 (без service-worker.js,
# с theme-init.js), хэшами и CSP; затем настоящий stage.sh заливает его на фейковые хосты.
make_target_dist() {
    local d="${FAKE_ROOT}/dist/${TARGET}"
    mkdir -p "${d}/hub/bin" "${d}/worker/bin" "${d}/synthetic/bin" "${d}/web/_app" "${d}/web/icons"
    # shellcheck disable=SC2016 # format string is intentional
    printf '#!/bin/sh\ncase "$1" in\n  --version) echo "canterlot-hub 1.17.0" ;;\n  openapi) cat "%s" ;;\n  *) exit 99 ;;\nesac\n' "${REPO_ROOT}/docs/api/openapi.json" >"${d}/hub/bin/canterlot-hub"
    # shellcheck disable=SC2016 # format string is intentional
    printf '#!/bin/sh\ncase "$1" in --version) echo "canterlot-worker 1.17.0";; capabilities) printf "capability claude_histories_v1\\ncapability path_probe_v1\\nharness claude_code\\nharness codex\\n";; *) exit 99;; esac\n' >"${d}/worker/bin/canterlot-worker"
    # shellcheck disable=SC2016 # format string is intentional
    printf '#!/bin/sh\ncase "$1" in --version) echo "canterlot-worker 1.17.0 (synthetic)";; *) exit 99;; esac\n' >"${d}/synthetic/bin/canterlot-worker"
    chmod +x "${d}/hub/bin/canterlot-hub" "${d}/worker/bin/canterlot-worker" "${d}/synthetic/bin/canterlot-worker"
    printf '<html><script>window.x=1</script></html>\n' >"${d}/web/200.html"
    echo x >"${d}/web/manifest.webmanifest"
    echo x >"${d}/web/theme-init.js"
    echo x >"${d}/web/icons/icon.png"
    printf '{"version":"%s"}' "${TARGET}" >"${d}/web/_app/version.json"
    printf 'sha=%s\ndate=x\nmigrations_last=0042_target.sql\nsource=%s%028d\n' "${TARGET}" "${TARGET}" 0 >"${d}/RELEASE"
    printf "default-src 'self'; script-src '%s'; connect-src wss://hub.example.com\n" "${TARGET_SCRIPT_HASH}" >"${d}/csp.txt"
    (cd "${d}" && find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)
}
TARGET_SCRIPT_HASH="sha256-hUmDl3yY/7GaaX+Zuy8aEy8IzxgpBs7xccqYHs2/7kQ="
stage_target() {
    bash "${DEPLOY_DIR}/stage.sh" "${TARGET}" --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage-setup.log" 2>&1 \
        || { echo "setup: stage.sh failed" >&2; tail -5 "${FAKE_ROOT}/stage-setup.log" >&2; }
    rm -f "${FAKE_ROOT}/stage-setup.log"
}

setup_dryrun() {
    setup_case "dryrun-$1"
    export SITE_CODES="meeting.example.com:200 git.example.com:200 caddy.example.com:302 gotify.example.com:200 immich.example.com:200 bookshelf.example.com:200 hub.example.com:200"
    EXEC1="$(new_exec_id)"; EXEC2="$(new_exec_id)"; export EXEC1 EXEC2
    write_configs
    deploy_release hub "bbbbbbbbbbbb"
    deploy_release web "bbbbbbbbbbbb"
    deploy_release worker "bbbbbbbbbbbb"
    printf 'MIGR_MAX=40\nFAILED=0\npayload\n' >"${FAKE_ROOT}/state/db.txt"
    printf '%s\n%s\n' "${EXEC1}" "${EXEC2}" >"${FAKE_ROOT}/state/live-ids"
    printf 'aaaaaaaa-0000-4000-8000-000000000001\n' >"${FAKE_ROOT}/state/live-convs"
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-worker >/dev/null
    FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-hub >/dev/null
    spawn_session "${EXEC1}" bbbbbbbbbbbb 100
    spawn_session "${EXEC2}" bbbbbbbbbbbb 200
    mkdir -p "${WORKER_DATA}/cj"
    python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', 50) + b'\0'*4)" >"${WORKER_DATA}/cj/ack"
    write_ownership
    printf '{"version":"bbbbbbbbbbbb"}' >"${FAKE_ROOT}/container/version.json"
    printf 'CSP-BODY' >"${FAKE_ROOT}/container/csp"
    printf 'CANTERLOT_CSP="CSP-BODY"\n' >"${WEB_ENV}"
    # Запись развёрнутого релиза, как её оставляет cutover (0600, равна живому env).
    cp "${WEB_ENV}" "${WEB_ENV}.${DEPLOYED}"
    chmod 0600 "${WEB_ENV}.${DEPLOYED}"
    # dist развёрнутого релиза: без него сравнения миграций и CSP с dist не выполняются.
    mkdir -p "${FAKE_ROOT}/dist/bbbbbbbbbbbb"
    printf 'sha=bbbbbbbbbbbb\ndate=x\nmigrations_last=0040_monthly_limits.sql\n' >"${FAKE_ROOT}/dist/bbbbbbbbbbbb/RELEASE"
    printf 'CSP-BODY' >"${FAKE_ROOT}/dist/bbbbbbbbbbbb/csp.txt"
    # Цель выката: dist и залитые каталоги на всех трёх хостах (NOSTAGE=1 — не заливать).
    make_target_dist
    [ "${NOSTAGE:-0}" = "1" ] || stage_target
    # Старты юнитов — часть подготовки, не проверяемого dry-run.
    : >"${FAKE_ROOT}/state/calls.log"
    SNAP_BEFORE="$(snapshot_tree)"
    export SNAP_BEFORE
}

# Хэши содержимого, не только имена: dry-run мог перезаписать существующий
# файл тем же набором имён, и сравнение списков это пропускало.
snapshot_tree() {
    find "${FAKE_ROOT}" -type f ! -name '*.log' ! -name '*.result' ! -name '*.result.soft' ! -name '*.result.codes' ! -name '*.result.codes.handled' -print0 2>/dev/null \
        | LC_ALL=C sort -z \
        | xargs -0 -r sha256sum 2>/dev/null || true
}

fs_unchanged() {
    local after
    after="$(snapshot_tree)"
    if [ "${after}" = "${SNAP_BEFORE}" ]; then
        case_ok "$1 (file names and contents unchanged)"
    else
        case_fail "$1 (files or contents changed)"
        diff <(printf '%s\n' "${SNAP_BEFORE}") <(printf '%s\n' "${after}") | head -10
    fi
    [ "$(readlink "${HUB_CURRENT}" | sed 's|.*/||')" = "bbbbbbbbbbbb" ] && case_ok "hub current untouched" || case_fail "hub current moved"
    [ "$(readlink "${WORKER_CURRENT}" | sed 's|.*/||')" = "bbbbbbbbbbbb" ] && case_ok "worker current untouched" || case_fail "worker current moved"
}

# Дозор во время прогона: фиксирует любой новый файл внутри FAKE_ROOT
# с шагом 0.2 c. Нарушение — в файл вне FAKE_ROOT. Свои локальные файлы прогона
# (лог, результат, .soft, .codes и отметка выхода дочернего скрипта .codes.handled)
# не в счёт.
watch_start() {
    WATCH_BASE="/tmp/canterlot-watch-base-${RANDOM}.txt"
    WATCH_LOG="/tmp/canterlot-watch-${RANDOM}.log"
    export WATCH_BASE WATCH_LOG
    find "${FAKE_ROOT}" -type f | grep -vE '/dry\.(log|result|result\.soft|result\.codes|result\.codes\.handled)$' | LC_ALL=C sort >"${WATCH_BASE}"
    ( while true; do
        if ! find "${FAKE_ROOT}" -type f 2>/dev/null | grep -vE '/dry\.(log|result|result\.soft|result\.codes|result\.codes\.handled)$' | LC_ALL=C sort | cmp -s "${WATCH_BASE}" -; then
            echo "NEW FILE DURING DRY-RUN at $(date -u +%T)" >>"${WATCH_LOG}"
            find "${FAKE_ROOT}" -type f 2>/dev/null | grep -vE '/dry\.(log|result|result\.soft|result\.codes|result\.codes\.handled)$' | LC_ALL=C sort | comm -13 "${WATCH_BASE}" - | head -5 >>"${WATCH_LOG}"
            find "${FAKE_ROOT}" -type f 2>/dev/null | grep -vE '/dry\.(log|result|result\.soft|result\.codes|result\.codes\.handled)$' | LC_ALL=C sort >"${WATCH_BASE}"
        fi
        sleep 0.2
      done ) &
    WATCH_PID=$!
}
watch_stop() {
    kill "${WATCH_PID}" 2>/dev/null || true
    wait "${WATCH_PID}" 2>/dev/null || true
    if [ -s "${WATCH_LOG}" ]; then
        case_fail "watcher saw new files during dry-run:"
        head -8 "${WATCH_LOG}"
    else
        case_ok "watcher: no files created during dry-run"
    fi
    rm -f "${WATCH_LOG}" "${WATCH_BASE}"
}

# D1: preflight --dry-run: все проверки, ничего не пишет, spec_compat только печатается.
case_begin "preflight dry-run"
setup_dryrun preflight
watch_start
if bash "${DEPLOY_DIR}/preflight.sh" "${TARGET}" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/dry.log"
fi
watch_stop
[ ! -f "${FAKE_ROOT}/snap.state" ] && case_ok "no state file written" || case_fail "state file written in dry-run"
if grep -q "^cargo test --locked -p canterlot-worker --all-features --test it spec_compat:: \[CANTERLOT_OLD_RELEASE_BIN=${WORKER_RELEASES}/${DEPLOYED}/bin/canterlot-worker TMPDIR=/tmp\]" "${FAKE_ROOT}/state/calls.log"; then
    case_ok "spec_compat ran against the live supervisor release with a short TMPDIR"
else
    case_fail "spec_compat did not run for the live release"; grep '^cargo' "${FAKE_ROOT}/state/calls.log" | head -3
fi
assert_not_contains "${FAKE_ROOT}/dry.log" "would run: CANTERLOT_OLD_RELEASE_BIN" "spec_compat is not only printed"
assert_contains "${FAKE_ROOT}/dry.log" "dry-run mode: checking TARGET ${TARGET}" "preflight announces the target"
fs_unchanged "preflight dry-run wrote nothing"
teardown_case
case_end

# D2: cutover --dry-run: план печатается, ворота проходятся, записей нет.
case_begin "cutover dry-run"
setup_dryrun cutover
watch_start
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/dry.log"
fi
watch_stop
assert_contains "${FAKE_ROOT}/dry.log" "dry-run plan for ${TARGET}" "plan printed for the TARGET release"
assert_contains "${FAKE_ROOT}/dry.log" "target check PASS for ${TARGET}" "target release was checked, not the deployed one"
assert_contains "${FAKE_ROOT}/dry.log" "baseline (deployed): hub=${DEPLOYED}" "deployed release named as the baseline"
assert_contains "${FAKE_ROOT}/dry.log" "dry-run PASS" "dry-run passed"
# Единственный локальный файл dry-run — его файл результата рядом с логом.
grep -qx 'status=pass' "${FAKE_ROOT}/dry.result" && case_ok "result file: status pass" || case_fail "result file: no status=pass"
grep -qx 'mode=dry-run' "${FAKE_ROOT}/dry.result" && case_ok "result file: mode dry-run" || case_fail "result file: wrong mode"
[ ! -e "${FAKE_ROOT}/dry.result.soft" ] && case_ok "soft log removed" || case_fail "soft log left behind"
[ ! -e "${FAKE_ROOT}/dry.result.codes" ] && case_ok "codes log removed" || case_fail "codes log left behind"
assert_contains "${FAKE_ROOT}/dry.log" "maintenance: 2 live executions, each checked for its exec dir and socket" "maintenance checked in the dry-run"
assert_contains "${FAKE_ROOT}/dry.log" "chunk merge preview: from ${DEPLOYED}" "chunk merge previewed"
assert_contains "${FAKE_ROOT}/dry.log" "owner checklist: authenticated checks" "owner checklist printed"
assert_contains "${FAKE_ROOT}/dry.log" "dry-run: geoip: ${HUB_GEOIP_PATH} is 0 days old; no update needed" "geoip state read in the dry-run"
assert_contains "${FAKE_ROOT}/dry.log" "   - geoip: canterlot-hub geoip update" "geoip update listed as a write step"
DRY_TOKEN="$(sed -n 's/^  CONFIRM=\([0-9a-f-]*\).*/\1/p' "${FAKE_ROOT}/dry.log")"
if bash -c 'source "$1/lib.sh"; ph="$(plan_hash "$2" "$3/$2" "$(deployed_sha hub)" "$(deployed_sha web)" "$(deployed_sha worker)")"; confirm_token_check "$4" "$2" "${ph}"' \
        _ "${DEPLOY_DIR}" "${TARGET}" "${FAKE_ROOT}/dist" "${DRY_TOKEN}" >/dev/null; then
    case_ok "the dry-run token is valid for this plan: ${DRY_TOKEN}"
else
    case_fail "dry-run token '${DRY_TOKEN}' not valid for the current plan"
fi
if grep -Eq 'compose (run|up)|^vzdump |^pg_dump |^dropdb |^pg_restore |^createdb |^systemctl .* (start|stop|restart)( |$)' "${FAKE_ROOT}/state/calls.log"; then
    case_fail "mutating calls in dry-run"
else
    case_ok "no mutating calls (stop/dump/restore/compose)"
fi
fs_unchanged "cutover dry-run wrote nothing"
teardown_case
case_end

# D3: verify --dry-run: два чтения, PASS, тики сверены.
case_begin "verify dry-run"
setup_dryrun verify
watch_start
if bash "${DEPLOY_DIR}/verify.sh" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/dry.log"
fi
watch_stop
assert_contains "${FAKE_ROOT}/dry.log" "verify PASS" "verify passed"
assert_contains "${FAKE_ROOT}/dry.log" "own tick snapshot unchanged (read 2)" "ticks compared"
fs_unchanged "verify dry-run wrote nothing"
teardown_case
case_end

# D4: ворота не прошли — dry-run завершается ненулевым кодом, ничего не тронув.
case_begin "dry-run fails on gate failure"
setup_dryrun badgate
export FAKE_CT_LOCK=1
watch_start
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 despite failed gate"
else
    case_ok "exit nonzero"
fi
watch_stop
unset FAKE_CT_LOCK
assert_contains "${FAKE_ROOT}/dry.log" "CT locked" "gate failure reported"
assert_not_contains "${FAKE_ROOT}/dry.log" "CONFIRM=" "a failed dry-run prints no token"
fs_unchanged "failed dry-run wrote nothing"
teardown_case
case_end

# D5: провал запроса списка исполнений в dry-run — soft-ошибка и ненулевой
# выход, а не пустой цикл и «PASS».
case_begin "cutover dry-run live query fails"
setup_dryrun livequeryfail
export FAKE_PSQL_FAIL_MATCH="select id::text from executions"
watch_start
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 despite failed live-execution query"
else
    case_ok "exit nonzero"
fi
watch_stop
unset FAKE_PSQL_FAIL_MATCH
assert_contains "${FAKE_ROOT}/dry.log" "cannot list live executions for the dry-run snapshot" "query failure reported"
fs_unchanged "failed dry-run wrote nothing"
teardown_case
case_end

# D6: неизвестный ключ worker.toml — dry-run обязан выполнить preflight и упасть.
case_begin "cutover dry-run runs preflight config gate"
setup_dryrun preflightconfig
printf 'bogus_key = 1\n' >>"${WORKER_CONFIG}"
SNAP_BEFORE="$(snapshot_tree)"
watch_start
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 despite unknown worker config key"
else
    case_ok "exit nonzero"
fi
watch_stop
assert_contains "${FAKE_ROOT}/dry.log" "unknown key" "preflight config gate ran in dry-run"
fs_unchanged "failed dry-run wrote nothing"
teardown_case
case_end

# D7: битый ownership — preflight в dry-run обязан это поймать.
case_begin "cutover dry-run runs ownership gate"
setup_dryrun preflightownership
printf 'seq = 0\nrecords = [[999]]\n' >"${WORKER_DATA}/ownership/snapshot.toml"
SNAP_BEFORE="$(snapshot_tree)"
watch_start
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 despite corrupt ownership"
else
    case_ok "exit nonzero"
fi
watch_stop
assert_contains "${FAKE_ROOT}/dry.log" "ownership journal decode failed" "ownership gate ran in dry-run"
fs_unchanged "failed dry-run wrote nothing"
teardown_case
case_end

# D8: verify-ворота (код сайта) тоже выполняются в dry-run.
case_begin "cutover dry-run runs verify gates"
setup_dryrun verifygate
export FAKE_SITE_FAIL=meeting.example.com
watch_start
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 despite failing verify site gate"
else
    case_ok "exit nonzero"
fi
watch_stop
unset FAKE_SITE_FAIL
assert_contains "${FAKE_ROOT}/dry.log" "site meeting.example.com: 500, expected 200" "verify site gate ran in dry-run"
assert_contains "${FAKE_ROOT}/dry.log" "verification read 1" "verify itself ran in dry-run"
fs_unchanged "failed dry-run wrote nothing"
teardown_case
case_end

# D9: план явно перечисляет NOT RUN write-шаги и говорит, что все чтения прошли.
case_begin "cutover dry-run lists NOT RUN write steps"
setup_dryrun notrunlist
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/dry.log"
fi
assert_contains "${FAKE_ROOT}/dry.log" "NOT RUN (write steps" "NOT RUN section printed"
assert_contains "${FAKE_ROOT}/dry.log" "worker stop" "worker stop listed as NOT RUN"
assert_contains "${FAKE_ROOT}/dry.log" "every read-only preflight/verify gate ran" "all read-only gates reported as run"
teardown_case
case_end

# D10: без локального dist база на 999 против RELEASE 40 не проходит dry-run
# (миграции берутся из RELEASE развёрнутого релиза), а сравнение CSP без dist базы —
# SKIPPED с причиной (запись релиза контейнера, если есть, сверяется), не провал.
case_begin "cutover dry-run without dist compares migrations with the deployed RELEASE"
setup_dryrun nodist
rm -rf "${FAKE_ROOT}/dist/${DEPLOYED}"
printf 'MIGR_MAX=999\nFAILED=0\npayload\n' >"${FAKE_ROOT}/state/db.txt"
SNAP_BEFORE="$(snapshot_tree)"
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" --allow-skipped >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 with DB at 999 and RELEASE at 40"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/dry.log" "migrations max 999, expected 40" "migration mismatch reported without the deployed dist"
printf 'MIGR_MAX=40\nFAILED=0\npayload\n' >"${FAKE_ROOT}/state/db.txt"
# Без dist базы и без записи релиза сравнение CSP — SKIPPED с причиной, а не NOT RUN;
# но отсутствие самой записи (источника отката PWA) — мягкий провал обслуживания.
rm -f "${WEB_ENV}.${DEPLOYED}"
SNAP_BEFORE="$(snapshot_tree)"
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 without the release env record"
else
    case_ok "no release env record: maintenance fails the dry-run"
fi
assert_contains "${FAKE_ROOT}/dry.log" "SKIPPED: container CSP vs dist" "SKIPPED reported with the reason"
assert_not_contains "${FAKE_ROOT}/dry.log" "NOT RUN: container CSP vs dist" "not counted as NOT RUN"
assert_contains "${FAKE_ROOT}/dry.log" "maintenance: env record canterlot.env.${DEPLOYED}: missing" "missing env record reported"
# С записью развёрнутого релиза контейнер сверяется с ней.
printf 'CANTERLOT_CSP="CSP-BODY"\n' >"${WEB_ENV}.${DEPLOYED}"
chmod 0600 "${WEB_ENV}.${DEPLOYED}"
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_ok "record matches the container: exit 0"
else
    case_fail "failed with a matching release record"; tail -5 "${FAKE_ROOT}/dry.log"
fi
assert_contains "${FAKE_ROOT}/dry.log" "matches the deployed release's own record" "compared with the release record"
printf 'CANTERLOT_CSP="OTHER"\n' >"${WEB_ENV}.${DEPLOYED}"
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 with a differing release record"
else
    case_ok "differing release record: exit nonzero"
fi
assert_contains "${FAKE_ROOT}/dry.log" "differs from the deployed release's own record" "difference reported"
assert_not_contains "${FAKE_ROOT}/dry.log" "OTHER" "record value not printed"
teardown_case
case_end

# D10b: csp.txt цели не выведен из финального билда (старый хэш скрипта) — провал до любого flip.
case_begin "dry-run refuses a target csp.txt that does not match the final build"
setup_dryrun cspstale
printf "default-src 'self'; script-src 'sha256-STALEHASH'; connect-src wss://hub.example.com\n" >"${FAKE_ROOT}/dist/${TARGET}/csp.txt"
(cd "${FAKE_ROOT}/dist/${TARGET}" && find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 with a stale CSP hash"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/dry.log" "target CSP is not derived from the dist build" "stale CSP reported"
teardown_case
case_end


# D11: dry-run без SHA цели не подставляет развёрнутый релиз молча.
case_begin "dry-run never substitutes the deployed release for a missing target"
setup_dryrun nosha
for tool in cutover preflight; do
    if bash "${DEPLOY_DIR}/${tool}.sh" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
        case_fail "${tool} --dry-run without a SHA exited 0"
    else
        case_ok "${tool} --dry-run without a SHA exits nonzero"
    fi
    assert_contains "${FAKE_ROOT}/dry.log" "dry-run needs the TARGET sha" "${tool}: usage names the target"
    assert_not_contains "${FAKE_ROOT}/dry.log" "checks against deployed release" "${tool}: no silent substitution"
done
teardown_case
case_end

# D12: цель ещё не залита — NOT RUN на каждом хосте, провал без --allow-skipped.
case_begin "dry-run reports a target that is not staged as NOT RUN"
NOSTAGE=1 setup_dryrun notstaged
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 although the target is not staged"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/dry.log" "NOT RUN: hub ${TARGET} is not staged" "hub NOT RUN"
assert_contains "${FAKE_ROOT}/dry.log" "NOT RUN: web ${TARGET} is not staged" "web NOT RUN"
assert_contains "${FAKE_ROOT}/dry.log" "NOT RUN: worker ${TARGET} is not staged" "worker NOT RUN"
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" --allow-skipped >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_ok "--allow-skipped acknowledges the gap: exit 0"
else
    case_fail "still failing with --allow-skipped"; grep -E 'HARD|FAIL' "${FAKE_ROOT}/dry.log" | head -3
fi
teardown_case
case_end

# D13: залитый релиз не совпадает с dist (испорченный файл, лишний top-level, чужая
# версия) и база впереди цели — жёстко, с названием причины.
case_begin "dry-run catches a damaged staged target and a database ahead of it"
setup_dryrun damaged
printf 'tampered\n' >>"${WEB_RELEASES}/${TARGET}/200.html"
echo junk >"${WEB_RELEASES}/${TARGET}/EXTRA"
printf 'tampered-bin\n' >>"${HUB_RELEASES}/${TARGET}/bin/canterlot-hub"
printf 'MIGR_MAX=50\nFAILED=0\npayload\n' >"${FAKE_ROOT}/state/db.txt"
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 with a damaged staged target"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/dry.log" "web ${TARGET} staged top entries differ from the dist" "extra web top-level entry caught"
assert_contains "${FAKE_ROOT}/dry.log" "web ${TARGET}: staged tree differs from the dist" "web file hash caught"
assert_contains "${FAKE_ROOT}/dry.log" "hub ${TARGET}: staged tree differs from the dist" "hub binary hash caught"
assert_contains "${FAKE_ROOT}/dry.log" "database is ahead of the target release" "DB ahead of the target caught"
teardown_case
case_end

# D13b: после ручного слияния старых immutable-чанков залитый web шире dist, и это норма;
# лишний файл вне _app/immutable по-прежнему провал.
case_begin "target-check accepts merged older immutable chunks and refuses other extras"
setup_dryrun merged
mkdir -p "${WEB_RELEASES}/${TARGET}/_app/immutable/assets"
echo old >"${WEB_RELEASES}/${TARGET}/_app/immutable/assets/old.css"
if bash "${DEPLOY_DIR}/target-check.sh" "${TARGET}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/tc.log" 2>&1; then
    case_ok "merged immutable chunks: exit 0"
else
    case_fail "merged immutable chunks refused"; grep -E 'HARD' "${FAKE_ROOT}/tc.log" | head -3
fi
assert_contains "${FAKE_ROOT}/tc.log" "older immutable chunk(s) merged in" "merge noted"
echo stray >"${WEB_RELEASES}/${TARGET}/_app/stray.js"
if bash "${DEPLOY_DIR}/target-check.sh" "${TARGET}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/tc.log" 2>&1; then
    case_fail "extra file outside _app/immutable accepted"
else
    case_ok "extra file outside _app/immutable: exit nonzero"
fi
assert_contains "${FAKE_ROOT}/tc.log" "1 extra file(s) outside _app/immutable" "stray file counted"
rm -f "${WEB_RELEASES}/${TARGET}/_app/immutable/assets/old.css"
printf 'x\n' >>"${WEB_RELEASES}/${TARGET}/theme-init.js"
rm -f "${WEB_RELEASES}/${TARGET}/_app/stray.js"
if bash "${DEPLOY_DIR}/target-check.sh" "${TARGET}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/tc.log" 2>&1; then
    case_fail "changed dist file accepted"
else
    case_ok "changed dist file: exit nonzero"
fi
teardown_case
case_end

# D14: dist цели повреждён (SHA256SUMS не сходится, чужая версия в RELEASE).
case_begin "dry-run catches a damaged target dist"
setup_dryrun baddist
printf 'tampered\n' >>"${FAKE_ROOT}/dist/${TARGET}/web/200.html"
sed -i "s/^sha=.*/sha=dddddddddddd/" "${FAKE_ROOT}/dist/${TARGET}/RELEASE"
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_fail "exit 0 with a damaged target dist"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/dry.log" "dist RELEASE sha dddddddddddd is not the target ${TARGET}" "RELEASE sha mismatch caught"
assert_contains "${FAKE_ROOT}/dry.log" "target dist SHA256SUMS does not verify" "dist hashes caught"
teardown_case
case_end

# D15: версия PWA читается внутри контейнера, не по пути хоста: при чужом хостовом
# build/_app/version.json и верном контейнере проверка проходит, при чужом контейнере — нет.
case_begin "dry-run reads the served PWA version inside the container"
setup_dryrun containerver
printf '{"version":"zzzzzzzzzzzz"}' >"${WEB_RELEASES}/${DEPLOYED}/_app/version.json"
if bash "${DEPLOY_DIR}/cutover.sh" "${TARGET}" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/dry.log" 2>&1; then
    case_ok "host-path version.json is not consulted for the served version"
else
    case_fail "host path was read"; grep -E 'HARD|SOFT' "${FAKE_ROOT}/dry.log" | head -3
fi
assert_contains "${FAKE_ROOT}/state/calls.log" "docker fakeweb exec caddy cat /srv/canterlot/web/build/_app/version.json" "version.json read via docker exec in the container"
teardown_case
case_end

# D16: EXPECT цели сверяется с `capabilities` боевого воркера dist (решение владельца 4).
for variant in match differ; do
    case_begin "target-check compares the dist EXPECT with the dist worker (${variant})"
    setup_dryrun "expect-${variant}"
    d="${FAKE_ROOT}/dist/${TARGET}"
    if [ "${variant}" = "match" ]; then
        printf 'format=1\ncapabilities=claude_histories_v1 path_probe_v1\nharnesses=claude_code codex\n' >"${d}/EXPECT"
    else
        printf 'format=1\ncapabilities=claude_histories_v1\nharnesses=claude_code codex\n' >"${d}/EXPECT"
    fi
    (cd "${d}" && find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)
    bash "${DEPLOY_DIR}/target-check.sh" "${TARGET}" --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/tc.log" 2>&1 || true
    if [ "${variant}" = "match" ]; then
        assert_contains "${FAKE_ROOT}/tc.log" "target dist EXPECT equals the worker's own capabilities: 2 capabilities, harnesses claude_code codex" "EXPECT matches the binary"
    else
        assert_contains "${FAKE_ROOT}/tc.log" "target dist EXPECT differs from 'canterlot-worker capabilities'" "EXPECT mismatch caught"
    fi
    teardown_case
    case_end
done

printf 'dryrun harness: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
