#!/usr/bin/env bash
# Полный гейт Canterlot внутри контейнера canterlot-gate: тот же scripts/gate.sh, что
# идёт на VM (npm ci, check.sh --all --no-web, наборы выката, check-web.sh),
# со своей PostgreSQL на 127.0.0.1:55441.
#
#   scripts/gate/in-container.sh --src DIR --out DIR --sha SHA12
#
# Только внутри контейнера гейта: скрипт вычищает /tmp и отказывает без метки
# CANTERLOT_GATE_CONTAINER=1, которую ставят scripts/gate-remote.sh и scripts/gate/runner-gate.sh.
#
# Окружение готовит scripts/gate-remote.sh:
#   /src              рабочая копия на проверяемом коммите (сюда же пишется
#                     web/node_modules);
#   /src/target       кэш сборки Rust, смонтирован снаружи (стенд ищет бинарники
#                     в target/debug, поэтому target лежит внутри /src, а не в /cache);
#   /out              каталог результата: result.txt, state.tsv и логи шагов
#                     (npm-ci.log, check.log, deploy.log, web.log) из scripts/gate.sh,
#                     gate.log с его выводом;
#   /cache/cargo      CARGO_HOME;
#   /cache/npm        npm_config_cache.
#
# Формат result.txt и state.tsv — как у scripts/gate.sh: строка с кодами возврата и
# временами, затем сводка nextest и первые упавшие тесты.
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
# Ниже скрипт вычищает /tmp. В контейнере это смонтированный каталог кэша, а на машине — её
# /tmp с файлами состояния идущего выката и временными каталогами всех сессий. Метку ставят
# только scripts/gate-remote.sh и scripts/gate/runner-gate.sh через docker run -e; запуск
# руками по строке использования её не несёт и останавливается здесь.
[[ "${CANTERLOT_GATE_CONTAINER:-}" == 1 ]] || {
    echo "FATAL: скрипт запущен не в контейнере гейта (нет CANTERLOT_GATE_CONTAINER=1): он вычищает /tmp. Запускайте через scripts/gate-remote.sh или scripts/gate/runner-gate.sh" >&2
    exit 1
}
# Дерево с чужим HEAD гейт бы не заметил, поэтому sha проверяется — и тоже до чистки /tmp.
[[ "$(git -C "$src" rev-parse --short=12 HEAD)" == "$want_sha" ]] \
    || { echo "FATAL: HEAD в $src не $want_sha" >&2; exit 1; }

port=${CANTERLOT_TEST_PG_PORT:-55441}
export CANTERLOT_TEST_PG_PORT="$port"
export CARGO_HOME=${CARGO_HOME:-/usr/local/cargo}
export npm_config_cache=${npm_config_cache:-$HOME/.npm}

# Короткий TMPDIR: путь сокета исполнения — <runtime>/canterlot/<uuid>.sock, а
# canterlot-worker::supervisor::ipc отвергает путь длиннее 107 байт. Ровно /tmp,
# а не подкаталог: tests/it/exec.rs создаёт runtime через tempdir_in("/tmp"), а
# уборка в maintenance пускает её только внутри std::env::temp_dir().
export TMPDIR=/tmp
# /tmp — смонтированный каталог кэша, он переживает прогон: чистим его, чтобы
# тесты не увидели чужие временные файлы. Всё в нём создаём мы сами.
find /tmp -mindepth 1 -maxdepth 1 -exec rm -rf {} +

# uid приходит снаружи (--user), записи о нём в /etc/passwd контейнера нет.
# libnss-wrapper подставляет запись из файла: она нужна не только initdb и postgres,
# но и тестам носителя — sshd ищет пользователя по имени из `id -un`, а без записи
# остаётся числовой uid, и sshd отвечает «Invalid user». Env держится до конца гейта.
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

started=$SECONDS
phase() { printf '\n==> %s\n' "$1"; phase_started=$SECONDS; }
phase_done() { printf '    %s: %ds\n' "$1" $((SECONDS - phase_started)); }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

sha=$want_sha
printf 'гейт %s: src=%s out=%s, pg=127.0.0.1:%s\n' "$sha" "$src" "$out" "$port"
printf 'cargo: %s, node: %s, psql: %s\n' "$(cargo --version)" "$(node --version)" "$(psql --version)"

# --- тестовая PostgreSQL ---
# scripts/test-db.sh поднимает docker-контейнер и на VM, и в гейте.
# Внутри контейнера docker нет:
# база уже поднята здесь же, поэтому подменяем docker заглушкой, которая отвечает
# «контейнер есть» и проверяет готовность локального pg_isready.
phase "тестовая PostgreSQL"
pgbin=""
for candidate in /usr/lib/postgresql/*/bin; do
    [[ -x "$candidate/initdb" ]] && pgbin=$candidate
done
[[ -n "$pgbin" ]] || die "нет initdb в /usr/lib/postgresql/*/bin"
pgdata=/tmp/canterlot-gate-pg
# Запись для uid уже подставлена выше, поэтому initdb и postgres видят своего
# пользователя; окружение общее для всего гейта, включая тесты носителя с sshd.
(
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

# --- сам гейт: scripts/gate.sh ---
# Шаги не перечисляются здесь заново: контейнер гоняет тот же scripts/gate.sh, что и VM
# (npm ci, check.sh --all --no-web, наборы scripts/deploy/tests/run.sh, check-web.sh),
# без аттестации и без аренды (воркера в контейнере нет). Покрытие проверяет
# scripts/gate-coverage.sh. HEAD рабочей копии сверен с --sha в начале, до чистки /tmp.
phase "scripts/gate.sh --no-attest"
g=0
(cd "$src" && CANTERLOT_NO_LEASE=1 bash scripts/gate.sh --no-attest) >"$out/gate.log" 2>&1 || g=$?
phase_done "scripts/gate.sh"

# Состояние, сводка и логи шагов лежат в .superpowers/gate/<sha8> рабочей копии;
# выносим их в каталог результата, откуда их забирает scripts/gate-remote.sh (или носитель).
results="$src/.superpowers/gate/${sha:0:8}"
if [[ -d "$results" ]]; then
    cp -a "$results/." "$out/" || die "не удалось скопировать результаты из $results"
else
    printf 'FATAL: гейт не оставил %s\n' "$results" >&2
    g=1
fi
printf '\n'
[[ ! -f "$out/result.txt" ]] || cat "$out/result.txt"
printf '\nгейт %s за %ds, код scripts/gate.sh: %s\n' "${sha:0:8}" $((SECONDS - started)) "$g"

((g == 0)) || exit 1
