#!/usr/bin/env bash
# Самотест уборки каталогов scratch_dir(): всё происходит во временном каталоге,
# настоящий /dev/shm не трогается, чужие процессы не убиваются.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

tmp=$(mktemp -d "${TMPDIR:-/tmp}/canterlot-reap-test.XXXXXX")
pids=""
cleanup() {
    local pid cmd
    exec 3<&- 2>/dev/null || true
    for pid in $pids; do
        cmd=$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null) || continue
        [[ "$cmd" == canterlot-reap-holder* ]] || continue
        kill "$pid" 2>/dev/null || true
    done
    rm -rf -- "$tmp"
}
trap cleanup EXIT

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

scratch="$tmp/shm"
root="$tmp/root"
mkdir -p "$scratch" "$root"

# Старый свободный каталог: должен уйти вместе со вложенным содержимым.
old="$scratch/canterlot-test-old"
mkdir -p "$old/nested"
: >"$old/nested/file"
touch -d '7 hours ago' "$old"

# Молодой каталог: остаётся.
young="$scratch/canterlot-test-young"
mkdir -p "$young"
: >"$young/marker"

# Старый каталог с живым рабочим каталогом процесса: остаётся.
cwd_dir="$scratch/canterlot-test-cwd"
mkdir -p "$cwd_dir"
: >"$cwd_dir/marker"
touch -d '7 hours ago' "$cwd_dir"
( cd "$cwd_dir" && : >"$tmp/cwd-ready" && exec -a canterlot-reap-holder sleep 300 ) &
cwd_pid=$!
pids+="$cwd_pid "
for _ in $(seq 1 100); do
    [[ -e "$tmp/cwd-ready" ]] && break
    sleep 0.01
done
[[ -e "$tmp/cwd-ready" ]] || fail "процесс с рабочим каталогом внутри теста не стартовал"

# Старый каталог с открытым файлом: остаётся.
fd_dir="$scratch/canterlot-test-fd"
mkdir -p "$fd_dir"
: >"$fd_dir/marker"
: >"$fd_dir/file"
touch -d '7 hours ago' "$fd_dir"
exec 3<"$fd_dir/file"

# Символьная ссылка не разыменовывается: цель остаётся нетронутой.
outside="$tmp/outside"
mkdir -p "$outside"
: >"$outside/marker"
ln -s "$outside" "$scratch/canterlot-test-link"
touch -h -d '7 hours ago' "$scratch/canterlot-test-link"
ln -s "$outside" "$old/link"
touch -d '7 hours ago' "$old"

# Префикс должен совпадать целиком.
other="$scratch/canterlot-testing-old"
mkdir -p "$other"
: >"$other/marker"
touch -d '7 hours ago' "$other"

REAP_SCRATCH_ROOT="$scratch" bash scripts/reap-stray-tests.sh "$root" >"$tmp/out"
cat "$tmp/out"

[[ ! -e "$old" ]] || fail "старый свободный каталог не удалён"
[[ -f "$young/marker" ]] || fail "молодой каталог удалён"
[[ -f "$cwd_dir/marker" ]] || fail "каталог с живым рабочим каталогом удалён"
[[ -f "$fd_dir/marker" ]] || fail "каталог с открытым файлом удалён"
[[ -f "$outside/marker" ]] || fail "цель символьной ссылки задет"
[[ -L "$scratch/canterlot-test-link" ]] || fail "символьная ссылка удалена"
[[ -f "$other/marker" ]] || fail "каталог с другим префиксом удалён"
grep -qF "reaped stray test dir=$old" "$tmp/out" || fail "нет отчёта об удалении"
printf 'reap stray tests: ok\n'
