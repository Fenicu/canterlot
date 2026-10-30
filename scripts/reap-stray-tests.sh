#!/usr/bin/env bash
# Супервизоры намеренно переживают воркера, поэтому убитый тестовый прогон оставляет их навсегда.
# Снимает только тестовые: бинарник из target этой копии, каталог /tmp/.tmp*, возраст от 30 минут.
set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
min_age="${REAP_MIN_AGE_SECONDS:-1800}"

while read -r pid age exe command flag dir _; do
    [[ "$exe" == "$root"/target/*/canterlot-worker ]] || continue
    [[ "$command" == supervise && "$flag" == --exec-dir && "$dir" == /tmp/.tmp* ]] || continue
    ((age >= min_age)) || continue
    pkill -KILL -P "$pid" || true
    kill -KILL "$pid" 2>/dev/null || true
    echo "reaped stray supervisor pid=$pid age=${age}s"
done < <(ps -eo pid=,etimes=,args=)
