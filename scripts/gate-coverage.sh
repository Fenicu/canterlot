#!/usr/bin/env bash
# Доказательство покрытия гейта: веб-прогон и шаги Rust, речи, статики и vitest
# идут теми же скриптами, что и до удаления браузерных тестов 22.11.2026.
# Сравнение идентичностей Playwright больше не нужно: браузерных тестов нет.
#
#   scripts/gate-coverage.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
gate=scripts/gate.sh
check=scripts/check.sh
web_check=scripts/check-web.sh
fail() {
    printf 'ПРОВАЛ: %s\n' "$*" >&2
    exit 1
}

[[ -f "$gate" && -f "$check" && -f "$web_check" ]] || fail 'нет scripts/gate.sh, check.sh или check-web.sh'

# --- шаги без браузера: те же скрипты и команды ---
# Гейт обязан состоять ровно из одного npm ci, check.sh --all --no-web,
# набора scripts/deploy/tests/run.sh и check-web.sh; комментарии в счёте
# не участвуют.
code=$(grep -vE '^[[:space:]]*#' "$gate")
check_code=$(grep -vE '^[[:space:]]*#' "$check")
web_code=$(grep -vE '^[[:space:]]*#' "$web_check")
[[ $(printf '%s\n' "$code" | grep -cE '&& npm ci |^[[:space:]]*npm ci ') == 1 ]] \
    || fail 'в gate.sh не один npm ci'
printf '%s\n' "$code" | grep -q 'bash scripts/check.sh --all --no-web' \
    || fail 'gate.sh не зовёт check.sh --all --no-web'
printf '%s\n' "$code" | grep -q 'bash scripts/check-web.sh' \
    || fail 'gate.sh не зовёт check-web.sh'

# --no-web обязан снимать только веб-этап: единственное место, где флаг меняет
# выбор, — want_web=0; Rust и речь от него не зависят.
grep -q '((no_web)) && want_web=0' "$check" || fail 'check.sh: --no-web не снимает want_web'
if grep -E 'no_web.*want_(rust|speech)' "$check" >/dev/null; then
    fail 'check.sh: --no-web трогает Rust или речь'
fi

# Команды Rust, речи и статических проверок должны остаться на месте.
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
)
for step in "${rust_steps[@]}"; do
    printf '%s\n' "$check_code" | grep -qF -- "$step" || fail "check.sh: пропал шаг «$step»"
done
for step in "${web_steps[@]}"; do
    printf '%s\n' "$web_code" | grep -qF -- "$step" || fail "check-web.sh: пропал шаг «$step»"
done

# --- контейнерный путь (gate-remote.sh, runner-gate.sh) = те же шаги ---
# Гейт на раннере и gate-remote.sh --local гоняют scripts/gate/in-container.sh. Он не
# перечисляет шаги заново, а зовёт сам scripts/gate.sh --no-attest: npm ci, check.sh --all
# --no-web, наборы scripts/deploy/tests/run.sh и check-web.sh идут оттуда же, поэтому
# отдельного списка, который мог бы разойтись, нет. Проверяем: путь действительно
# делегирует, не подменяет шаги и не пропускает наборы выката, а образ несёт то, что им нужно.
runner_gate=scripts/gate/runner-gate.sh
in_container=scripts/gate/in-container.sh
gate_remote=scripts/gate-remote.sh
dockerfile=docker/gate/Dockerfile
for f in "$runner_gate" "$in_container" "$gate_remote" "$dockerfile"; do
    [[ -f "$f" ]] || fail "нет $f"
done
container_code=$(grep -vE '^[[:space:]]*#' "$in_container")
printf '%s\n' "$container_code" | grep -qE 'bash scripts/gate\.sh --no-attest' \
    || fail 'in-container.sh не зовёт scripts/gate.sh --no-attest: контейнерный путь перестал совпадать с гейтом'
printf '%s\n' "$container_code" | grep -qE 'CANTERLOT_NO_LEASE=1' \
    || fail 'in-container.sh не отключает аренду: гейт в контейнере ждал бы воркер, которого там нет'
# Шагов в контейнере по отдельности нет: каждый такой вызов был бы вторым списком.
if printf '%s\n' "$container_code" | grep -qE 'scripts/check(-web)?\.sh|npm (ci|run)|deploy/tests/run\.sh'; then
    fail 'in-container.sh вызывает шаги сам, а не через scripts/gate.sh: списки шагов могут разойтись'
fi
# Тестовые швы подменяют или пропускают шаги: в контейнерном пути их быть не должно.
for f in "$in_container" "$runner_gate" "$gate_remote"; do
    if grep -vE '^[[:space:]]*#' "$f" | grep -qE 'CANTERLOT_GATE_(STEP_|DEPLOY_NEEDED)'; then
        fail "$f задаёт тестовые швы CANTERLOT_GATE_*: шаги гейта были бы подменены или пропущены"
    fi
done
# Наборы выката идут в гейте и, значит, в контейнере; им нужны shellcheck и zstd,
# а тестам носителя — openssh (ssh, ssh-keygen, sshd) и rsync для копирования файлов.
printf '%s\n' "$code" | grep -q 'scripts/deploy/tests/run.sh' \
    || fail 'gate.sh не зовёт scripts/deploy/tests/run.sh'
for tool in shellcheck zstd openssh-client openssh-server rsync; do
    grep -qE "^[[:space:]]+$tool( |\\\\|$)" "$dockerfile" \
        || fail "docker/gate/Dockerfile: нет пакета $tool, нужного наборам scripts/deploy/tests и тестам носителя"
done
# runner-gate.sh гоняет именно in-container.sh из той же рабочей копии, а gate.sh
# --runner — именно runner-gate.sh на полном sha.
grep -vE '^[[:space:]]*#' "$runner_gate" | grep -q 'bash /build/in-container.sh' \
    || fail 'runner-gate.sh не запускает in-container.sh'
# shellcheck disable=SC2016 # ищем буквальный текст «$in_container», а не его значение
grep -vE '^[[:space:]]*#' "$runner_gate" | grep -qF -- '"$in_container:/build/in-container.sh:ro"' \
    || fail 'runner-gate.sh берёт in-container.sh не из рабочей копии'
# shellcheck disable=SC2016 # ищем буквальный текст «$sha»
printf '%s\n' "$code" | grep -qF 'scripts/gate/runner-gate.sh "$sha"' \
    || fail 'gate.sh --runner не запускает scripts/gate/runner-gate.sh на полном sha'

printf 'Не-Playwright шаги:\n'
printf '  версия, fmt, clippy, nextest, речь                 %s\n' "$check"
printf '  статические проверки, vitest, сборки, vitest.build  %s\n' "$web_check"
printf '  --no-web снимает только веб-этап, Rust и речь целы: да\n'
printf '  один npm ci, check.sh --all --no-web, check-web.sh: да\n'
printf '  шаги не изменились: да\n'
printf '  контейнерный путь (gate-remote.sh, runner-gate.sh) = scripts/gate.sh, наборы выката включены: да\n'
printf 'Итог: покрытие совпадает\n'
