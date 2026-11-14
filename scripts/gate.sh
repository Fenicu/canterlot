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

# --- npm ci: один раз, дальше check-web.sh видит свежий node_modules ---
printf '\n==> npm ci\n'
s=$SECONDS
npm_status=0
(cd web && npm ci --no-audit --no-fund) >"$log_dir/npm-ci.log" 2>&1 || npm_status=$?
npm_s=$((SECONDS - s))
printf '    npm ci: %ds, код %s\n' "$npm_s" "$npm_status"

# --- Rust, речь и версия без веб-половины ---
printf '\n==> scripts/check.sh --all --no-web\n'
s=$SECONDS
check_status=0
bash scripts/check.sh --all --no-web >"$log_dir/check.log" 2>&1 || check_status=$?
check_s=$((SECONDS - s))
printf '    check.sh --all --no-web: %ds, код %s\n' "$check_s" "$check_status"

# --- вся веб-половина и сквозные сценарии: один прогон ---
printf '\n==> scripts/check-web.sh --stack\n'
s=$SECONDS
web_status=0
bash scripts/check-web.sh --stack >"$log_dir/web.log" 2>&1 || web_status=$?
web_s=$((SECONDS - s))
printf '    check-web.sh --stack: %ds, код %s\n' "$web_s" "$web_status"

total_s=$((SECONDS - started))
finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# Сводка в формате прежнего result.txt: коды и времена, затем nextest,
# Playwright и первые упавшие тесты.
mapfile -t rust_summary < <(grep -E 'Summary \[' "$log_dir/check.log" || true)
mapfile -t playwright_summary < <(grep -E '^\s+[0-9]+ (passed|failed|flaky|skipped|did not run)' "$log_dir/web.log" || true)
mapfile -t failed_rust < <(grep -E '^ +FAIL \[' "$log_dir/check.log" | head || true)
mapfile -t failed_web < <(grep -E '^\s+[0-9]+\) ' "$log_dir/web.log" | sort -u | head || true)

{
    printf '%s npm=%s check=%s %ss web=%s %ss\n' \
        "$short" "$npm_status" "$check_status" "$check_s" "$web_status" "$web_s"
    ((${#rust_summary[@]})) && printf '%s\n' "${rust_summary[@]}"
    ((${#playwright_summary[@]})) && printf '%s\n' "${playwright_summary[@]}"
    ((${#failed_rust[@]})) && printf '%s\n' "${failed_rust[@]}"
    ((${#failed_web[@]})) && printf '%s\n' "${failed_web[@]}"
    true
} >"$log_dir/result.txt"
cat "$log_dir/result.txt"
printf 'гейт %s за %ds\n' "$short" "$total_s"

if ((npm_status != 0 || check_status != 0 || web_status != 0)); then
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
    printf 'finished=%s\n' "$finished_at"
    printf 'duration_s=%s\n' "$total_s"
    printf 'npm_ci_exit=%s\n' "$npm_status"
    printf 'npm_ci_duration_s=%s\n' "$npm_s"
    printf 'check_exit=%s\n' "$check_status"
    printf 'check_duration_s=%s\n' "$check_s"
    printf 'web_exit=%s\n' "$web_status"
    printf 'web_duration_s=%s\n' "$web_s"
    printf 'logs=%s\n' "$log_dir"
    while IFS= read -r line; do
        printf 'summary=%s\n' "$line"
    done <"$log_dir/result.txt"
} >"$tmp"
mv -f "$tmp" "$attestation"
printf 'гейт: аттестация записана в %s\n' "$attestation"
