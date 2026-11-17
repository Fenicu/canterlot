#!/usr/bin/env bash
# Полный гейт перед выпуском. Заменяет частный /tmp/gate/run.sh и гоняет веб
# ровно один раз:
#   1. npm ci в web/ — один раз на весь прогон;
#   2. scripts/check.sh --all --no-web — версия, fmt, clippy, nextest, речь;
#   3. scripts/check-web.sh --stack — вся веб-половина (статические проверки,
#      vitest, две сборки PWA, vitest.build, Playwright e2e/ui и e2e/parity)
#      и сквозные сценарии на настоящем хабе.
#
#   scripts/gate.sh              гейт с аттестацией .superpowers/gate/<full sha>.ok
#   scripts/gate.sh --no-attest  гейт без аттестации (нужен, если рабочая копия грязная)
#   scripts/gate.sh --help
#
# Гейт берёт одну аренду gate на весь прогон (scripts/lease.sh). Переменные:
#   CANTERLOT_TEST_PG_PORT  порт тестовой PostgreSQL (по умолчанию 55441);
#   CANTERLOT_WORKER_BIN    бинарник воркера для аренды (по умолчанию установка
#                           воркера в $HOME/.local/opt/canterlot/current/bin);
#   CANTERLOT_GATE_TMPDIR   временный каталог гейта (по умолчанию /tmp: сокет
#                           исполнения не влезает в SUN_LEN при длинном пути);
#   CANTERLOT_NO_LEASE=1    идти без аренды (отладка).
#
# Сводка и логи прогона — .superpowers/gate/<sha8>/. Аттестация пишется только
# при полном успехе, чистой рабочей копии и неизменном HEAD: release.sh ship
# принимает только аттестацию ровно того коммита, который выкатывает.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
original_args=("$@")

usage() {
    echo 'Использование: scripts/gate.sh [--no-attest]'
}

attest=1
while (($#)); do
    case "$1" in
        --no-attest) attest=0 ;;
        --help | -h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
    shift
done

# Значения частного скрипта остаются умолчаниями и перекрываются окружением.
export CANTERLOT_WORKER_BIN="${CANTERLOT_WORKER_BIN:-$HOME/.local/opt/canterlot/current/bin/canterlot-worker}"
export CANTERLOT_TEST_PG_PORT="${CANTERLOT_TEST_PG_PORT:-55441}"
# Сокет исполнения — <TMPDIR>/canterlot/<uuid>.sock, а supervisor::ipc отвергает
# путь длиннее 107 байт: длинный TMPDIR сессии роняет тесты хаба, супервизоров и
# адаптеров. Как и контейнерный гейт, берём короткий /tmp; свой путь — в
# CANTERLOT_GATE_TMPDIR.
export TMPDIR="${CANTERLOT_GATE_TMPDIR:-/tmp}"

# Тестовые швы: подмена шагов и выбора наборов выката. В обычном прогоне пусто;
# они используются только scripts/gate/test.sh, чтобы гонять гейт без сети.
step_npm="${CANTERLOT_GATE_STEP_NPM:-}"
step_check="${CANTERLOT_GATE_STEP_CHECK:-}"
step_deploy="${CANTERLOT_GATE_STEP_DEPLOY:-}"
step_web="${CANTERLOT_GATE_STEP_WEB:-}"

clean_tree() {
    # git status может упасть (битый индекс, отказ safe.directory): пустой вывод
    # тогда ничего не значит, поэтому проверка обязана провалиться, а не пройти.
    local out
    out=$(git status --porcelain) || return 1
    [[ -z "$out" ]]
}

# Грязная копия не даёт аттестации, а без неё гейт нужен только по явной просьбе.
if ((attest)) && ! clean_tree; then
    echo 'гейт: рабочая копия грязная, аттестация невозможна; запустите с --no-attest' >&2
    git status --short >&2 || true
    exit 1
fi

# shellcheck disable=SC1091 # путь к scripts/lease.sh задаётся рабочей копией
source scripts/lease.sh
lease_self_wrap gate "gate.sh${original_args[*]:+ ${original_args[*]}}" \
    bash "$PWD/scripts/gate.sh" "${original_args[@]}"

if [[ -n "${CANTERLOT_GATE_DEPLOY_NEEDED:-}" ]]; then
    deploy_needed() { [[ "${CANTERLOT_GATE_DEPLOY_NEEDED}" == 1 ]]; }
else
    # shellcheck disable=SC1091 # путь к scripts/deploy/changed.sh задаётся рабочей копией
    source scripts/deploy/changed.sh
    deploy_needed() { deploy_tests_needed all; }
fi

# Аренда могла ждать долго: начало прогона и HEAD фиксируются после неё.
sha=$(git rev-parse HEAD)
short=${sha:0:8}
log_dir=".superpowers/gate/$short"
mkdir -p "$log_dir"
started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
started=$SECONDS

if ((attest)) && ! clean_tree; then
    echo 'гейт: рабочая копия стала грязной, пока ждали аренду; аттестация невозможна' >&2
    git status --short >&2 || true
    exit 1
fi

printf 'гейт %s: pg=127.0.0.1:%s, логи %s\n' "$short" "$CANTERLOT_TEST_PG_PORT" "$log_dir"

# Куда шаги кладут machine-readable отчёты. JUnit nextest — фиксированный путь,
# отчёты Playwright задаются окружением и читаются конфигом web/playwright.config.ts.
junit="target/nextest/default/junit.xml"
export CANTERLOT_PW_JSON_REPORT="$PWD/$log_dir/pw-report"
export CANTERLOT_STACK_JSON_REPORT="$PWD/$log_dir/stack-report"

# --- запуск шага ---------------------------------------------------------------
# В обычном прогоне вызывает настоящий скрипт; в тесте — подменённую команду.
# Режим и выбор передаются подмене через окружение (GATE_STEP_*).
run_gate_step() {
    local kind=$1 cmd=$2 mode=$3
    shift 3
    local selection="$*"
    if [[ -n "$cmd" ]]; then
        GATE_STEP_KIND="$kind" GATE_STEP_MODE="$mode" GATE_STEP_SELECTION="$selection" \
            bash -c "$cmd" gate "$@"
    else
        case "$kind" in
            npm) (cd web && npm ci --no-audit --no-fund) ;;
            check) bash scripts/check.sh --all --no-web ;;
            deploy) bash scripts/deploy/tests/run.sh ;;
            web) bash scripts/check-web.sh --stack ;;
        esac
    fi
}

# --- разбор отчётов в идентичности упавших тестов ------------------------------
# Идентичности канонические: rust|binary|test, pw|project|file|title,
# deploy|suite, stack|spec|title. Те же строки идут в состояние и в flaky= аттестации.
rust_failures() {
    [[ -f "$junit" ]] || return 0
    python3 - "$junit" <<'PY'
import sys
import xml.etree.ElementTree as ET
try:
    root = ET.parse(sys.argv[1]).getroot()
except Exception:
    sys.exit(0)
for suite in root.iter('testsuite'):
    binary = suite.get('name', '')
    for case in suite.iter('testcase'):
        if any(child.tag in ('failure', 'error') for child in case):
            print(f"rust|{binary}|{case.get('name', '')}")
PY
}

pw_failures() {
    local prefix=$1 f files=() slot
    [[ -f "$prefix.json" ]] && files+=("$prefix.json")
    for slot in 1 2 3 4 5 6 7 8; do
        [[ -f "$prefix-$slot.json" ]] && files+=("$prefix-$slot.json")
    done
    ((${#files[@]})) || return 0
    python3 - "${files[@]}" <<'PY'
import json
import sys


def walk(suites, out):
    for suite in suites or []:
        for spec in suite.get('specs', []) or []:
            path = spec.get('file', '')
            spec_title = spec.get('title', '')
            for test in spec.get('tests', []) or []:
                results = test.get('results', []) or []
                if any(r.get('status') in ('failed', 'timedOut', 'crashed') or r.get('error') for r in results):
                    project = test.get('projectName') or spec.get('projectName') or ''
                    title = test.get('title', '')
                    full = f"{spec_title} › {title}" if spec_title and title else (spec_title or title)
                    out.append(f"pw|{project}|{path}|{full}")
        walk(suite.get('suites', []), out)


out = []
for path in sys.argv[1:]:
    try:
        with open(path) as handle:
            data = json.load(handle)
    except Exception:
        continue
    walk(data.get('suites', []), out)
seen = set()
for item in out:
    if item not in seen:
        seen.add(item)
        print(item)
PY
}

deploy_failures() {
    local dir=$1 f rc name
    [[ -d "$dir" ]] || return 0
    for f in "$dir"/*.rc; do
        [[ -f "$f" ]] || continue
        read -r rc _ <"$f" || continue
        [[ "$rc" == 0 ]] && continue
        name=${f##*/}
        printf 'deploy|%s\n' "${name%.rc}"
    done
}

# --- состояние прогона ---------------------------------------------------------
declare -a state_lines=()
state_add() { state_lines+=("$1"$'\t'"$2"); }
write_state() {
    local tmp="$log_dir/state.tsv.$$"
    printf '%s\n' "${state_lines[@]}" >"$tmp"
    mv -f "$tmp" "$log_dir/state.tsv"
}
step_failure_kind() {
    local status=$1
    shift
    if [[ "$status" == 0 || "$status" == skipped ]]; then
        printf 'ok\n'
    elif (($#)); then
        printf 'tests\n'
    else
        printf 'other\n'
    fi
}

# --- npm ci: один раз, дальше check-web.sh видит свежий node_modules ---
printf '\n==> npm ci\n'
s=$SECONDS
npm_status=0
run_gate_step npm "$step_npm" full >"$log_dir/npm-ci.log" 2>&1 || npm_status=$?
npm_s=$((SECONDS - s))
printf '    npm ci: %ds, код %s\n' "$npm_s" "$npm_status"

# --- Rust, речь и версия без веб-половины ---
printf '\n==> scripts/check.sh --all --no-web\n'
s=$SECONDS
check_status=0
run_gate_step check "$step_check" full >"$log_dir/check.log" 2>&1 || check_status=$?
check_s=$((SECONDS - s))
printf '    check.sh --all --no-web: %ds, код %s\n' "$check_s" "$check_status"

# --- наборы scripts/deploy: отдельным шагом, не вместе с веб-половиной ---
printf '\n==> scripts/deploy/tests/run.sh\n'
s=$SECONDS
deploy_exit=skipped
deploy_s=0
if deploy_needed; then
    deploy_exit=0
    mkdir -p "$log_dir/deploy"
    RUN_LOG_DIR="$PWD/$log_dir/deploy" run_gate_step deploy "$step_deploy" full \
        >"$log_dir/deploy.log" 2>&1 || deploy_exit=$?
    deploy_s=$((SECONDS - s))
    printf '    deploy tests: %ds, код %s\n' "$deploy_s" "$deploy_exit"
else
    printf '    deploy tests: skipped (no changes since the last release tag)\n'
fi

# --- вся веб-половина и сквозные сценарии: один прогон ---
printf '\n==> scripts/check-web.sh --stack\n'
s=$SECONDS
web_status=0
run_gate_step web "$step_web" full >"$log_dir/web.log" 2>&1 || web_status=$?
web_s=$((SECONDS - s))
printf '    check-web.sh --stack: %ds, код %s\n' "$web_s" "$web_status"

# --- идентичности упавших тестов и состояние ---
mapfile -t failed_rust_ids < <(rust_failures)
mapfile -t failed_pw_ids < <(pw_failures "$CANTERLOT_PW_JSON_REPORT")
mapfile -t failed_stack_ids < <(pw_failures "$CANTERLOT_STACK_JSON_REPORT")
mapfile -t failed_deploy_ids < <(deploy_failures "$log_dir/deploy")

state_add schema 1
state_add sha "$sha"
state_add started "$started_at"
state_add finished "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
state_add completed 1
state_add mode full
state_add npm_ci_exit "$npm_status"
state_add npm_ci_duration_s "$npm_s"
state_add check_exit "$check_status"
state_add check_duration_s "$check_s"
state_add check_failed "$(step_failure_kind "$check_status" "${failed_rust_ids[@]}")"
for id in "${failed_rust_ids[@]}"; do state_add rust_fail "$id"; done
state_add deploy_exit "$deploy_exit"
state_add deploy_duration_s "$deploy_s"
state_add deploy_failed "$(step_failure_kind "$deploy_exit" "${failed_deploy_ids[@]}")"
for id in "${failed_deploy_ids[@]}"; do state_add deploy_fail "$id"; done
state_add web_exit "$web_status"
state_add web_duration_s "$web_s"
state_add web_failed "$(step_failure_kind "$web_status" "${failed_pw_ids[@]}" "${failed_stack_ids[@]}")"
for id in "${failed_pw_ids[@]}"; do state_add pw_fail "$id"; done
for id in "${failed_stack_ids[@]}"; do state_add stack_fail "$id"; done
state_add npm_ci_failed "$(step_failure_kind "$npm_status")"
write_state

# Сводка в формате прежнего result.txt: коды и времена, затем nextest,
# Playwright и первые упавшие тесты.
mapfile -t rust_summary < <(grep -E 'Summary \[' "$log_dir/check.log" || true)
mapfile -t playwright_summary < <(grep -E '^\s+[0-9]+ (passed|failed|flaky|skipped|did not run)' "$log_dir/web.log" || true)
mapfile -t failed_rust < <(grep -E '^ +FAIL \[' "$log_dir/check.log" | head || true)
mapfile -t failed_web < <(grep -E '^\s+[0-9]+\) ' "$log_dir/web.log" | sort -u | head || true)

{
    printf '%s npm=%s check=%s %ss deploy=%s %ss web=%s %ss\n' \
        "$short" "$npm_status" "$check_status" "$check_s" "$deploy_exit" "$deploy_s" "$web_status" "$web_s"
    ((${#rust_summary[@]})) && printf '%s\n' "${rust_summary[@]}"
    ((${#playwright_summary[@]})) && printf '%s\n' "${playwright_summary[@]}"
    ((${#failed_rust[@]})) && printf '%s\n' "${failed_rust[@]}"
    ((${#failed_web[@]})) && printf '%s\n' "${failed_web[@]}"
    true
} >"$log_dir/result.txt"
cat "$log_dir/result.txt"
total_s=$((SECONDS - started))
printf 'гейт %s за %ds\n' "$short" "$total_s"

if ((npm_status != 0 || check_status != 0 || web_status != 0)) \
    || [[ "$deploy_exit" != 0 && "$deploy_exit" != skipped ]]; then
    printf 'гейт: провал, аттестация не пишется; логи в %s\n' "$log_dir" >&2
    exit 1
fi
if ((!attest)); then
    printf 'гейт: успех без аттестации (--no-attest), логи в %s\n' "$log_dir"
    exit 0
fi

# Аттестация честна только для того же чистого коммита, что был в начале.
head_now=$(git rev-parse HEAD)
if [[ "$head_now" != "$sha" ]]; then
    printf 'гейт: HEAD сместился с %s на %s за прогон; аттестация не пишется\n' "$sha" "$head_now" >&2
    exit 1
fi
if ! clean_tree; then
    echo 'гейт: рабочая копия стала грязной за прогон; аттестация не пишется' >&2
    git status --short >&2 || true
    exit 1
fi

attestation=".superpowers/gate/$sha.ok"
tmp="$attestation.tmp.$$"
{
    printf 'sha=%s\n' "$sha"
    printf 'started=%s\n' "$started_at"
    printf 'finished=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'duration_s=%s\n' "$total_s"
    printf 'npm_ci_exit=%s\n' "$npm_status"
    printf 'npm_ci_duration_s=%s\n' "$npm_s"
    printf 'check_exit=%s\n' "$check_status"
    printf 'check_duration_s=%s\n' "$check_s"
    printf 'deploy_exit=%s\n' "$deploy_exit"
    printf 'deploy_duration_s=%s\n' "$deploy_s"
    printf 'web_exit=%s\n' "$web_status"
    printf 'web_duration_s=%s\n' "$web_s"
    printf 'logs=%s\n' "$log_dir"
    while IFS= read -r line; do
        printf 'summary=%s\n' "$line"
    done <"$log_dir/result.txt"
} >"$tmp"
mv -f "$tmp" "$attestation"
printf 'гейт: аттестация записана в %s\n' "$attestation"
