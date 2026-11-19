#!/usr/bin/env bash
# Подмена scripts/check-web.sh для scripts/gate/test.sh: полный прогон веба с двумя
# упавшими тестами в разных проектах Playwright.
set -euo pipefail
echo "check-web $*" >>"$GATE_FAKE_CALLS"
mkdir -p "${CANTERLOT_PW_JSON_REPORT%/*}"
printf '%s\n' '{"suites":[{"specs":[{"file":"ui/x.spec.ts","line":42,"title":"spec","tests":[{"projectName":"phone","title":"title","results":[{"status":"failed"}]}]},{"file":"ui/w.spec.ts","line":5,"title":"spec","tests":[{"projectName":"desktop","title":"title","results":[{"status":"failed"}]}]}]}]}' \
    >"${CANTERLOT_PW_JSON_REPORT}.json"
exit 1
