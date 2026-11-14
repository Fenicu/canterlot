#!/usr/bin/env bash
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
# Прогон всей обвязки scripts/deploy/tests: shellcheck, синтаксис python,
# статическая проверка pipe-lint, затем тестовые наборы ПАРАЛЛЕЛЬНО (каждый в
# своём FAKE_ROOT, процессы и /proc-сканы изолированы по корню и exec id).
# Ничего не трогает прод: harness ходит только в temp-каталоги через fake-ssh
# (настоящие хосты — отказ).
# RUN_JOBS=N — сколько наборов одновременно (по умолчанию все); RUN_SUITES="a b" —
# только перечисленные наборы. Лог каждого набора: $RUN_LOG_DIR/<набор>.log.
set -euo pipefail

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
cd "${DEPLOY_DIR}/../.."

log() { printf '[harness-run %s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
STARTED="$(date +%s)"

log "shellcheck"
shellcheck scripts/check-proxy.sh scripts/release.sh scripts/deploy/*.sh scripts/deploy/tests/*.sh scripts/deploy/tests/fake-bin/*
log "shellcheck clean"

log "python syntax"
for py in scripts/deploy/config-check.py scripts/deploy/ownership-check.py scripts/deploy/spec-shape.py scripts/deploy/tests/pipe-lint.py; do
    python3 -I -c "import ast, sys; ast.parse(open(sys.argv[1]).read())" "${py}"
done
log "python OK"

log "pipe-lint (SIGPIPE, wait loops, && break, source, self-match, run_write)"
python3 -I scripts/deploy/tests/pipe-lint.py
log "pipe-lint clean"

log "lib unit checks (in a child bash: lib.sh sets -e and must not leak into this runner)"
bash -c '
set -euo pipefail
source scripts/deploy/lib.sh
fail() { echo "[harness-run] $*"; exit 1; }
sha_match 8be10fce886b 8be10fce || fail "sha_match full-vs-short failed"
sha_match 8be10fce 8be10fce886b || fail "sha_match short-vs-full failed"
sha_match d3efc1e d3efc1e || fail "sha_match equal failed"
if sha_match d3efc1e ba35992; then fail "sha_match different passed (must fail)"; fi
if sha_match "" d3efc1e; then fail "sha_match empty passed (must fail)"; fi
if sha_match abc abcdef1; then fail "sha_match short passed (must fail)"; fi
if sha_match UNREADABLE d3efc1e; then fail "sha_match unreadable passed (must fail)"; fi
for ok in 8be10fce886b 8be10fce886b8be10fce886b8be10fce886b8be1; do
    is_release_id "${ok}" || fail "release id rejected: ${ok}"
done
for bad in "" 8be10fce 8be10fce886g "'"'"';>PWN;#xxxx" rel000000001; do
    if is_release_id "${bad}"; then fail "bad release id accepted: ${bad}"; fi
done
in_lines b "$(printf "a\nb\nc")" || fail "in_lines missed b"
if in_lines bb "$(printf "a\nb")"; then fail "in_lines matched a substring"; fi
'
log "lib unit checks OK"

ALL_SUITES="lint token guardrails invariants site-env preflight stage retain verify dryrun rehearse rollback-db cutover cutover-signals geoip speech check-proxy release"
SUITES="${RUN_SUITES:-${ALL_SUITES}}"
RUN_LOG_DIR="${RUN_LOG_DIR:-$(mktemp -d /tmp/canterlot-deploy-run.XXXXXX)}"
mkdir -p "${RUN_LOG_DIR}"
JOBS="${RUN_JOBS:-0}"
log "suites: ${SUITES} (logs in ${RUN_LOG_DIR}, jobs ${JOBS:-all})"

declare -A PIDS=()
run_suite() {
    local t="$1" s rc=0
    s="$(date +%s)"
    bash "scripts/deploy/tests/test-${t}.sh" >"${RUN_LOG_DIR}/${t}.log" 2>&1 || rc=$?
    printf '%s %s\n' "${rc}" "$(( $(date +%s) - s ))" >"${RUN_LOG_DIR}/${t}.rc"
}
running() {
    local n=0 t
    for t in "${!PIDS[@]}"; do
        if kill -0 "${PIDS[${t}]}" 2>/dev/null; then n=$((n + 1)); fi
    done
    printf '%s' "${n}"
}
for t in ${SUITES}; do
    if [ "${JOBS}" != "0" ]; then
        while [ "$(running)" -ge "${JOBS}" ]; do sleep 0.2; done
    fi
    run_suite "${t}" &
    PIDS[${t}]=$!
done
pass=0
fail=0
for t in ${SUITES}; do
    wait "${PIDS[${t}]}" || true
    read -r rc secs <"${RUN_LOG_DIR}/${t}.rc"
    if [ "${rc}" = "0" ]; then
        pass=$((pass + 1))
        log "suite ${t}: PASS in ${secs}s ($(grep -c '^PASS' "${RUN_LOG_DIR}/${t}.log" || true) checks)"
    else
        fail=$((fail + 1))
        log "SUITE FAILED: ${t} (rc ${rc}, ${secs}s); failures:"
        grep -E '^FAIL' "${RUN_LOG_DIR}/${t}.log" | sed 's/^/    /' || tail -5 "${RUN_LOG_DIR}/${t}.log" | sed 's/^/    /'
    fi
done
log "suites passed: ${pass}, failed: ${fail}; total $(( $(date +%s) - STARTED ))s"
[ "${fail}" = "0" ]
