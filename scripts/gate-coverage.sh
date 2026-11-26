#!/usr/bin/env bash
# Состав гейта по тексту скриптов: gate.sh зовёт ровно настоящие шаги (один npm ci,
# check.sh --all --no-web, профиль stress при X.Y.0 или --stress, наборы
# scripts/deploy/tests/run.sh, check-web.sh), check.sh и
# check-web.sh держат свои команды Rust, речи, статики и vitest, а контейнерный путь
# (gate-remote.sh, гейт на раннере) делегирует gate.sh и несёт пакеты наборов выката.
# scripts/gate/test.sh проверяет поведение гейта на подменённых шагах
# (CANTERLOT_GATE_STEP_*), поэтому настоящие команды шагов сверяются здесь.
# Идёт в каждом scripts/check.sh: доли секунды и никакой сборки.
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
# Текст без строк-комментариев. Поиск — по here-string, а не `printf | grep -q`:
# grep -q закрывает канал на первом совпадении, и под pipefail printf получает
# SIGPIPE (141) на большом тексте, а проверка ложно падает.
uncommented() { grep -vE '^[[:space:]]*#' "$1" || true; }
has() {
    local text=$1
    shift
    grep -q "$@" <<<"$text"
}

[[ -f "$gate" && -f "$check" && -f "$web_check" ]] || fail 'нет scripts/gate.sh, check.sh или check-web.sh'

# --- шаги гейта: те же скрипты и команды ---
# Гейт обязан состоять ровно из одного npm ci, check.sh --all --no-web,
# набора scripts/deploy/tests/run.sh и check-web.sh; комментарии в счёте
# не участвуют.
code=$(uncommented "$gate")
check_code=$(uncommented "$check")
web_code=$(uncommented "$web_check")
[[ $(grep -cE '&& npm ci |^[[:space:]]*npm ci ' <<<"$code") == 1 ]] \
    || fail 'в gate.sh не один npm ci'
has "$code" 'bash scripts/check.sh --all --no-web' \
    || fail 'gate.sh не зовёт check.sh --all --no-web'
has "$code" 'bash scripts/check-web.sh' \
    || fail 'gate.sh не зовёт check-web.sh'
# Нагрузочный профиль — отдельный шаг гейта: он идёт при версии X.Y.0 или с --stress.
has "$code" -- '--profile stress' || fail 'gate.sh не запускает профиль stress'
has "$code" -- '--stress' || fail 'gate.sh не принимает --stress'

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
    'bash scripts/gate-coverage.sh'
)
web_steps=(
    'background check npm run check'
    'background lint npm run lint'
    'background tokens npm run lint:tokens'
    'npx --no-install vitest run'
    'VITE_UI_TESTS=1 npm run build'
    'npx --no-install vitest run --config vitest.build.config.ts'
)
for step in "${rust_steps[@]}"; do
    has "$check_code" -F -- "$step" || fail "check.sh: пропал шаг «$step»"
done
for step in "${web_steps[@]}"; do
    has "$web_code" -F -- "$step" || fail "check-web.sh: пропал шаг «$step»"
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
container_code=$(uncommented "$in_container")
runner_code=$(uncommented "$runner_gate")
has "$container_code" -E 'bash scripts/gate\.sh --no-attest' \
    || fail 'in-container.sh не зовёт scripts/gate.sh --no-attest: контейнерный путь перестал совпадать с гейтом'
has "$container_code" -E 'CANTERLOT_NO_LEASE=1' \
    || fail 'in-container.sh не отключает аренду: гейт в контейнере ждал бы воркер, которого там нет'
# Шагов в контейнере по отдельности нет: каждый такой вызов был бы вторым списком.
if has "$container_code" -E 'scripts/check(-web)?\.sh|npm (ci|run)|deploy/tests/run\.sh'; then
    fail 'in-container.sh вызывает шаги сам, а не через scripts/gate.sh: списки шагов могут разойтись'
fi
# Тестовые швы подменяют или пропускают шаги: в контейнерном пути их быть не должно.
for f in "$in_container" "$runner_gate" "$gate_remote"; do
    if has "$(uncommented "$f")" -E 'CANTERLOT_GATE_(STEP_|DEPLOY_NEEDED)'; then
        fail "$f задаёт тестовые швы CANTERLOT_GATE_*: шаги гейта были бы подменены или пропущены"
    fi
done
# Наборы выката идут в гейте и, значит, в контейнере; им нужны shellcheck и zstd,
# а тестам носителя — openssh (ssh, ssh-keygen, sshd) и rsync для копирования файлов.
has "$code" 'scripts/deploy/tests/run.sh' \
    || fail 'gate.sh не зовёт scripts/deploy/tests/run.sh'
for tool in shellcheck zstd openssh-client openssh-server rsync; do
    grep -qE "^[[:space:]]+$tool( |\\\\|$)" "$dockerfile" \
        || fail "docker/gate/Dockerfile: нет пакета $tool, нужного наборам scripts/deploy/tests и тестам носителя"
done
# runner-gate.sh гоняет именно in-container.sh из той же рабочей копии, а gate.sh
# --runner — именно runner-gate.sh на полном sha.
has "$runner_code" 'bash /build/in-container.sh' \
    || fail 'runner-gate.sh не запускает in-container.sh'
# shellcheck disable=SC2016 # ищем буквальный текст «$in_container», а не его значение
has "$runner_code" -F -- '"$in_container:/build/in-container.sh:ro"' \
    || fail 'runner-gate.sh берёт in-container.sh не из рабочей копии'
# shellcheck disable=SC2016 # ищем буквальный текст «$sha»
has "$code" -F 'scripts/gate/runner-gate.sh "$sha"' \
    || fail 'gate.sh --runner не запускает scripts/gate/runner-gate.sh на полном sha'

printf 'Состав гейта:\n'
printf '  версия, fmt, clippy, nextest, речь, эта сверка      %s\n' "$check"
printf '  статические проверки, vitest, сборки, vitest.build  %s\n' "$web_check"
printf '  --no-web снимает только веб-этап, Rust и речь целы: да\n'
printf '  один npm ci, check.sh --all --no-web, профиль stress, наборы выката, check-web.sh: да\n'
printf '  контейнерный путь (gate-remote.sh, runner-gate.sh) = scripts/gate.sh, наборы выката включены: да\n'
printf 'Итог: состав гейта на месте\n'
