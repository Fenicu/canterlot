#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../web"
if [[ $(df --output=avail -BG / | tail -1 | tr -dc '0-9') -lt 5 ]]; then
  echo 'BLOCKED: менее 5 ГБ свободного места' >&2
  exit 1
fi
if [[ "${1:-}" != "" && "${1:-}" != "--stack" ]]; then
  echo 'Использование: bash scripts/check-web.sh [--stack]' >&2
  exit 2
fi
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/canterlot-playwright}"
export TOKIO_WORKER_THREADS="${TOKIO_WORKER_THREADS:-4}"
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-4}"
export MOZ_TASKCONTROLLER_THREADCOUNT="${MOZ_TASKCONTROLLER_THREADCOUNT:-4}"
export GSETTINGS_BACKEND="${GSETTINGS_BACKEND:-memory}"
# Каждый прогон берёт свой свободный порт: параллельные проверки из соседних рабочих копий не делят 4173.
export PORT="${PORT:-$(node -e "const s=require('net').createServer();s.listen(0,'127.0.0.1',()=>{console.log(s.address().port);s.close()})")}"
npm ci
npx --no-install playwright install chromium firefox
npm run check
npm run lint
npm run lint:tokens
VITE_UI_TESTS=1 npm run build
VITE_UI_TESTS=1 PWA_BUILD_DIR=build-update PWA_VERSION=ui-update-fixture npm run build
npm run test:unit
npx --no-install playwright test e2e/ui
if [[ "${1:-}" == "--stack" ]]; then npm run test:stack; fi
