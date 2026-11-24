#!/usr/bin/env bash
# Полный гейт Canterlot на раннере: контейнерный путь scripts/gate-remote.sh внутри
# рабочей копии, которую носитель подготовил для задачи (scripts/gate.sh --runner).
#
#   scripts/gate/runner-gate.sh <полный sha>
#
# Запускается на раннере из рабочей копии <корень>/jobs/<домен>/<задача> ровно на том
# коммите, который назвал scripts/gate.sh. Dockerfile и in-container.sh берутся из ЭТОЙ
# рабочей копии, с VM не копируется ничего. Скрипт:
#   1. проверяет, что HEAD равен sha, и находит корень раннера по пути рабочей копии
#      (без <корень>/worker.toml это не раннер, и скрипт отказывает);
#   2. собирает образ canterlot-gate:<hash Dockerfile + rust-toolchain.toml> или
#      переиспользует готовый;
#   3. запускает контейнер canterlot-gate-<sha12>-<случайное> с --cpus и --memory от
#      nproc и free (хосту остаются 2 ядра и 4 ГБ) и кэшами <корень>/cache/{cargo,
#      target,npm,tmp}; по TERM и INT контейнер убивается по имени;
#   4. кладёт result.txt, state.tsv и логи в ./.gate-out (его собирает носитель) и
#      дописывает в state.tsv строку «runner<TAB>имя»: имя из CANTERLOT_RUNNER_NAME,
#      иначе имя хоста.
#
# Код выхода — код гейта. Шаги гейта те же, что у scripts/gate.sh: в контейнере
# scripts/gate/in-container.sh зовёт сам scripts/gate.sh.
#   CANTERLOT_DOCKER        команда docker (по умолчанию docker; нужна тестам);
#   CANTERLOT_RUNNER_NAME   имя раннера для state.tsv.
set -euo pipefail
export LC_ALL=C

die() { printf 'runner-gate: %s\n' "$*" >&2; exit 1; }
log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }

if (($# != 1)); then
    echo 'Использование: scripts/gate/runner-gate.sh <полный sha>' >&2
    exit 2
fi
sha=$1
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || die "аргумент не полный sha: $sha"
sha12=${sha:0:12}

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
wt=$(pwd)
docker_cmd=${CANTERLOT_DOCKER:-docker}

head=$(git rev-parse HEAD) || die 'не git-копия'
[[ "$head" == "$sha" ]] || die "HEAD $head не совпадает с запрошенным $sha"

# Корень раннера: <корень>/jobs/<домен>/<задача>. Без worker.toml в корне это не рабочая
# копия раннера (например, чей-то обычный checkout), и кэши класть некуда.
jobs_dir=$(dirname "$(dirname "$wt")")
root=$(dirname "$jobs_dir")
[[ "$(basename "$jobs_dir")" == jobs && "$root" == /* && "$root" != / ]] \
    || die "корень раннера не найден: $wt не лежит в <корень>/jobs/<домен>/<задача>"
[[ -f "$root/worker.toml" ]] || die "нет $root/worker.toml: это не раннер"

dockerfile="$wt/docker/gate/Dockerfile"
in_container="$wt/scripts/gate/in-container.sh"
[[ -f "$dockerfile" && -f "$in_container" ]] || die 'в рабочей копии нет docker/gate/Dockerfile или scripts/gate/in-container.sh'
rust_version=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
[[ -n "$rust_version" ]] || die 'в rust-toolchain.toml нет channel'
image_tag=$({ cat "$dockerfile" rust-toolchain.toml; } | sha256sum | cut -c1-12)
image="canterlot-gate:$image_tag"

# Ограничения: хосту остаются 2 ядра и 4 ГБ. pids 8192, init, shm и SYS_PTRACE — по тем же
# причинам, что в scripts/gate-remote.sh (супервизоры, strace в тестах).
cpus=$(($(nproc) - 2))
((cpus >= 1)) || cpus=1
mem_mb=$(($(free -m | awk '/^Mem:/{print $2}') - 4096))
((mem_mb >= 2048)) || mem_mb=2048
cache="$root/cache"
mkdir -p "$cache/cargo" "$cache/target" "$cache/npm" "$cache/tmp"
out="$wt/.gate-out"
mkdir -p "$out"
[[ -z "$(ls -A "$out")" ]] || die "каталог $out не пуст"

log "коммит $sha12, раннер $root, образ $image (Rust $rust_version)"
if "$docker_cmd" image inspect "$image" >/dev/null 2>&1; then
    log 'образ уже есть'
else
    "$docker_cmd" build -t "$image" \
        --build-arg "RUST_VERSION=$rust_version" \
        -f "$dockerfile" "$wt/docker/gate" || die 'не удалось собрать образ'
fi

name="canterlot-gate-$sha12-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
run_pid=""
# Прерывание (задачу отменили или вышел таймаут): контейнер не должен пережить скрипт.
on_signal() {
    local code=$1
    trap '' INT TERM
    log "сигнал: убираю контейнер $name"
    "$docker_cmd" kill "$name" >/dev/null 2>&1 || true
    [[ -z "$run_pid" ]] || wait "$run_pid" 2>/dev/null || true
    exit "$code"
}
trap 'on_signal 130' INT
trap 'on_signal 143' TERM

# Рабочая копия — связанный worktree: её .git — файл, указывающий в общий каталог git
# (<корень>/repos/<домен>/<ключ>.git) вне рабочей копии. Контейнер видит только /src, и без
# общего каталога по его собственному пути `git -C /src rev-parse` падает до первого шага.
# git в гейте только читает (rev-parse, status, diff, merge-base), поэтому только чтение.
git_common=$(git rev-parse --path-format=absolute --git-common-dir) || die 'не удалось найти общий каталог git'
[[ "$git_common" == /* && -d "$git_common" ]] || die "общий каталог git не найден: $git_common"

run_args=(
    --rm --init --name "$name"
    --user "$(id -u):$(id -g)"
    --cpus "$cpus" --memory "${mem_mb}m" --pids-limit 8192
    --shm-size=2g
    --cap-add SYS_PTRACE
    -v "$cache/tmp:/tmp"
    -v "$wt:/src"
    -v "$git_common:$git_common:ro"
    -v "$cache/cargo:/cache/cargo"
    -v "$cache/target:/src/target"
    -v "$cache/npm:/cache/npm"
    -v "$out:/out"
    -v "$in_container:/build/in-container.sh:ro"
    -e CARGO_HOME=/cache/cargo
    -e npm_config_cache=/cache/npm
    -w /src
)
log "контейнер $name: --cpus $cpus --memory ${mem_mb}m"
gate_code=0
"$docker_cmd" run "${run_args[@]}" "$image" \
    bash /build/in-container.sh --src /src --out /out --sha "$sha12" &
run_pid=$!
wait "$run_pid" || gate_code=$?
run_pid=""
log "гейт завершился с кодом $gate_code"

if [[ -f "$out/state.tsv" ]]; then
    printf 'runner\t%s\n' "${CANTERLOT_RUNNER_NAME:-${HOSTNAME%%.*}}" >>"$out/state.tsv"
fi
((gate_code == 0)) || exit "$gate_code"
[[ -f "$out/result.txt" && -f "$out/state.tsv" ]] || die 'гейт вышел с 0, но не оставил result.txt и state.tsv в .gate-out'
