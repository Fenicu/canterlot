#!/usr/bin/env bash
# Подмена scripts/check-web.sh для scripts/gate/test.sh: веб-шаг падает не по тестам
# (браузерные тесты удалены 22.11.2026), идентичностей упавших тестов нет.
set -euo pipefail
echo "check-web $*" >>"$GATE_FAKE_CALLS"
exit 1
