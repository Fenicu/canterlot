#!/usr/bin/env bash
# Доказательство покрытия гейта: набор тестов Playwright, который запускает
# scripts/gate.sh, совпадает с объединением прогонов прежней последовательности
# (/tmp/gate/run.sh: check.sh --all → check-web.sh полный → check-web.sh --stack).
# Сравниваются идентичности (проект, спека, тест) из `playwright test --list` в
# тех же конфигурациях, что у обоих путей. Плюс печатается сверка остальных
# шагов: версия, Rust, речь, статические проверки и vitest идут теми же
# скриптами, а --no-web снимает только веб-этап check.sh.
#
#   scripts/gate-coverage.sh
#
# Нужен установленный web/node_modules: его ставит npm ci (scripts/gate.sh).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
gate=scripts/gate.sh
check=scripts/check.sh
web_check=scripts/check-web.sh
fail() {
    printf 'ПРОВАЛ: %s\n' "$*" >&2
    exit 1
}

[[ -x web/node_modules/.bin/playwright ]] \
    || fail 'нет web/node_modules; сначала выполните npm ci в web/ (это делает scripts/gate.sh)'
[[ -f "$gate" && -f "$check" && -f "$web_check" ]] || fail 'нет scripts/gate.sh, check.sh или check-web.sh'

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# `playwright test --list` печатает строки «  [проект] › спека:строка › тест»;
# остальное (заголовок и Total) отбрасываем. Сортировка и уникальность делают
# набор независимым от порядка обхода файлов.
list() {
    local out=$1
    shift
    (cd web && npx --no-install playwright test "$@" --list) \
        | grep -E '^  \[' | sort -u >"$out" \
        || fail "playwright --list не удался: $*"
}
count() { wc -l <"$1" | tr -d ' '; }

# Старый путь: полный веб-прогон внутри check.sh, такой же прогон внутри
# check-web.sh --stack, затем сквозные сценарии. Новый: один веб-прогон
# check-web.sh --stack и те же сквозные сценарии.
list "$tmp/old-full" e2e/ui e2e/parity
list "$tmp/old-stack-web" e2e/ui e2e/parity
list "$tmp/old-stack" e2e/stack
list "$tmp/new-web" e2e/ui e2e/parity
list "$tmp/new-stack" e2e/stack

# Оба прежних веб-прогона обязаны быть одинаковыми: это одна и та же команда
# check-web.sh в полном и в --stack режимах.
cmp -s "$tmp/old-full" "$tmp/old-stack-web" \
    || fail 'полный и --stack веб-прогоны запускают разные наборы Playwright'

cat "$tmp/old-full" "$tmp/old-stack-web" "$tmp/old-stack" | sort -u >"$tmp/old-union"
cat "$tmp/new-web" "$tmp/new-stack" | sort -u >"$tmp/new-union"
old_count=$(count "$tmp/old-union")
new_count=$(count "$tmp/new-union")

printf 'Сверка покрытия гейта\n'
printf 'Playwright, идентичности (проект, спека, тест):\n'
printf '  прежний: check.sh --all → check-web.sh (полный)   %s\n' "$(count "$tmp/old-full")"
printf '  прежний: check-web.sh --stack, веб-прогон         %s\n' "$(count "$tmp/old-stack-web")"
printf '  новый:   check-web.sh --stack, веб-прогон         %s\n' "$(count "$tmp/new-web")"
printf '  сквозные сценарии (прежний и новый)               %s\n' "$(count "$tmp/old-stack")"
if ! cmp -s "$tmp/old-union" "$tmp/new-union"; then
    diff -u "$tmp/old-union" "$tmp/new-union" | head -40 >&2 || true
    fail "наборы различаются: прежний $old_count, новый $new_count"
fi
printf '  объединение прежних = новый: да, %s идентичностей\n' "$new_count"

# --- не-Playwright шаги: те же скрипты и команды ---
# Гейт обязан состоять ровно из одного npm ci, check.sh --all --no-web и
# check-web.sh --stack; комментарии в счёте не участвуют.
code=$(grep -vE '^[[:space:]]*#' "$gate")
[[ $(printf '%s\n' "$code" | grep -cE '&& npm ci |^[[:space:]]*npm ci ') == 1 ]] \
    || fail 'в gate.sh не один npm ci'
printf '%s\n' "$code" | grep -q 'bash scripts/check.sh --all --no-web' \
    || fail 'gate.sh не зовёт check.sh --all --no-web'
printf '%s\n' "$code" | grep -q 'bash scripts/check-web.sh --stack' \
    || fail 'gate.sh не зовёт check-web.sh --stack'

# --no-web обязан снимать только веб-этап: единственное место, где флаг меняет
# выбор, — want_web=0; Rust и речь от него не зависят.
grep -q '((no_web)) && want_web=0' "$check" || fail 'check.sh: --no-web не снимает want_web'
if grep -E 'no_web.*want_(rust|speech)' "$check" >/dev/null; then
    fail 'check.sh: --no-web трогает Rust или речь'
fi

# Команды Rust, речи, статических проверок, vitest, сборок и Playwright должны
# остаться на месте: гейт вызывает те же скрипты, что и прежняя последовательность.
rust_steps=(
    'bash scripts/check-version.sh'
    'cargo fmt --all --check'
    'cargo clippy --workspace --exclude canterlot-speech --all-targets --all-features -- -D warnings'
    'cargo clippy -p canterlot-speech --all-targets -- -D warnings'
    'cargo nextest run --workspace --exclude canterlot-speech'
    'cargo nextest run -p canterlot-speech'
    'cargo check --workspace --all-targets'
)
web_steps=(
    'background check npm run check'
    'background lint npm run lint'
    'background tokens npm run lint:tokens'
    'npx --no-install vitest run'
    'VITE_UI_TESTS=1 npm run build'
    'PWA_BUILD_DIR=build-update'
    'npx --no-install vitest run --config vitest.build.config.ts'
    'npx --no-install playwright test e2e/ui e2e/parity'
    'npm run stack:build && npm run test:stack'
)
for step in "${rust_steps[@]}"; do
    grep -qF -- "$step" "$check" || fail "check.sh: пропал шаг «$step»"
done
for step in "${web_steps[@]}"; do
    grep -qF -- "$step" "$web_check" || fail "check-web.sh: пропал шаг «$step»"
done

printf 'Не-Playwright шаги:\n'
printf '  версия, fmt, clippy, nextest, речь                 %s\n' "$check"
printf '  статические проверки, vitest, сборки, vitest.build, Playwright  %s\n' "$web_check"
printf '  --no-web снимает только веб-этап, Rust и речь целы: да\n'
printf '  один npm ci, check.sh --all --no-web, check-web.sh --stack: да\n'
printf '  шаги не изменились: да\n'
printf 'Итог: покрытие совпадает\n'
