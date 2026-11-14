#!/usr/bin/env bash
# Проверка веба.
#   scripts/check-web.sh                 сверка сгенерированных типов API, svelte-check, eslint, токены, vitest, проверки собранных артефактов, Playwright e2e/ui и e2e/parity
#   scripts/check-web.sh --fast          то же без Playwright, vitest только по изменённым файлам;
#                                        проверки собранных артефактов — только если была сборка
#                                        изменённые спеки e2e запускаются на phone
#   scripts/check-web.sh --fast --base REF  сравнивать с REF (по умолчанию main)
#   scripts/check-web.sh --stack         полная плюс сквозные сценарии на настоящем хабе (перед выкаткой)
# Прогон встаёт в очередь тяжёлых задач воркера: --stack берёт аренду gate, остальные
# режимы — build. Число воркеров Playwright задаёт аренда через PW_WORKERS.
set -euo pipefail

cd "$(dirname "$0")/../web"
original_args=("$@")

fast=0
stack=0
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
specs=()
if ((fast)); then
    if merge_base=$(git merge-base HEAD "$base" 2>/dev/null); then
        unit=(npx --no-install vitest run --changed "$merge_base" --passWithNoTests)
        mapfile -t specs < <({ git diff --relative --name-only "$merge_base" -- e2e/ui e2e/parity; git ls-files --others --exclude-standard -- e2e/ui e2e/parity; } | grep -E '\.spec\.ts$' | sort -u)
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
        printf '\n==> Playwright: изменённые спеки на phone\n'
        npx --no-install playwright test --project=phone "${specs[@]}"
    fi
else
    printf '\n==> Playwright e2e/ui и e2e/parity\n'
    npx --no-install playwright test e2e/ui e2e/parity
fi
if ((stack)); then
    printf '\n==> сквозные сценарии на настоящем хабе\n'
    npm run stack:build && npm run test:stack
fi
printf 'web готов за %ds\n' $((SECONDS - started))
