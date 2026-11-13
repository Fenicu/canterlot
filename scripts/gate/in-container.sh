#!/usr/bin/env bash
# Полный гейт Canterlot внутри контейнера canterlot-gate: прежние три шага
# /tmp/gate/run.sh (scripts/gate.sh на VM гоняет веб один раз), со своей
# PostgreSQL на 127.0.0.1:55441.
#
#   scripts/gate/in-container.sh --src DIR --out DIR --sha SHA12
#
# Окружение готовит scripts/gate-remote.sh:
#   /src              рабочая копия на проверяемом коммите (сюда же пишется
#                     web/node_modules);
#   /src/target       кэш сборки Rust, смонтирован снаружи (стенд ищет бинарники
#                     в target/debug, поэтому target лежит внутри /src, а не в /cache);
#   /out              каталог результата: build.log, check.log, web.log, result.txt;
#   /cache/cargo      CARGO_HOME;
#   /cache/npm        npm_config_cache;
#   /cache/playwright PLAYWRIGHT_BROWSERS_PATH, пустой кэш наполняется браузерами
#                     из образа (/opt/ms-playwright).
#
# Формат result.txt — как у /tmp/gate/run.sh: строка с кодами возврата и
# временами, затем сводка nextest, сводка Playwright и первые упавшие тесты.
set -euo pipefail

# Локаль — C.UTF-8 из образа, как на VM. LC_ALL=C ломает тесты супервизоров:
# запуск двойников CLI в них уходит в таймаут (проверено: 20 падений вместо 2).

src=""
out=""
want_sha=""
while (($#)); do
    case "$1" in
        --src) src=${2:?--src требует каталог}; shift ;;
        --out) out=${2:?--out требует каталог}; shift ;;
        --sha) want_sha=${2:?--sha требует sha12}; shift ;;
        *) echo "Использование: $0 --src DIR --out DIR --sha SHA12" >&2; exit 2 ;;
    esac
    shift
done
[[ -n "$src" && -n "$out" && -n "$want_sha" ]] \
    || { echo "Использование: $0 --src DIR --out DIR --sha SHA12" >&2; exit 2; }
[[ "$want_sha" =~ ^[0-9a-f]{12}$ ]] || { echo "FATAL: --sha не sha12: $want_sha" >&2; exit 1; }
[[ -d "$src" ]] || { echo "FATAL: нет каталога исходников $src" >&2; exit 1; }
[[ -d "$out" ]] || { echo "FATAL: нет каталога вывода $out" >&2; exit 1; }
[[ -z "$(ls -A "$out")" ]] || { echo "FATAL: каталог вывода $out не пуст" >&2; exit 1; }

port=${CANTERLOT_TEST_PG_PORT:-55441}
export CANTERLOT_TEST_PG_PORT="$port"
export CARGO_HOME=${CARGO_HOME:-/usr/local/cargo}
export npm_config_cache=${npm_config_cache:-$HOME/.npm}
export PLAYWRIGHT_BROWSERS_PATH=${PLAYWRIGHT_BROWSERS_PATH:-/opt/ms-playwright}

# Короткий TMPDIR: путь сокета исполнения — <runtime>/canterlot/<uuid>.sock, а
# canterlot-worker::supervisor::ipc отвергает путь длиннее 107 байт. Ровно /tmp,
# а не подкаталог: tests/it/exec.rs создаёт runtime через tempdir_in("/tmp"), а
# уборка в maintenance пускает её только внутри std::env::temp_dir().
export TMPDIR=/tmp
# /tmp — смонтированный каталог кэша, он переживает прогон: чистим его, чтобы
# тесты не увидели чужие временные файлы. Всё в нём создаём мы сами.
find /tmp -mindepth 1 -maxdepth 1 -exec rm -rf {} +

# Playwright берёт 50% от nproc, но nproc в контейнере — потоки хоста, а не квота
# --cpus. На 30 CPU квоты 18 воркеров с Chromium и Firefox не успевают довести
# страницу до [data-ready] за 15 с, и тесты падают по таймауту. Один воркер на
# ~4 CPU квоты: тот же набор проверок, но без гонки за процессор.
if [[ -z "${PW_WORKERS:-}" && -r /sys/fs/cgroup/cpu.max ]]; then
    read -r quota period </sys/fs/cgroup/cpu.max
    if [[ "$quota" != "max" && -n "${period:-}" ]]; then
        PW_WORKERS=$((quota / period / 4))
        ((PW_WORKERS < 4)) && PW_WORKERS=4
        export PW_WORKERS
    fi
fi
printf 'браузерных воркеров: %s\n' "${PW_WORKERS:-по умолчанию (50%% nproc)}"

started=$SECONDS
phase() { printf '\n==> %s\n' "$1"; phase_started=$SECONDS; }
phase_done() { printf '    %s: %ds\n' "$1" $((SECONDS - phase_started)); }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

sha=$want_sha
printf 'гейт %s: src=%s out=%s, pg=127.0.0.1:%s\n' "$sha" "$src" "$out" "$port"
printf 'cargo: %s, node: %s, psql: %s\n' "$(cargo --version)" "$(node --version)" "$(psql --version)"

# --- тестовая PostgreSQL ---
# scripts/test-db.sh поднимает docker-контейнер и на VM, и в стенде
# (e2e/stack/global-setup.ts зовёт его напрямую). Внутри контейнера docker нет:
# база уже поднята здесь же, поэтому подменяем docker заглушкой, которая отвечает
# «контейнер есть» и проверяет готовность локального pg_isready.
phase "тестовая PostgreSQL"
pgbin=""
for candidate in /usr/lib/postgresql/*/bin; do
    [[ -x "$candidate/initdb" ]] && pgbin=$candidate
done
[[ -n "$pgbin" ]] || die "нет initdb в /usr/lib/postgresql/*/bin"
pgdata=/tmp/canterlot-gate-pg
# uid приходит снаружи (--user), записи о нём в /etc/passwd контейнера нет, а
# initdb и postgres ищут свой uid через getpwuid. libnss-wrapper подставляет
# запись из файла; env выставлен только вокруг базы, тесты его не видят.
(
    if [[ -z "$(getent passwd "$(id -u)" || true)" ]]; then
        identity=$TMPDIR/identity
        mkdir -p "$identity"
        printf 'gate:x:%s:%s:Canterlot gate:%s:/bin/sh\nroot:x:0:0:root:/root:/bin/sh\n' \
            "$(id -u)" "$(id -g)" "$HOME" >"$identity/passwd"
        printf 'gate:x:%s:\nroot:x:0:\n' "$(id -g)" >"$identity/group"
        export NSS_WRAPPER_PASSWD="$identity/passwd"
        export NSS_WRAPPER_GROUP="$identity/group"
        export LD_PRELOAD=libnss_wrapper.so
    fi
    rm -rf "$pgdata"
    # Локаль как у postgres:18.6-trixie из scripts/test-db.sh: от неё зависит
    # порядок кириллицы в тестах пресетов (en_US, а не байтовый C).
    "$pgbin/initdb" -D "$pgdata" -U canterlot --auth=trust --no-sync -E UTF8 \
        --locale=en_US.UTF-8 --lc-collate=en_US.UTF-8 --lc-ctype=en_US.UTF-8 \
        >/tmp/initdb.log 2>&1 \
        || { tail -n 40 /tmp/initdb.log >&2; die "initdb не удался"; }
    {
        printf "listen_addresses = '127.0.0.1'\n"
        printf 'port = %s\n' "$port"
        # Как у postgres:*-trixie в scripts/test-db.sh (max_connections=500), плюс
        # отключённые fsync: данные лежат в /tmp контейнера, терять их не жалко.
        printf 'max_connections = 500\n'
        printf 'fsync = off\n'
        printf 'synchronous_commit = off\n'
        printf 'full_page_writes = off\n'
        printf "unix_socket_directories = '/tmp'\n"
    } >>"$pgdata/postgresql.conf"
    "$pgbin/pg_ctl" -D "$pgdata" -l /tmp/postgres.log -w -t 60 start >/tmp/pgctl.log 2>&1 \
        || { tail -n 40 /tmp/postgres.log >&2; die "PostgreSQL не запустилась"; }
    "$pgbin/createdb" -h 127.0.0.1 -p "$port" -U canterlot canterlot \
        || die "не удалось создать базу canterlot"
    "$pgbin/psql" -h 127.0.0.1 -p "$port" -U canterlot -d canterlot -q \
        -c "ALTER ROLE canterlot PASSWORD 'canterlot'" >/dev/null
)

shim=/tmp/gate-bin
mkdir -p "$shim"
cat >"$shim/docker" <<EOF
#!/bin/sh
# Заглушка docker для scripts/test-db.sh: база уже поднята в этом контейнере.
case "\$1" in
    container|start|stop|rm|inspect) exit 0 ;;
    exec) exec pg_isready -h 127.0.0.1 -p $port -U canterlot -d canterlot ;;
    *) echo "docker-заглушка гейта: неизвестная команда: \$*" >&2; exit 1 ;;
esac
EOF
chmod 0755 "$shim/docker"
export PATH="$shim:$PATH"
pg_isready -h 127.0.0.1 -p "$port" -U canterlot -d canterlot >/dev/null || die "pg_isready не видит базу"
phase_done "PostgreSQL"

# --- браузеры Playwright ---
phase "браузеры Playwright"
if [[ -z "$(ls -A "$PLAYWRIGHT_BROWSERS_PATH" 2>/dev/null)" ]]; then
    mkdir -p "$PLAYWRIGHT_BROWSERS_PATH"
    cp -a /opt/ms-playwright/. "$PLAYWRIGHT_BROWSERS_PATH/" || die "не удалось наполнить кэш браузеров"
fi
printf '    в кэше: %s\n' "$(find "$PLAYWRIGHT_BROWSERS_PATH" -mindepth 1 -maxdepth 1 -printf '%f ')"
phase_done "браузеры"

# --- шаг 1: зависимости и сборка web ---
phase "npm ci"
(cd "$src/web" && npm ci --no-audit --no-fund) >"$out/build.log" 2>&1 \
    || { tail -n 60 "$out/build.log" >&2; die "npm ci не удался (см. $out/build.log)"; }
phase_done "npm ci"

phase "сборка web"
b=0
(cd "$src/web" && VITE_UI_TESTS=1 npm run build) >>"$out/build.log" 2>&1 || b=$?
# npm ci и сборка оставляют сотни мегабайт грязных страниц. На медленном диске
# их отложенная запись совпадает с линтом и тестами, и запуск процессов внутри
# тестов растягивается до таймаутов; сбрасываем запись до check.sh.
sync
phase_done "сборка web"

# --- шаг 2: scripts/check.sh --all ---
phase "scripts/check.sh --all"
s=$SECONDS
c=0
bash "$src/scripts/check.sh" --all >"$out/check.log" 2>&1 || c=$?
t1=$((SECONDS - s))
phase_done "scripts/check.sh --all"

# --- шаг 3: scripts/check-web.sh --stack ---
phase "scripts/check-web.sh --stack"
s=$SECONDS
w=0
bash "$src/scripts/check-web.sh" --stack >"$out/web.log" 2>&1 || w=$?
t2=$((SECONDS - s))
phase_done "scripts/check-web.sh --stack"

{
    printf '%s build=%s check=%s %ss web=%s %ss\n' "${sha:0:8}" "$b" "$c" "$t1" "$w" "$t2"
    grep -E 'Summary \[' "$out/check.log" | tail -1 || true
    grep -E '^\s+[0-9]+ (passed|failed|flaky|did not run)' "$out/web.log" || true
    grep -E '^\s+[0-9]+\) ' "$out/web.log" | sort -u | head || true
} >"$out/result.txt"
printf '\n'
cat "$out/result.txt"
printf '\nгейт %s за %ds, коды: build=%s check=%s web=%s\n' "${sha:0:8}" $((SECONDS - started)) "$b" "$c" "$w"

((b == 0 && c == 0 && w == 0)) || exit 1
