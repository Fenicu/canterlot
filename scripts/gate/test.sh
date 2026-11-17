#!/usr/bin/env bash
# Быстрые проверки аргументов и предусловий scripts/gate.sh,
# scripts/gate-remote.sh и scripts/gate/in-container.sh: без сети, без docker
# и без прогона гейта. Полный прогон проверяет scripts/gate-remote.sh --local.
set -euo pipefail
export LC_ALL=C

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
gate=scripts/gate-remote.sh
# Литеральный путь: тест удаляет только свой каталог и только с меткой.
tmp=/tmp/canterlot-gate-test
if [[ -e "$tmp" ]]; then
    [[ -f "$tmp/.canterlot-gate-test" ]] || {
        echo "FATAL: $tmp занят не этим тестом" >&2
        exit 1
    }
    rm -rf /tmp/canterlot-gate-test
fi
mkdir -p /tmp/canterlot-gate-test
: >/tmp/canterlot-gate-test/.canterlot-gate-test
trap 'rm -rf /tmp/canterlot-gate-test' EXIT

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
out="$tmp/log"
run() {
    local want=$1
    shift
    local got=0
    "$@" >"$out" 2>&1 || got=$?
    [[ "$got" == "$want" ]] || { cat "$out" >&2; fail "ожидался код $want, получен $got: $*"; }
}

run 2 bash "$gate" --bogus
grep -q 'Использование' "$out" || fail "нет подсказки при неизвестном аргументе"
run 0 bash "$gate" --help
grep -q 'Использование' "$out" || fail "--help не печатает подсказку"

run 1 bash "$gate" nosuchrev --local
grep -q 'нет такого коммита' "$out" || { cat "$out" >&2; fail "нет отказа для отсутствующего коммита"; }

sha12=$(git rev-parse --short=12 HEAD)

# Существующий каталог результата отказывается до обращения к docker: CANTERLOT_DOCKER
# заведомо несуществующий, и сообщение должно быть именно про каталог.
mkdir -p "$tmp/out/$sha12"
: >"$tmp/out/$sha12/marker"
run 1 env CANTERLOT_GATE_OUT="$tmp/out" CANTERLOT_DOCKER=/nonexistent/canterlot-docker \
    bash "$gate" HEAD --local
grep -q 'уже существует' "$out" || { cat "$out" >&2; fail "нет отказа для существующего каталога"; }

# --local без docker: пустой каталог результата, docker подменён на несуществующий путь.
mkdir -p "$tmp/out-empty"
run 1 env CANTERLOT_GATE_OUT="$tmp/out-empty" CANTERLOT_DOCKER=/nonexistent/canterlot-docker \
    bash "$gate" HEAD --local
grep -q 'нет docker' "$out" || { cat "$out" >&2; fail "нет отказа без docker"; }

# Аргументы in-container.sh проверяются до базы, npm и docker.
mkdir -p "$tmp/src" "$tmp/outdir" "$tmp/full-out"
: >"$tmp/full-out/marker"
run 2 bash scripts/gate/in-container.sh
run 1 bash scripts/gate/in-container.sh --src "$tmp/src" --out "$tmp/outdir" --sha not-a-sha
grep -q 'не sha12' "$out" || { cat "$out" >&2; fail "нет отказа для неверного --sha"; }
run 1 bash scripts/gate/in-container.sh --src "$tmp/missing" --out "$tmp/outdir" \
    --sha "$sha12"
grep -q 'нет каталога исходников' "$out" || { cat "$out" >&2; fail "нет отказа без --src"; }
run 1 bash scripts/gate/in-container.sh --src "$tmp/src" --out "$tmp/full-out" \
    --sha "$sha12"
grep -q 'не пуст' "$out" || { cat "$out" >&2; fail "нет отказа для непустого --out"; }

# --- scripts/gate.sh: отказы и аренда без прогона проверок ---
# Гейт проверяет чистоту копии до аренды, а аренду — до npm ci: это проверяется
# в пустом репозитории, без единого реального шага.
scratch="$tmp/scratch"
mkdir -p "$scratch/scripts"
cp scripts/gate.sh scripts/lease.sh "$scratch/scripts/"
chmod 0755 "$scratch/scripts/gate.sh"
(
    cd "$scratch"
    git init -q
    git config user.email gate@example.invalid
    git config user.name gate
    git config commit.gpgsign false
    : >tracked
    git add tracked scripts
    git commit -qm init
)
: >"$scratch/dirty"
run 1 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh"
grep -q -- '--no-attest' "$out" || { cat "$out" >&2; fail 'грязная копия: нет подсказки --no-attest'; }
[[ ! -e "$scratch/.superpowers" ]] || fail 'грязная копия успела создать .superpowers'
run 0 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh" --no-attest
grep -q 'kind=gate' "$out" || { cat "$out" >&2; fail '--no-attest не дошёл до аренды'; }
rm -f "$scratch/dirty"
run 0 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh"
grep -q 'kind=gate' "$out" || { cat "$out" >&2; fail 'чистая копия не дошла до аренды'; }
run 2 bash "$scratch/scripts/gate.sh" --bogus
run 0 bash "$scratch/scripts/gate.sh" --help

# --- машиночитаемое состояние полного прогона ---
# Шаги подменены короткими командами: они пишут те же отчёты, что настоящие шаги,
# и Notes в журнал вызовов, чтобы тест видел, что именно перезапускалось.
g2="$tmp/g2"
mkdir -p "$g2/scripts"
cp scripts/gate.sh scripts/lease.sh "$g2/scripts/"
chmod 0755 "$g2/scripts/gate.sh"
(
    cd "$g2"
    git init -q
    git config user.email gate@example.invalid
    git config user.name gate
    git config commit.gpgsign false
    : >tracked
    git add tracked scripts
    git commit -qm init
)
export GATE_FAKE_CALLS="$tmp/g2-calls"
: >"$GATE_FAKE_CALLS"
# shellcheck disable=SC2016 # подмены исполняются через bash -c, где переменные раскрываются
fake_npm='echo "npm $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 0'
# shellcheck disable=SC2016
fake_check='mkdir -p target/nextest/default; printf "%s\n" "<testsuites><testsuite name=\"crate-a\"><testcase name=\"mod::flaky\"><failure/></testcase><testcase name=\"mod::ok\"/></testsuite></testsuites>" >target/nextest/default/junit.xml; echo "check $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 1'
# shellcheck disable=SC2016
fake_deploy='echo "deploy $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 0'
# shellcheck disable=SC2016
fake_web='echo "web $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 0'
gate2_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=0
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="$fake_check"
    CANTERLOT_GATE_STEP_DEPLOY="$fake_deploy" CANTERLOT_GATE_STEP_WEB="$fake_web"
)
run 1 "${gate2_env[@]}" bash "$g2/scripts/gate.sh" --no-attest
short=$(git -C "$g2" rev-parse --short=8 HEAD)
state="$g2/.superpowers/gate/$short/state.tsv"
[[ -f "$state" ]] || fail 'нет машиночитаемого состояния прогона'
grep -qF $'schema\t1' "$state" || fail 'нет schema в состоянии'
grep -qF "$(printf 'sha\t%s' "$(git -C "$g2" rev-parse HEAD)")" "$state" || fail 'нет полного sha в состоянии'
grep -qF $'completed\t1' "$state" || fail 'нет признака завершения в состоянии'
grep -qF $'npm_ci_exit\t0' "$state" || fail 'нет кода npm ci в состоянии'
grep -qF $'check_exit\t1' "$state" || fail 'нет кода check в состоянии'
grep -qF $'check_failed\ttests' "$state" || fail 'падение check не отнесено к тестам'
grep -qF $'rust_fail\trust|crate-a|mod::flaky' "$state" || fail 'нет идентичности упавшего Rust-теста'
grep -qF $'deploy_exit\tskipped' "$state" || fail 'нет пропущенного deploy в состоянии'
if grep -qF $'rust_fail\trust|crate-a|mod::ok' "$state"; then fail 'прошедший тест попал в упавшие'; fi

# Битый индекс: git status падает, и проверка чистоты обязана провалиться, а не
# пропустить аттестацию из-за пустого вывода.
printf 'garbage' >"$scratch/.git/index"
run 1 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh"
grep -q 'грязная' "$out" || { cat "$out" >&2; fail 'битый индекс не остановил гейт'; }

if command -v shellcheck >/dev/null 2>&1; then
    shellcheck scripts/gate.sh scripts/gate-coverage.sh scripts/gate-remote.sh \
        scripts/gate/in-container.sh scripts/gate/test.sh \
        || fail "shellcheck"
fi

printf 'все быстрые проверки прошли\n'
