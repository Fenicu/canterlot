#!/usr/bin/env bash
# Быстрые проверки аргументов и предусловий scripts/build-release.sh: без сети,
# без docker и без сборки. Полную сборку проверяет scripts/build-release.sh --local.
set -euo pipefail
export LC_ALL=C

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
build=scripts/build-release.sh
# Литеральный путь: тест удаляет только свой каталог и только с меткой.
tmp=/tmp/canterlot-release-build-test
if [[ -e "$tmp" ]]; then
    [[ -f "$tmp/.canterlot-release-build-test" ]] || {
        echo "FATAL: $tmp занят не этим тестом" >&2
        exit 1
    }
    rm -rf /tmp/canterlot-release-build-test
fi
mkdir -p /tmp/canterlot-release-build-test
: >/tmp/canterlot-release-build-test/.canterlot-release-build-test
trap 'rm -rf /tmp/canterlot-release-build-test' EXIT

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
out="$tmp/log"
run() {
    local want=$1
    shift
    local got=0
    "$@" >"$out" 2>&1 || got=$?
    [[ "$got" == "$want" ]] || { cat "$out" >&2; fail "ожидался код $want, получен $got: $*"; }
}

run 2 bash "$build"
grep -q 'Использование' "$out" || fail "нет подсказки при пустых аргументах"

run 2 bash "$build" 1.2.3
run 2 bash "$build" v1.2
run 2 bash "$build" v1.2.3.4
run 0 bash "$build" --help
grep -q 'Использование' "$out" || fail "--help не печатает подсказку"

run 1 bash "$build" v0.0.0 --local
grep -q 'локального тега v0.0.0 нет' "$out" || fail "нет отказа для отсутствующего тега"

tag=$(git tag -l 'v*' --sort=-v:refname | sed -n '1p')
if [[ -z "$tag" ]]; then
    echo 'пропуск проверок с тегом: в репозитории нет тегов v*'
    exit 0
fi
sha12=$(git rev-parse --short=12 "$tag^{commit}")

# Существующий dist отказывается до обращения к docker и к сети: CANTERLOT_DOCKER
# заведомо несуществующий, и сообщение должно быть именно про каталог.
mkdir -p "$tmp/dist/$sha12"
: >"$tmp/dist/$sha12/marker"
run 1 env CANTERLOT_DIST_ROOT="$tmp/dist" CANTERLOT_DOCKER=/nonexistent/canterlot-docker \
    bash "$build" "$tag" --local
grep -q 'уже существует' "$out" || { cat "$out" >&2; fail "нет отказа для существующего dist"; }

# --local без docker: пустой dist, docker подменён на несуществующий путь.
mkdir -p "$tmp/dist-empty"
run 1 env CANTERLOT_DIST_ROOT="$tmp/dist-empty" CANTERLOT_DOCKER=/nonexistent/canterlot-docker \
    bash "$build" "$tag" --local
grep -q 'нет docker' "$out" || { cat "$out" >&2; fail "нет отказа без docker"; }

# Неизвестный аргумент — ошибка использования, а не тихая сборка.
run 2 bash "$build" "$tag" --remote

# Аргументы in-container.sh проверяются до docker и сборки.
mkdir -p "$tmp/src" "$tmp/outdir"
run 2 bash scripts/release-build/in-container.sh
run 1 bash scripts/release-build/in-container.sh --src "$tmp/src" --out "$tmp/outdir" \
    --sha not-a-sha --commit 2f1ebd179c9b77cc4e822366d27c40ff3207f690
run 1 bash scripts/release-build/in-container.sh --src "$tmp/src" --out "$tmp/outdir" \
    --sha 2f1ebd179c9b --commit 0000000000000000000000000000000000000000
grep -q 'не начало --commit' "$out" || fail "нет отказа при несовпадении --sha и --commit"
run 1 env -u CARGO_TARGET_DIR bash scripts/release-build/in-container.sh \
    --src "$tmp/src" --out "$tmp/outdir" \
    --sha 2f1ebd179c9b --commit 2f1ebd179c9b77cc4e822366d27c40ff3207f690
grep -q 'CARGO_TARGET_DIR не задан' "$out" || fail "нет отказа без CARGO_TARGET_DIR"

# EXPECT пишется до SHA256SUMS, иначе он не попадёт в суммы выпуска.
ic=scripts/release-build/in-container.sh
# shellcheck disable=SC2016 # ищется буквальный текст скрипта
expect_line=$(grep -nF '>"$out/EXPECT"' "$ic" | cut -d: -f1)
sums_line=$(grep -nF 'sha256sum >SHA256SUMS.new' "$ic" | cut -d: -f1)
[[ -n "$expect_line" && -n "$sums_line" ]] || fail "в in-container.sh нет записи EXPECT или SHA256SUMS"
((expect_line < sums_line)) || fail "EXPECT пишется после SHA256SUMS"

# Проверка на этой машине знает про каждый файл, который пишет контейнер: иначе готовый выпуск отбраковывается.
grep -qF 'csp.txt EXPECT; do' "$build" || fail "build-release.sh не требует EXPECT в выпуске"

# expect_of из in-container.sh на поддельном воркере: формат EXPECT и отказ на чужой строке.
expect_of_with() {
    printf '#!/bin/sh\nprintf "%s"\n' "$1" >"$tmp/fake-worker"
    chmod +x "$tmp/fake-worker"
    (
        # shellcheck disable=SC2317 # die вызывает expect_of из eval
        die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }
        eval "$(sed -n '/^expect_of() {$/,/^}$/p' "$ic")"
        expect_of "$tmp/fake-worker"
    )
}
run 0 expect_of_with 'capability a_v1\ncapability b_v1\nharness x\nharness y\n'
printf 'format=1\ncapabilities=a_v1 b_v1\nharnesses=x y\n' | cmp -s - "$out" \
    || { cat "$out" >&2; fail "expect_of печатает не тот EXPECT"; }
run 1 expect_of_with 'capability a_v1\nunknown z\n'
grep -q 'неожиданная строка' "$out" || fail "expect_of не отказывает на чужой строке"
run 1 expect_of_with ''

if command -v shellcheck >/dev/null 2>&1; then
    shellcheck scripts/build-release.sh scripts/release-build/in-container.sh scripts/release-build/test.sh \
        || fail "shellcheck"
fi

printf 'все быстрые проверки прошли\n'
