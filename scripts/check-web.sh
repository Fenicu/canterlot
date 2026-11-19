#!/usr/bin/env bash
# Проверка веба.
#   scripts/check-web.sh                 сверка сгенерированных типов API, svelte-check, eslint, токены, vitest, проверки собранных артефактов, Playwright e2e/ui и e2e/parity;
#                                        полнота карты областей e2e/areas.json проверяется в обоих режимах
#   scripts/check-web.sh --fast          то же без полного Playwright, vitest только по изменённым файлам;
#                                        проверки собранных артефактов — только если была сборка;
#                                        по diff выбираются изменённые спеки и спеки затронутых областей
#                                        из e2e/areas.json (phone и desktop; спека desktop-layout — проекты
#                                        layout-1024/1440/2560/3840), глобальный путь — весь набор
#                                        только на phone; сгенерированные типы API — только проверка типов
#   scripts/check-web.sh --fast --base REF  сравнивать с REF (по умолчанию main)
#   scripts/check-web.sh --stack         полная плюс сквозные сценарии на настоящем хабе (перед выкаткой)
# Прогон встаёт в очередь тяжёлых задач воркера: --stack берёт аренду gate, остальные
# режимы — build. Число воркеров Playwright задаёт аренда через PW_WORKERS.
set -euo pipefail

cd "$(dirname "$0")/../web"
original_args=("$@")

fast=0
stack=0
ui_status=0
built=0
base=main
while (($#)); do
    case "$1" in
        --fast) fast=1 ;;
        --stack) stack=1 ;;
        --base) base="${2:?--base требует ссылку}"; shift ;;
        *) echo 'Использование: bash scripts/check-web.sh [--fast] [--base REF] [--stack]' >&2; exit 2 ;;
    esac
    shift
done
if ((fast && stack)); then
    echo '--fast и --stack несовместимы' >&2
    exit 2
fi
lease_kind=build
((stack)) && lease_kind=gate
# shellcheck source=scripts/lease.sh
source ../scripts/lease.sh
lease_self_wrap "$lease_kind" "check-web.sh${original_args[*]:+ ${original_args[*]}}" \
    bash "$(cd .. && pwd)/scripts/check-web.sh" "${original_args[@]}"

# Lock-файл ставится и вне локальной сети: в нём только публичный реестр npm.
[[ -f package-lock.json ]] || { echo 'BLOCKED: нет web/package-lock.json' >&2; exit 1; }
foreign=$(grep -oE '"resolved": "[^"]+' package-lock.json | grep -vc '"resolved": "https://registry.npmjs.org/' || true)
if ((foreign)); then
    echo "BLOCKED: в web/package-lock.json $foreign ссылок не на https://registry.npmjs.org/;" \
        "заменить адрес реестра в полях resolved" >&2
    exit 1
fi

if [[ $(df --output=avail -BG / | tail -1 | tr -dc '0-9') -lt 5 ]]; then
    echo 'BLOCKED: менее 5 ГБ свободного места' >&2
    exit 1
fi
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/canterlot-playwright}"
export TOKIO_WORKER_THREADS="${TOKIO_WORKER_THREADS:-4}"
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-4}"
export MOZ_TASKCONTROLLER_THREADCOUNT="${MOZ_TASKCONTROLLER_THREADCOUNT:-4}"
export GSETTINGS_BACKEND="${GSETTINGS_BACKEND:-memory}"
# Каждый прогон берёт свой свободный порт: параллельные проверки из соседних рабочих копий не делят 4173.
export PORT="${PORT:-$(node -e "const s=require('net').createServer();s.listen(0,'127.0.0.1',()=>{console.log(s.address().port);s.close()})")}"

started=$SECONDS
logs=$(mktemp -d)
trap 'rm -rf "$logs"' EXIT
declare -a pids=() names=()
background() {
    local name=$1
    shift
    "$@" >"$logs/$name.log" 2>&1 &
    pids+=($!)
    names+=("$name")
}
join_background() {
    local failed=0 index
    for index in "${!pids[@]}"; do
        if wait "${pids[$index]}"; then
            printf '    %s: ok\n' "${names[$index]}"
        else
            printf -- '--- %s упал ---\n' "${names[$index]}"
            tail -n 60 "$logs/${names[$index]}.log"
            failed=1
        fi
    done
    pids=()
    names=()
    return "$failed"
}

# Зависимости ставятся только когда lock-файл новее установленного.
if [[ ! -d node_modules || package-lock.json -nt node_modules/.package-lock.json ]]; then
    npm ci
fi
npx --no-install svelte-kit sync

# Карта областей не должна отставать от исходников: иначе правка нового файла web/src
# молча не возьмёт ни одну спек, и регрессия всплывёт только в полном гейте.
node e2e/select-specs.mjs --completeness

# Сгенерированные типы сверяются с docs/api: в быстром режиме, только когда менялся контракт или сами типы.
api_changed=1
if ((fast)) && api_base=$(git merge-base HEAD "$base" 2>/dev/null); then
    api_changed=$({ git diff --name-only "$api_base" -- ../docs/api src/lib/api/generated scripts/gen-api.mjs; git ls-files --others --exclude-standard -- ../docs/api src/lib/api/generated; } | grep -c . || true)
fi
if ((api_changed)); then
    printf '\n==> сгенерированные типы API\n'
    node scripts/gen-api.mjs --check
fi

unit=(npx --no-install vitest run)
declare -a specs=() reasons=() projects=()
if ((fast)); then
    if merge_base=$(git merge-base HEAD "$base" 2>/dev/null); then
        unit=(npx --no-install vitest run --changed "$merge_base" --passWithNoTests)
        # Выбор спеков: изменённые спеки ∪ спеки областей, чьи исходники затронуты diff.
        # Карта и глобальные пути живут в e2e/areas.json; причину выбора печатаем перед прогоном.
        changed=$({ git diff --relative --name-only "$merge_base"; git ls-files --others --exclude-standard; } | sort -u)
        if [[ -n "$changed" ]]; then
            selection=$(printf '%s\n' "$changed" | node e2e/select-specs.mjs --select)
            if [[ -n "$selection" ]]; then
                while IFS=$'\t' read -r spec reason spec_projects; do
                    specs+=("$spec")
                    reasons+=("$reason")
                    projects+=("$spec_projects")
                done <<<"$selection"
            fi
        fi
    fi
fi

printf '\n==> статические проверки и vitest\n'
background check npm run check
background lint npm run lint
background tokens npm run lint:tokens
background unit "${unit[@]}"

if ((!fast)) || ((${#specs[@]})); then
    npx --no-install playwright install chromium firefox
    VITE_UI_TESTS=1 npm run build
    VITE_UI_TESTS=1 PWA_BUILD_DIR=build-update PWA_VERSION=ui-update-fixture npm run build
    built=1
fi
join_background

# Проверки, читающие build/, идут только после сборок: иначе сборка удаляет
# каталог под работающим vitest и тесты падают с ENOENT.
if ((built)); then
    printf '\n==> проверки собранных артефактов\n'
    npx --no-install vitest run --config vitest.build.config.ts
else
    printf '\n==> проверки собранных артефактов пропущены: сборка не запускалась\n'
fi

export PATH="${HOME}/.cargo/bin:${PATH}"
if ((fast)); then
    if ((${#specs[@]})); then
        printf '\n==> Playwright: выбранные спеки\n'
        printf 'Выбор по e2e/areas.json (изменённая спека / область / глобальный путь):\n'
        for index in "${!specs[@]}"; do
            printf '  %s — %s [%s]\n' "${specs[$index]}" "${reasons[$index]}" "${projects[$index]}"
        done
        # Глобальный путь дёргает весь набор, но только на phone: desktop — только для
        # изменённых спек и областей, чтобы быстрая проверка не распухала до гейта.
        # Спеку раскладки (desktop-layout) весь набор layout-* берёт отдельным проходом.
        # Самопроверки обвязки (контактный лист, провод fake-hub) в обычный прогон не входят:
        # изменившуюся из них запускает PW_HARNESS=1, иначе она молча пропустилась бы.
        plain_phone=() plain_both=() harness_phone=() harness_both=() layout_specs=()
        for index in "${!specs[@]}"; do
            spec=${specs[$index]}
            harness=0
            case "$spec" in
                *contact-sheet.spec.ts|*fake-hub-wire.spec.ts) harness=1 ;;
            esac
            # Раскладка рабочего стола живёт в проектах layout-*: phone и desktop эту спеку пропускают.
            if [[ "${projects[$index]}" == layout-* ]]; then
                layout_specs+=("$spec")
            elif [[ "${projects[$index]}" == "phone" ]]; then
                if ((harness)); then harness_phone+=("$spec"); else plain_phone+=("$spec"); fi
            else
                if ((harness)); then harness_both+=("$spec"); else plain_both+=("$spec"); fi
            fi
        done
        if ((${#plain_phone[@]})); then
            printf '\n-- глобальный выбор, phone: %d спек\n' "${#plain_phone[@]}"
            npx --no-install playwright test --project=phone "${plain_phone[@]}"
        fi
        if ((${#plain_both[@]})); then
            printf '\n-- изменённые спеки и области, phone и desktop: %d спек\n' "${#plain_both[@]}"
            npx --no-install playwright test --project=phone --project=desktop "${plain_both[@]}"
        fi
        if ((${#layout_specs[@]})); then
            printf '\n-- раскладка рабочего стола, layout-1024/1440/2560/3840: %d спек\n' "${#layout_specs[@]}"
            npx --no-install playwright test --project=layout-1024 --project=layout-1440 --project=layout-2560 --project=layout-3840 "${layout_specs[@]}"
        fi
        if ((${#harness_phone[@]})); then
            printf '\n-- обвязка, phone: %d спеки\n' "${#harness_phone[@]}"
            PW_HARNESS=1 npx --no-install playwright test --project=phone "${harness_phone[@]}"
        fi
        if ((${#harness_both[@]})); then
            printf '\n-- обвязка, phone и desktop: %d спеки\n' "${#harness_both[@]}"
            PW_HARNESS=1 npx --no-install playwright test --project=phone --project=desktop "${harness_both[@]}"
        fi
    fi
else
    printf '\n==> Playwright e2e/ui и e2e/parity\n'
    # С --stack сквозные сценарии идут и после падения UI: гейт перезапускает упавшие
    # тесты по отчётам и без отчёта стека не узнал бы его результат.
    npx --no-install playwright test e2e/ui e2e/parity || ui_status=$?
    ((stack)) || ((ui_status == 0)) || exit "$ui_status"
fi
if ((stack)); then
    printf '\n==> сквозные сценарии на настоящем хабе\n'
    npm run stack:build && npm run test:stack
fi
((ui_status == 0)) || exit "$ui_status"
printf 'web готов за %ds\n' $((SECONDS - started))
