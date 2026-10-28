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
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-/tmp/canterlot-playwright}"
export TOKIO_WORKER_THREADS="${TOKIO_WORKER_THREADS:-2}"
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-2}"
export MOZ_TASKCONTROLLER_THREADCOUNT="${MOZ_TASKCONTROLLER_THREADCOUNT:-2}"
export GSETTINGS_BACKEND="${GSETTINGS_BACKEND:-memory}"
npm ci
npm run check
npm run lint
npm run lint:tokens
npm run test:unit
VITE_UI_TESTS=1 npm run build
npx --no-install playwright test e2e/ui
if [[ "${1:-}" == "--stack" ]]; then npm run test:stack; fi
