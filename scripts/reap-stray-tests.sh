#!/usr/bin/env bash
# Супервизоры намеренно переживают воркера, поэтому убитый тестовый прогон оставляет их навсегда.
# Снимает только тестовые: бинарник из target этой копии, каталог /tmp/.tmp*, возраст от 30 минут.
# Каталоги scratch_dir() из /dev/shm убирает отдельно: tempfile снимает их при обычном
# завершении теста, а убитый прогон оставляет их лежать в памяти до перезагрузки.
set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
min_age="${REAP_MIN_AGE_SECONDS:-1800}"
# REAP_SCRATCH_ROOT — шов для самотестов: настоящий /dev/shm трогать нельзя,
# пока другие сессии гоняют свои тесты.
scratch_root="${REAP_SCRATCH_ROOT:-/dev/shm}"
dir_min_age="${REAP_DIR_MIN_AGE_SECONDS:-21600}"

while read -r pid age exe command flag dir _; do
    [[ "$exe" == "$root"/target/*/canterlot-worker ]] || continue
    [[ "$command" == supervise && "$flag" == --exec-dir && "$dir" == /tmp/.tmp* ]] || continue
    ((age >= min_age)) || continue
    pkill -KILL -P "$pid" || true
    kill -KILL "$pid" 2>/dev/null || true
    echo "reaped stray supervisor pid=$pid age=${age}s"
done < <(ps -eo pid=,etimes=,args=)

# Занятым считается каталог, в котором у живого процесса есть рабочий каталог или
# открытый файл: читаем ссылки /proc, а не держим за себя блокировку. Символьные ссылки не
# разыменовываем ни при поиске, ни при удалении: find без -L даёт только каталоги.
dir_in_use() {
    local dir=$1 link target
    for link in /proc/[0-9]*/cwd /proc/[0-9]*/fd/*; do
        target=$(readlink "$link" 2>/dev/null) || continue
        if [[ "$target" == "$dir" || "$target" == "$dir"/* ]]; then
            return 0
        fi
    done
    return 1
}

now=$(date +%s)
while IFS= read -r dir; do
    [[ -d "$dir" && ! -L "$dir" ]] || continue
    modified=$(stat -c %Y -- "$dir" 2>/dev/null) || continue
    age=$((now - modified))
    ((age >= dir_min_age)) || continue
    if dir_in_use "$dir"; then
        continue
    fi
    rm -rf -- "$dir"
    echo "reaped stray test dir=$dir age=${age}s"
done < <(find "$scratch_root" -maxdepth 1 -type d -name 'canterlot-test-*' -print 2>/dev/null)
