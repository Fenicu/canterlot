#!/usr/bin/env bash
# Карантин нестабильных тестов: единственные места, где разрешён повтор. Каждая запись помечена
# QUARANTINE и причиной, остальные тесты не перезапускаются.
cd "$(dirname "${BASH_SOURCE[0]}")/.."
echo "quarantine (retries allowed):"
found=$(grep -rn "QUARANTINE" .config web/e2e web/tests 2>/dev/null | sed 's/^/  /' || true)
printf '%s\n' "${found:-  none}"
