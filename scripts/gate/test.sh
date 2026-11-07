#!/usr/bin/env bash
# Быстрые проверки аргументов и предусловий scripts/gate-remote.sh и
# scripts/gate/in-container.sh: без сети, без docker и без прогона гейта.
# Полный прогон проверяет scripts/gate-remote.sh --local.
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

if command -v shellcheck >/dev/null 2>&1; then
    shellcheck scripts/gate-remote.sh scripts/gate/in-container.sh scripts/gate/test.sh \
        || fail "shellcheck"
fi

printf 'все быстрые проверки прошли\n'
