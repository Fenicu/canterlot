#!/usr/bin/env bash
# Полный гейт Canterlot (scripts/check.sh --all + scripts/check-web.sh --stack)
# в контейнере Debian 13 на сборочной машине: те же шаги, что /tmp/gate/run.sh на
# этой VM, но изолированно от хоста и на быстром железе.
#
#   scripts/gate-remote.sh [REV]            REV на CANTERLOT_BUILD_HOST (mainpc-build)
#   scripts/gate-remote.sh [REV] --local    то же на местном docker (для проверки)
#
# REV — любой коммит (по умолчанию HEAD этой рабочей копии). Скрипт:
#   1. отправляет коммит в bare-репозиторий ~/canterlot.git как refs/gate/<sha12>
#      и создаёт там отсоединённую рабочую копию ~/gate/src-<sha12>;
#   2. собирает (или переиспользует) образ canterlot-gate:<hash Dockerfile +
#      rust-toolchain.toml + версия Playwright из web/package-lock.json>;
#   3. запускает контейнер от uid хоста с кэшами
#      ~/gate/cache/{cargo,target,npm,playwright};
#   4. переносит result.txt и логи в ${CANTERLOT_GATE_OUT:-/tmp/gate-remote}/<sha12>;
#   5. убирает рабочую копию (git worktree remove --force) и refs/gate/<sha12>.
#
# Код возврата — код гейта. На хосте ничего не удаляется через rm: только
# worktree remove и update-ref -d.
set -euo pipefail
export LC_ALL=C

usage() {
    echo "Использование: scripts/gate-remote.sh [REV] [--local]" >&2
}

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
dockerfile="$repo/docker/gate/Dockerfile"
in_container="$repo/scripts/gate/in-container.sh"
rev=HEAD
mode=remote
while (($#)); do
    case "$1" in
        --local) mode=local ;;
        -h | --help) usage; exit 0 ;;
        --*) usage; exit 2 ;;
        *) rev=$1 ;;
    esac
    shift
done

started=$SECONDS
phase() { printf '\n==> %s\n' "$1"; phase_started=$SECONDS; }
phase_done() { printf '    %s: %ds\n' "$1" $((SECONDS - phase_started)); }
log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
die() { printf '[%s] FATAL: %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; exit 1; }
q() { printf '%q' "$1"; }

phase "проверка предусловий"
[[ -f "$dockerfile" && -f "$in_container" ]] \
    || die "нет docker/gate/Dockerfile или scripts/gate/in-container.sh"
git rev-parse --verify --quiet "${rev}^{commit}" >/dev/null \
    || die "нет такого коммита: $rev"
commit=$(git rev-parse --verify "${rev}^{commit}")
sha12=$(git rev-parse --short=12 "$commit")
out_root=${CANTERLOT_GATE_OUT:-/tmp/gate-remote}
out="$out_root/$sha12"
[[ ! -e "$out" ]] \
    || die "каталог $out уже существует: результат этого коммита уже снят (удалите его, если нужен повтор)"
docker_cmd=${CANTERLOT_DOCKER:-docker}
if [[ "$mode" == local ]]; then
    command -v "$docker_cmd" >/dev/null || die "нет docker ($docker_cmd): --local гоняет гейт на этой машине"
    uid=$(id -u)
    gid=$(id -g)
else
    host=${CANTERLOT_BUILD_HOST:-mainpc-build}
    command -v ssh >/dev/null || die "нет ssh"
fi
rust_version=$(git show "$commit:rust-toolchain.toml" | sed -n 's/^channel = "\(.*\)"/\1/p')
[[ -n "$rust_version" ]] || die "в rust-toolchain.toml коммита $sha12 нет channel"
playwright_version=$(git show "$commit:web/package-lock.json" \
    | jq -r '.packages["node_modules/playwright-core"].version // empty')
[[ -n "$playwright_version" ]] || die "в web/package-lock.json коммита $sha12 нет playwright-core"
image_tag=$({ cat "$dockerfile"; git show "$commit:rust-toolchain.toml"; printf '%s\n' "$playwright_version"; } \
    | sha256sum | cut -c1-12)
image="canterlot-gate:$image_tag"
log "коммит $sha12 ($rev), режим $mode, образ $image (Rust $rust_version, Playwright $playwright_version)"
phase_done "предусловия"

# --- удалённая машина ---
remote() { ssh -n -o BatchMode=yes -o ConnectTimeout=15 "$host" "$1" </dev/null; }
remote_home=""
remote_repo=""
remote_base=""
remote_tools=""
src=""
script_path="$in_container"
cache_root=""
worktree_parent=""
cleaned=0

# Уборка повторяется и на выходе по ошибке: рабочая копия и ref не должны
# пережить упавший прогон. На хосте только git, без rm.
cleanup() {
    if ((cleaned)); then return 0; fi
    cleaned=1
    if [[ -z "$src" ]]; then return 0; fi
    if [[ "$mode" == remote ]]; then
        # shellcheck disable=SC2016 # пути раскрывает удалённый shell
        cmd=$(printf 'if [ -e %q ]; then git --git-dir %q worktree remove --force %q; fi; git --git-dir %q worktree prune; git --git-dir %q update-ref -d %q 2>/dev/null || true' \
            "$src" "$remote_repo" "$src" "$remote_repo" "$remote_repo" "refs/gate/$sha12")
        if remote "$cmd"; then
            log "рабочая копия $src и refs/gate/$sha12 убраны на $host"
        else
            log "ВНИМАНИЕ: не удалось убрать $src или refs/gate/$sha12 на $host; уберите вручную"
        fi
    else
        if [[ -n "$src" && -e "$src" ]]; then
            if git -C "$repo" worktree remove --force "$src"; then
                if [[ -n "$worktree_parent" ]]; then rmdir "$worktree_parent" 2>/dev/null || true; fi
                log "рабочая копия $src удалена"
            else
                log "ВНИМАНИЕ: не удалось удалить $src; уберите её: git -C $repo worktree remove --force $src"
            fi
        fi
    fi
}
trap cleanup EXIT

if [[ "$mode" == remote ]]; then
    phase "сборочная машина $host"
    # shellcheck disable=SC2016 # $HOME раскрывает удалённый shell
    remote_home=$(remote 'printf %s "$HOME"') || die "ssh $host не отвечает"
    [[ "$remote_home" == /* && "$remote_home" != *$'\n'* ]] || die "странный HOME на $host: '$remote_home'"
    uid=$(remote 'id -u') || die "не удалось узнать uid на $host"
    gid=$(remote 'id -g') || die "не удалось узнать gid на $host"
    remote_repo="$remote_home/canterlot.git"
    remote_base="$remote_home/gate"
    remote_tools="$remote_base/tools"
    src="$remote_base/src-$sha12"
    cache_root="$remote_base/cache"
    remote 'command -v git >/dev/null && command -v docker >/dev/null && command -v tar >/dev/null' \
        || die "на $host нет git, docker или tar"
    [[ "$(remote "git --git-dir $(q "$remote_repo") rev-parse --is-bare-repository 2>/dev/null")" == "true" ]] \
        || die "на $host нет bare-репозитория $remote_repo"
    remote "docker info >/dev/null 2>&1" || die "docker на $host не работает"
    phase_done "сборочная машина"

    phase "отправка коммита"
    git push --quiet "$host:canterlot.git" "$commit:refs/gate/$sha12" \
        || die "не удалось отправить $commit в $remote_repo"
    log "коммит отправлен как refs/gate/$sha12"
    phase_done "отправка коммита"
else
    cache_root=${CANTERLOT_GATE_CACHE:-$HOME/.cache/canterlot-gate}
fi

# --- чистая рабочая копия ---
phase "рабочая копия"
if [[ "$mode" == remote ]]; then
    # shellcheck disable=SC2016 # пути раскрывает удалённый shell
    cmd=$(printf 'set -e; wt=%q; if [ -e "$wt" ]; then git --git-dir %q worktree remove --force "$wt"; fi; git --git-dir %q worktree prune; mkdir -p %q; git --git-dir %q worktree add --detach "$wt" %q; mkdir -p "$wt/.gate-out"' \
        "$src" "$remote_repo" "$remote_repo" "$remote_base" "$remote_repo" "$commit")
    remote "$cmd" || die "не удалось создать рабочую копию $src на $host"
    head=$(remote "git -C $(q "$src") rev-parse --short=12 HEAD 2>/dev/null || true")
else
    common=$(cd "$repo" && cd "$(git rev-parse --git-common-dir)" && pwd)
    worktree_parent="$common/canterlot-gate"
    src="$worktree_parent/src-$sha12"
    mkdir -p "$worktree_parent"
    if [[ -e "$src" ]]; then
        git -C "$repo" worktree remove --force "$src" || die "не удалось убрать старую рабочую копию $src"
    fi
    git -C "$repo" worktree prune
    git -C "$repo" worktree add --detach "$src" "$commit" >/dev/null
    mkdir -p "$src/.gate-out"
    head=$(git -C "$src" rev-parse --short=12 HEAD)
fi
[[ "$head" == "$sha12" ]] || die "рабочая копия $src не на $sha12"
log "источник: $src"
phase_done "рабочая копия"

# --- образ ---
phase "образ гейта"
if [[ "$mode" == remote ]]; then
    remote "mkdir -p $(q "$remote_tools") $(q "$cache_root/cargo") $(q "$cache_root/target") $(q "$cache_root/npm") $(q "$cache_root/playwright") $(q "$cache_root/tmp")" \
        || die "не удалось создать $remote_tools и кэши"
    # Dockerfile и in-container.sh берутся из ЭТОЙ рабочей копии: коммит может быть
    # старше самого гейта.
    ssh -o BatchMode=yes -o ConnectTimeout=15 "$host" "cat > $(q "$remote_tools/Dockerfile")" <"$dockerfile" \
        || die "не удалось скопировать Dockerfile на $host"
    ssh -o BatchMode=yes -o ConnectTimeout=15 "$host" "cat > $(q "$remote_tools/in-container.sh")" <"$in_container" \
        || die "не удалось скопировать in-container.sh на $host"
    script_path="$remote_tools/in-container.sh"
    if remote "docker image inspect $(q "$image") >/dev/null 2>&1"; then
        log "образ уже есть на $host"
    else
        cmd=$(printf 'docker build -t %q --build-arg %q --build-arg %q -f %q %q' \
            "$image" "RUST_VERSION=$rust_version" "PLAYWRIGHT_VERSION=$playwright_version" \
            "$remote_tools/Dockerfile" "$remote_tools")
        remote "$cmd" || die "не удалось собрать образ на $host"
    fi
else
    mkdir -p "$cache_root/cargo" "$cache_root/target" "$cache_root/npm" "$cache_root/playwright" "$cache_root/tmp"
    if "$docker_cmd" image inspect "$image" >/dev/null 2>&1; then
        log "образ уже есть на этой машине"
    else
        "$docker_cmd" build -t "$image" \
            --build-arg "RUST_VERSION=$rust_version" \
            --build-arg "PLAYWRIGHT_VERSION=$playwright_version" \
            -f "$dockerfile" "$repo/docker/gate" || die "не удалось собрать образ"
    fi
fi
phase_done "образ"

# --- гейт в контейнере ---
# Ограничения: у 7950X 32 потока и 61 ГБ. --cpus 30 оставляет хосту два потока на
# ssh, dockerd и повседневную работу; --memory 48g оставляет ~13 ГБ запаса
# (гейт на VM укладывается в ~10 ГБ); pids 8192 — Playwright, супервизоры
# исполнений и nextest вместе берут тысячи процессов. --init нужен, потому что
# PID 1 в контейнере иначе не пожинает осиротевших потомков, а они считаются в
# pids-limit. --cap-add SYS_PTRACE — тесты долговечности каталогов запускают
# strace, а профиль seccomp docker без этой возможности ptrace запрещает.
phase "гейт в контейнере"
run_args=(
    --rm --init
    --user "${uid}:${gid}"
    --cpus 30 --memory 48g --pids-limit 8192
    # Браузеры Playwright держат общую память в /dev/shm, а docker по умолчанию
    # даёт 64 МБ: с ними страницы не доходят до [data-ready], и тесты падают по
    # таймауту.
    --shm-size=2g
    --cap-add SYS_PTRACE
    # /tmp — отдельный каталог кэша, а не overlay контейнера и не tmpfs: на
    # overlayfs одновременные Firefox-воркеры не успевают отрисовать страницу до
    # [data-ready], а страницы tmpfs нельзя выбросить без swap и запуск двойников
    # CLI в Rust-тестах растягивается до таймаута. Обычная ФС хоста держит и то,
    # и другое; каталог чистит in-container.sh в начале прогона.
    -v "$cache_root/tmp:/tmp"
    -v "$src:/src"
    -v "$cache_root/cargo:/cache/cargo"
    -v "$cache_root/target:/src/target"
    -v "$cache_root/npm:/cache/npm"
    -v "$cache_root/playwright:/cache/playwright"
    -v "$src/.gate-out:/out"
    -v "$script_path:/build/in-container.sh:ro"
    -e CARGO_HOME=/cache/cargo
    -e npm_config_cache=/cache/npm
    -e PLAYWRIGHT_BROWSERS_PATH=/cache/playwright
    -w /src
)
gate_code=0
if [[ "$mode" == remote ]]; then
    cmd="docker run"
    for arg in "${run_args[@]}"; do
        cmd+=" $(q "$arg")"
    done
    cmd+=" $(q "$image") bash /build/in-container.sh --src /src --out /out --sha $(q "$sha12")"
    remote "$cmd" || gate_code=$?
else
    "$docker_cmd" run "${run_args[@]}" "$image" \
        bash /build/in-container.sh --src /src --out /out --sha "$sha12" || gate_code=$?
fi
log "гейт завершился с кодом $gate_code"
phase_done "гейт в контейнере"

# --- перенос результата ---
phase "перенос результата"
mkdir -p "$out_root"
staging="$out.partial.$$"
[[ ! -e "$staging" ]] || die "временный каталог $staging уже существует"
mkdir -p "$staging"
if [[ "$mode" == remote ]]; then
    ssh -o BatchMode=yes -o ConnectTimeout=15 "$host" "tar -C $(q "$src/.gate-out") -cf - ." \
        | tar -C "$staging" -xf - || log "ВНИМАНИЕ: не удалось перенести результат с $host"
else
    cp -a "$src/.gate-out/." "$staging/" || log "ВНИМАНИЕ: не удалось скопировать результат из $src/.gate-out"
fi
[[ -f "$staging/result.txt" ]] || log "ВНИМАНИЕ: гейт не оставил result.txt (см. вывод выше)"
mv "$staging" "$out" || die "не удалось переименовать $staging в $out"
log "результат: $out"
phase_done "перенос результата"

phase "уборка"
cleanup
phase_done "уборка"

printf '\nрезультат гейта %s (%s):\n' "$sha12" "$out"
if [[ -f "$out/result.txt" ]]; then
    cat "$out/result.txt"
else
    printf 'result.txt нет: гейт не дошёл до проверок\n'
fi
printf '\nлоги: %s/{build,check,web}.log\n' "$out"
printf 'гейт %s закончил за %ds, код %s\n' "$sha12" $((SECONDS - started)) "$gate_code"
[[ "$gate_code" != 0 ]] || [[ -f "$out/result.txt" ]] || gate_code=1
exit "$gate_code"
