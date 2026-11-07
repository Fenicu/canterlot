#!/usr/bin/env bash
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
# Прогон всей обвязки scripts/deploy/tests: shellcheck, синтаксис python,
# затем все тестовые файлы по порядку. Ничего не трогает прод:
# harness ходит только в temp-каталоги через fake-ssh (настоящие хосты — отказ).
set -euo pipefail

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
cd "${DEPLOY_DIR}/../.."

log() { printf '[harness-run] %s\n' "$*"; }

log "shellcheck"
shellcheck scripts/deploy/*.sh scripts/deploy/tests/*.sh scripts/deploy/tests/fake-bin/*
log "shellcheck clean"

log "python syntax"
python3 -c "import ast; ast.parse(open('scripts/deploy/config-check.py').read()); ast.parse(open('scripts/deploy/ownership-check.py').read()); ast.parse(open('scripts/deploy/spec-shape.py').read())"
log "python OK"

log "sha_match unit checks"
# shellcheck disable=SC1091
source scripts/deploy/lib.sh
sha_match 8be10fce886b 8be10fce || { log "sha_match full-vs-short failed"; exit 1; }
sha_match 8be10fce 8be10fce886b || { log "sha_match short-vs-full failed"; exit 1; }
sha_match d3efc1e d3efc1e || { log "sha_match equal failed"; exit 1; }
if sha_match d3efc1e ba35992; then log "sha_match different passed (must fail)"; exit 1; fi
if sha_match "" d3efc1e; then log "sha_match empty passed (must fail)"; exit 1; fi
if sha_match abc abcdef1; then log "sha_match short passed (must fail)"; exit 1; fi
if sha_match UNREADABLE d3efc1e; then log "sha_match unreadable passed (must fail)"; exit 1; fi
log "sha_match OK"

log "release id unit checks"
# shellcheck source=/dev/null
source scripts/deploy/lib.sh
for ok in 8be10fce886b 8be10fce886b8be10fce886b8be10fce886b8be1; do
    is_release_id "${ok}" || { log "release id rejected: ${ok}"; exit 1; }
done
for bad in "" 8be10fce 8be10fce886g "';>PWN;#xxxx" rel000000001; do
    if is_release_id "${bad}"; then log "bad release id accepted: ${bad}"; exit 1; fi
done
log "release id OK"

pass=0
fail=0
for t in guardrails invariants preflight stage retain verify dryrun rollback-db cutover; do
    log "suite: ${t}"
    if bash "scripts/deploy/tests/test-${t}.sh"; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        log "SUITE FAILED: ${t}"
    fi
done
log "suites passed: ${pass}, failed: ${fail}"
[ "${fail}" = "0" ]
