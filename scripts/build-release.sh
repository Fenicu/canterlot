#!/usr/bin/env bash
# Собирает выпуск Canterlot один раз в контейнере Debian 13 на удалённой
# сборочной машине и кладёт готовый dist/<sha12> в эту рабочую копию — оттуда его
# берут выкат и scripts/publish-release.sh.
#
#   scripts/build-release.sh vX.Y.Z            сборка на CANTERLOT_BUILD_HOST (mainpc-build)
#   scripts/build-release.sh vX.Y.Z --local    та же сборка на локальном docker (для проверки)
#
# Почему контейнер: сборочная машина — Arch с glibc 2.44, а прод — Debian 13 с
# glibc 2.41. Сборка в debian:trixie даёт бинарники, которые запускаются на проде.
#
# Тег обязан быть локально и на origin. Полный прогон проверок на коммите уже
# прошёл: скрипт только собирает и проверяет состав dist (см.
# scripts/release-build/in-container.sh). Каталог dist можно задать в
# CANTERLOT_DIST_ROOT (по умолчанию .superpowers/deploy/dist этой рабочей копии).
set -euo pipefail
export LC_ALL=C

usage() {
    echo "Использование: scripts/build-release.sh vX.Y.Z [--local]" >&2
}
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
dockerfile="$repo/docker/release-build/Dockerfile"
in_container="$repo/scripts/release-build/in-container.sh"
tag=""
mode=remote
while (($#)); do
    case "$1" in
        --local) mode=local ;;
        -h | --help) usage; exit 0 ;;
        v[0-9]*.[0-9]*.[0-9]*) tag=$1 ;;
        *) usage; exit 2 ;;
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
[[ -n "$tag" ]] || { usage; exit 2; }
[[ "$tag" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || { echo "неверный тег: $tag (ожидался vX.Y.Z)" >&2; exit 2; }
version=${BASH_REMATCH[1]}
[[ -f "$dockerfile" && -f "$in_container" ]] || die "в $repo нет docker/release-build/Dockerfile или scripts/release-build/in-container.sh"
[[ -f "$repo/rust-toolchain.toml" ]] || die "в $repo нет rust-toolchain.toml"
git -C "$repo" rev-parse --verify --quiet "refs/tags/$tag" >/dev/null \
    || die "локального тега $tag нет: сначала поставьте и запушьте его"
commit=$(git -C "$repo" rev-parse --verify "$tag^{commit}")
sha12=$(git -C "$repo" rev-parse --short=12 "$tag^{commit}")
dist_root=${CANTERLOT_DIST_ROOT:-.superpowers/deploy/dist}
dist="$dist_root/$sha12"
[[ ! -e "$dist" ]] || die "каталог $dist уже существует: выпуск уже собран (удалите его вручную, если нужна пересборка)"
docker_cmd=${CANTERLOT_DOCKER:-docker}
if [[ "$mode" == local ]]; then
    command -v "$docker_cmd" >/dev/null || die "нет docker ($docker_cmd): --local собирает на этой машине"
else
    host=${CANTERLOT_BUILD_HOST:-mainpc-build}
    command -v ssh >/dev/null || die "нет ssh"
fi
log "выпуск $tag ($sha12), режим $mode"
phase_done "предусловия"

# Тег на origin должен указывать на тот же коммит, иначе соберём не то, что опубликуем.
phase "проверка тега на origin"
if ! refs=$(git -C "$repo" ls-remote --tags origin "refs/tags/$tag" "refs/tags/$tag^{}" 2>&1); then
    die "не удалось прочитать теги origin: $refs"
fi
remote_commit=""
direct_commit=""
while read -r oid ref; do
    [[ -n "${oid:-}" ]] || continue
    case "$ref" in
        "refs/tags/$tag^{}") remote_commit=$oid ;;
        "refs/tags/$tag") direct_commit=$oid ;;
    esac
done <<<"$refs"
[[ -n "$remote_commit" ]] || remote_commit=$direct_commit
[[ "$remote_commit" == "$commit" ]] || die "на origin тег $tag указывает на '${remote_commit:-ничего}', а локально на $commit"
log "origin: $tag -> $commit"
phase_done "тег на origin"

# --- удалённая машина ---
remote() { ssh -n -o BatchMode=yes -o ConnectTimeout=15 "$host" "$1" </dev/null; }
remote_home=""
remote_repo=""
remote_base=""
remote_wt=""
remote_tools=""

if [[ "$mode" == remote ]]; then
    phase "сборочная машина $host"
    # shellcheck disable=SC2016 # $HOME раскрывает удалённый shell
    remote_home=$(remote 'printf %s "$HOME"') || die "ssh $host не отвечает"
    [[ "$remote_home" == /* && "$remote_home" != *$'\n'* ]] || die "странный HOME на $host: '$remote_home'"
    remote_repo="$remote_home/canterlot.git"
    remote_base="$remote_home/release"
    remote_wt="$remote_base/src-$sha12"
    remote_tools="$remote_base/tools"
    remote 'command -v git >/dev/null && command -v docker >/dev/null && command -v tar >/dev/null' \
        || die "на $host нет git, docker или tar"
    [[ "$(remote "git --git-dir $(q "$remote_repo") rev-parse --is-bare-repository 2>/dev/null")" == "true" ]] \
        || die "на $host нет bare-репозитория $remote_repo"
    remote "docker info >/dev/null 2>&1" || die "docker на $host не работает"
    phase_done "сборочная машина"

    phase "отправка тега"
    git -C "$repo" push --quiet "$host:canterlot.git" "refs/tags/$tag:refs/tags/$tag" \
        || die "не удалось отправить тег $tag на $host"
    log "тег $tag отправлен в $remote_repo"
    phase_done "отправка тега"
fi

# --- чистая рабочая копия на коммите тега ---
phase "чистая рабочая копия"
if [[ "$mode" == remote ]]; then
    # shellcheck disable=SC2016 # $wt раскрывает удалённый shell
    cmd=$(printf 'set -e; wt=%q; if [ -e "$wt" ]; then git --git-dir %q worktree remove --force "$wt"; fi; git --git-dir %q worktree prune; mkdir -p %q; git --git-dir %q worktree add --detach "$wt" %q; mkdir -p "$wt/.release-out"' \
        "$remote_wt" "$remote_repo" "$remote_repo" "$remote_base" "$remote_repo" "$commit")
    remote "$cmd" || die "не удалось создать рабочую копию $remote_wt на $host"
    src="$remote_wt"
else
    common=$(cd "$repo" && cd "$(git rev-parse --git-common-dir)" && pwd)
    worktree_parent="$common/canterlot-release-build"
    src="$worktree_parent/src-$sha12"
    mkdir -p "$worktree_parent"
    if [[ -e "$src" ]]; then
        git -C "$repo" worktree remove --force "$src" || die "не удалось убрать старую рабочую копию $src"
    fi
    git -C "$repo" worktree prune
    git -C "$repo" worktree add --detach "$src" "$commit" >/dev/null
    mkdir -p "$src/.release-out"
fi
if [[ "$mode" == remote ]]; then
    [[ "$(remote "git -C $(q "$src") rev-parse --short=12 HEAD")" == "$sha12" ]] \
        || die "рабочая копия $src не на $sha12"
    dirty=$(remote "git -C $(q "$src") status --porcelain") || die "не удалось проверить чистоту $src"
else
    [[ "$(git -C "$src" rev-parse --short=12 HEAD)" == "$sha12" ]] || die "рабочая копия $src не на $sha12"
    dirty=$(git -C "$src" status --porcelain)
fi
[[ -z "$dirty" ]] || die "рабочая копия $src грязная"
log "источник: $src"
phase_done "рабочая копия"

# --- образ ---
phase "сборочный образ"
rust_version=$(git -C "$repo" show "$tag:rust-toolchain.toml" | sed -n 's/^channel = "\(.*\)"/\1/p')
[[ -n "$rust_version" ]] || die "в rust-toolchain.toml тега $tag нет channel"
image_tag=$({ cat "$dockerfile"; git -C "$repo" show "$tag:rust-toolchain.toml"; } | sha256sum | cut -c1-12)
image="canterlot-release-build:$image_tag"
log "образ $image (Rust $rust_version)"
if [[ "$mode" == remote ]]; then
    remote "mkdir -p $(q "$remote_tools")" || die "не удалось создать $remote_tools"
    # Dockerfile и in-container.sh берутся из ЭТОЙ рабочей копии: тег может быть
    # старше сборочного инструмента (первая сборка v1.16.1 именно такая).
    ssh -o BatchMode=yes -o ConnectTimeout=15 "$host" "cat > $(q "$remote_tools/Dockerfile")" <"$dockerfile" \
        || die "не удалось скопировать Dockerfile на $host"
    ssh -o BatchMode=yes -o ConnectTimeout=15 "$host" "cat > $(q "$remote_tools/in-container.sh")" <"$in_container" \
        || die "не удалось скопировать in-container.sh на $host"
    if remote "docker image inspect $(q "$image") >/dev/null 2>&1"; then
        log "образ уже есть на $host"
    else
        cmd=$(printf 'docker build -t %q --build-arg %q -f %q %q' \
            "$image" "RUST_VERSION=$rust_version" "$remote_tools/Dockerfile" "$remote_tools")
        remote "$cmd" || die "не удалось собрать образ на $host"
    fi
    script_path="$remote_tools/in-container.sh"
    cache_root="$remote_base/cache"
else
    if "$docker_cmd" image inspect "$image" >/dev/null 2>&1; then
        log "образ уже есть на этой машине"
    else
        "$docker_cmd" build -t "$image" --build-arg "RUST_VERSION=$rust_version" \
            -f "$dockerfile" "$repo/docker/release-build" || die "не удалось собрать образ"
    fi
    script_path="$in_container"
    cache_root=${CANTERLOT_BUILD_CACHE:-$HOME/.cache/canterlot-release-build}
fi
if [[ "$mode" == remote ]]; then
    remote "mkdir -p $(q "$cache_root/cargo") $(q "$cache_root/target") $(q "$cache_root/npm")" \
        || die "не удалось создать кэши в $cache_root"
else
    mkdir -p "$cache_root/cargo" "$cache_root/target" "$cache_root/npm"
fi
phase_done "образ"

# --- сборка в контейнере ---
phase "сборка в контейнере"
if [[ "$mode" == remote ]]; then
    # $(id -u) должен раскрыться на сборочной машине, поэтому строка собирается
    # здесь, а значения путей проходят через %q.
    # shellcheck disable=SC2016 # $(id -u) раскрывает удалённый shell
    cmd=$(printf 'docker run --rm --user "$(id -u):$(id -g)" --cpus 24 --memory 40g --pids-limit 4096 -v %q:/src -v %q:/cache/cargo -v %q:/cache/target -v %q:/cache/npm -v %q:/out -v %q:/build/in-container.sh:ro -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target -e npm_config_cache=/cache/npm -e %q -e %q -w /src %q bash /build/in-container.sh --src /src --out /out --sha %q --commit %q' \
        "$src" "$cache_root/cargo" "$cache_root/target" "$cache_root/npm" "$src/.release-out" "$script_path" \
        "SITE_ORIGIN=${CANTERLOT_SITE_ORIGIN:-https://hub.fenicu.com}" \
        "WSS_ORIGIN=${CANTERLOT_WSS_ORIGIN:-wss://hub.fenicu.com}" "$image" "$sha12" "$commit")
    remote "$cmd" || die "сборка в контейнере на $host не удалась"
else
    "$docker_cmd" run --rm --user "$(id -u):$(id -g)" --cpus 24 --memory 40g --pids-limit 4096 \
        -v "$src:/src" \
        -v "$cache_root/cargo:/cache/cargo" \
        -v "$cache_root/target:/cache/target" \
        -v "$cache_root/npm:/cache/npm" \
        -v "$src/.release-out:/out" \
        -v "$script_path:/build/in-container.sh:ro" \
        -e CARGO_HOME=/cache/cargo \
        -e CARGO_TARGET_DIR=/cache/target \
        -e npm_config_cache=/cache/npm \
        -e "SITE_ORIGIN=${CANTERLOT_SITE_ORIGIN:-https://hub.fenicu.com}" \
        -e "WSS_ORIGIN=${CANTERLOT_WSS_ORIGIN:-wss://hub.fenicu.com}" \
        -w /src \
        "$image" \
        bash /build/in-container.sh --src /src --out /out --sha "$sha12" --commit "$commit" \
        || die "сборка в контейнере не удалась"
fi
phase_done "сборка в контейнере"

# --- перенос результата ---
phase "перенос результата"
mkdir -p "$dist_root"
staging="$dist.partial.$$"
[[ ! -e "$staging" ]] || die "временный каталог $staging уже существует"
mkdir -p "$staging"
if [[ "$mode" == remote ]]; then
    ssh -o BatchMode=yes -o ConnectTimeout=15 "$host" "tar -C $(q "$src/.release-out") -cf - ." \
        | tar -C "$staging" -xf - || die "не удалось перенести результат с $host"
else
    cp -a "$src/.release-out/." "$staging/" || die "не удалось скопировать результат из $src/.release-out"
fi
log "результат во временном каталоге $staging"
phase_done "перенос результата"

# --- проверка на этой машине ---
phase "проверка на этой машине"
[[ -f "$staging/SHA256SUMS" ]] || die "в $staging нет SHA256SUMS"
(cd "$staging" && sha256sum --quiet -c SHA256SUMS) || die "$staging не сходится с SHA256SUMS"
grep -qx "sha=$sha12" "$staging/RELEASE" || die "$staging/RELEASE собран не из $sha12"
grep -qx "source=$commit" "$staging/RELEASE" || die "$staging/RELEASE собран не из $commit"
top=$(find "$staging" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)
[[ "$(printf '%s\n' "$top" | wc -l)" == "7" ]] || die "неожиданный состав $staging: $(printf '%s' "$top" | tr '\n' ' ')"
for f in hub/bin/canterlot-hub worker/bin/canterlot-worker synthetic/bin/canterlot-worker \
    web/200.html web/_app/version.json csp.txt; do
    [[ -f "$staging/$f" ]] || die "в $staging нет $f"
done
[[ ! -e "$staging/web/web" ]] || die "в $staging/web лишний уровень web/"
[[ "$(jq -r .version "$staging/web/_app/version.json")" == "$sha12" ]] || die "web/_app/version.json не $sha12"
for bin in hub/bin/canterlot-hub worker/bin/canterlot-worker; do
    got=$("$staging/$bin" --version | awk '{print $NF}')
    [[ "$got" == "$version" ]] || die "$bin --version = $got, ожидалась $version"
done
log "файлов: $(find "$staging" -type f | wc -l), SHA256SUMS и RELEASE сходятся"
phase_done "проверка"

mv "$staging" "$dist" || die "не удалось переименовать $staging в $dist"
log "выпуск готов: $dist"

# --- уборка ---
phase "уборка"
if [[ "$mode" == remote ]]; then
    cmd=$(printf 'git --git-dir %q worktree remove --force %q && git --git-dir %q worktree prune' \
        "$remote_repo" "$remote_wt" "$remote_repo")
    if remote "$cmd"; then
        log "рабочая копия $remote_wt удалена на $host"
    else
        log "ВНИМАНИЕ: не удалось удалить $remote_wt на $host; уберите её вручную: git --git-dir $remote_repo worktree remove --force $remote_wt"
    fi
else
    if git -C "$repo" worktree remove --force "$src"; then
        rmdir "$worktree_parent" 2>/dev/null || true
        log "рабочая копия $src удалена"
    else
        log "ВНИМАНИЕ: не удалось удалить рабочую копию $src; уберите её: git -C $repo worktree remove --force $src"
    fi
fi
phase_done "уборка"

printf '\nготово за %ds: %s\n' $((SECONDS - started)) "$dist"
