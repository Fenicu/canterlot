#!/usr/bin/env bash
# Сборка выпуска внутри контейнера canterlot-release-build: хаб, воркер,
# синтетический воркер, служба распознавания речи и PWA из смонтированного
# чистого чекаута.
#
#   scripts/release-build/in-container.sh --src DIR --out DIR --sha SHA12 --commit SHA40
#
# Окружение:
#   CARGO_TARGET_DIR   каталог target на смонтированном кэше (обязателен);
#   CARGO_HOME         каталог cargo на смонтированном кэше (ставит build-release.sh);
#   npm_config_cache   кэш npm на смонтированном кэше;
#   TRANSCRIBE_CMAKE_ARGS  флаги CMake для transcribe.cpp (по умолчанию sm_70,
#                      GGML_NATIVE=OFF, без системного BLAS); их задаёт
#                      build-release.sh и включает в ключ кэша нативной сборки;
#   SITE_ORIGIN        адрес PWA для csp.txt (по умолчанию https://hub.example.com);
#   WSS_ORIGIN         адрес WebSocket для проверки csp.txt (по умолчанию wss://hub.example.com).
#
# Тестов здесь нет: полный прогон scripts/check.sh и check-web.sh уже прошёл на
# коммите. Скрипт собирает и проверяет ровно то, что проверяет выкат (Task 1
# плана пилота), плюс максимальную версию GLIBC у бинарников.
set -euo pipefail
export LC_ALL=C

src=""
out=""
want_sha=""
want_commit=""
while (($#)); do
    case "$1" in
        --src) src=${2:?--src требует каталог}; shift ;;
        --out) out=${2:?--out требует каталог}; shift ;;
        --sha) want_sha=${2:?--sha требует sha12}; shift ;;
        --commit) want_commit=${2:?--commit требует полный sha}; shift ;;
        *) echo "Использование: $0 --src DIR --out DIR --sha SHA12 --commit SHA40" >&2; exit 2 ;;
    esac
    shift
done
[[ -n "$src" && -n "$out" && -n "$want_sha" && -n "$want_commit" ]] \
    || { echo "Использование: $0 --src DIR --out DIR --sha SHA12 --commit SHA40" >&2; exit 2; }
[[ "$want_sha" =~ ^[0-9a-f]{12}$ ]] || { echo "FATAL: --sha не sha12: $want_sha" >&2; exit 1; }
[[ "$want_commit" =~ ^[0-9a-f]{40}$ ]] || { echo "FATAL: --commit не полный sha: $want_commit" >&2; exit 1; }
[[ "${want_commit:0:12}" == "$want_sha" ]] || { echo "FATAL: --sha $want_sha не начало --commit $want_commit" >&2; exit 1; }
[[ -d "$src" ]] || { echo "FATAL: нет каталога исходников $src" >&2; exit 1; }
[[ -d "$out" ]] || { echo "FATAL: нет каталога вывода $out" >&2; exit 1; }
[[ -z "$(ls -A "$out")" ]] || { echo "FATAL: каталог вывода $out не пуст" >&2; exit 1; }
: "${CARGO_TARGET_DIR:?CARGO_TARGET_DIR не задан: target должен лежать на кэше}"
export CARGO_TARGET_DIR
export CARGO_HOME=${CARGO_HOME:-/usr/local/cargo}
export npm_config_cache=${npm_config_cache:-$HOME/.npm}
site_origin=${SITE_ORIGIN:-https://hub.example.com}
wss_origin=${WSS_ORIGIN:-wss://hub.example.com}

started=$SECONDS
phase() { printf '\n==> %s\n' "$1"; phase_started=$SECONDS; }
phase_done() { printf '    %s: %ds\n' "$1" $((SECONDS - phase_started)); }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

# Содержимое EXPECT по выводу `canterlot-worker capabilities`: что предлагает
# воркер выпуска. Выкладка сверяет с ним развёрнутый воркер.
expect_of() {
    local listing kind name rest capabilities="" harnesses=""
    listing=$("$1" capabilities) || die "$1 capabilities завершился с ошибкой"
    while read -r kind name rest; do
        [[ -n "$name" && -z "$rest" ]] || die "неожиданная строка capabilities: $kind $name $rest"
        case "$kind" in
            capability) capabilities+="${capabilities:+ }$name" ;;
            harness) harnesses+="${harnesses:+ }$name" ;;
            *) die "неожиданная строка capabilities: $kind $name" ;;
        esac
    done <<<"$listing"
    printf 'format=1\ncapabilities=%s\nharnesses=%s\n' "$capabilities" "$harnesses"
}

sha=$want_sha
commit=$want_commit

printf 'сборка выпуска %s из %s в %s\n' "$sha" "$src" "$out"
printf 'cargo: %s, node: %s, protoc: %s\n' "$(cargo --version)" "$(node --version)" "$(protoc --version)"
printf 'кэши: cargo=%s target=%s npm=%s\n' "$CARGO_HOME" "$CARGO_TARGET_DIR" "$npm_config_cache"

phase "бинарники хаба и воркера"
mkdir -p "$out/hub/bin" "$out/worker/bin" "$out/synthetic/bin" "$out/web"
(
    cd "$src"
    cargo build --release --locked -p canterlot-hub -p canterlot-worker
)
install -m 0755 "$CARGO_TARGET_DIR/release/canterlot-hub" "$out/hub/bin/"
install -m 0755 "$CARGO_TARGET_DIR/release/canterlot-worker" "$out/worker/bin/"
phase_done "хаб и воркер"

phase "синтетический воркер"
(
    cd "$src"
    cargo build --release --locked -p canterlot-worker --features synthetic-adapter
)
install -m 0755 "$CARGO_TARGET_DIR/release/canterlot-worker" "$out/synthetic/bin/"
phase_done "синтетический воркер"

# canterlot-speech: CUDA 12.9 из образа, код только для sm_70 (Tesla V100). CPU-часть
# ggml без подстройки под сборочную машину (GGML_NATIVE=OFF: AVX2/FMA/F16C), без
# системного BLAS. Хаб и воркер собраны выше отдельным вызовом и CUDA не видят.
# Строку флагов задаёт scripts/build-release.sh (он же включает её в ключ кэша
# нативной сборки); значение по умолчанию — для прямого запуска этого скрипта.
phase "служба распознавания речи (CUDA)"
cuda_root=${CUDA_PATH:-/usr/local/cuda-12.9}
[[ -x "$cuda_root/bin/nvcc" ]] || die "нет nvcc в $cuda_root"
export TRANSCRIBE_CMAKE_ARGS=${TRANSCRIBE_CMAKE_ARGS:--DCMAKE_CUDA_ARCHITECTURES=70 -DGGML_NATIVE=OFF -DTRANSCRIBE_USE_SYSTEM_BLAS=OFF}
printf '    TRANSCRIBE_CMAKE_ARGS: %s\n' "$TRANSCRIBE_CMAKE_ARGS"
(
    cd "$src"
    export PATH="$cuda_root/bin:$PATH" CUDACXX="$cuda_root/bin/nvcc" CUDAToolkit_ROOT="$cuda_root"
    cargo build --release --locked -p canterlot-speech --features cuda
)
install -m 0755 "$CARGO_TARGET_DIR/release/canterlot-speech" "$out/hub/bin/"
phase_done "служба распознавания речи"

phase "PWA"
(
    cd "$src/web"
    npm ci
    PWA_VERSION="$sha" npm run build
)
phase_done "PWA"

phase "укладка dist"
html_files=$(find "$src/web/build" -name '*.html')
[[ "$html_files" = "$src/web/build/200.html" ]] || die "ожидался только build/200.html, получено: $html_files"
node "$src/web/scripts/print-csp.mjs" "$site_origin" >"$out/csp.txt"
cp -a "$src/web/build/." "$out/web/"
migrations_last=$(find "$src/crates/canterlot-hub/migrations" -maxdepth 1 -name '*.sql' -printf '%f\n' | sort | tail -1)
[[ -n "$migrations_last" ]] || die "в crates/canterlot-hub/migrations нет *.sql"
{
    printf 'sha=%s\n' "$sha"
    printf 'date=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'migrations_last=%s\n' "$migrations_last"
    printf 'source=%s\n' "$commit"
} >"$out/RELEASE"
expect_of "$out/worker/bin/canterlot-worker" >"$out/EXPECT"
(
    cd "$out"
    find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS.new
    mv SHA256SUMS.new SHA256SUMS
)
phase_done "укладка dist"

phase "проверки выпуска"
ldd_out=$(ldd "$out/hub/bin/canterlot-hub")
case "$ldd_out" in
    *libssl*) die "hub линкует libssl" ;;
    *cuda* | *cublas*) die "hub линкует CUDA" ;;
esac
# Прямые зависимости canterlot-speech: системные библиотеки glibc и C++ плюс ровно
# библиотеки CUDA 12, которые стоят на проде (драйвер и redist cudart/cublas).
speech_bin="$out/hub/bin/canterlot-speech"
speech_needed=$(objdump -p "$speech_bin" | awk '$1 == "NEEDED" {print $2}' | sort)
for lib in $speech_needed; do
    case "$lib" in
        libc.so.6 | libm.so.6 | libdl.so.2 | librt.so.1 | libpthread.so.0 | ld-linux-x86-64.so.2) ;;
        libgcc_s.so.1 | libstdc++.so.6 | libgomp.so.1) ;;
        libcuda.so.1 | libcudart.so.12 | libcublas.so.12 | libcublasLt.so.12) ;;
        *) die "canterlot-speech зависит от неожиданной библиотеки $lib" ;;
    esac
done
for lib in libcuda.so.1 libcudart.so.12 libcublas.so.12; do
    grep -qx "$lib" <<<"$speech_needed" || die "canterlot-speech собран без CUDA: нет $lib"
done
if objdump -p "$speech_bin" | grep -qE 'RPATH|RUNPATH'; then
    die "у canterlot-speech есть RPATH/RUNPATH"
fi
printf '    canterlot-speech NEEDED: %s\n' "$(tr '\n' ' ' <<<"$speech_needed")"
LD_LIBRARY_PATH="$cuda_root/lib:$cuda_root/lib/stubs" ldd "$speech_bin" | sed 's/^/    ldd: /'
"$out/hub/bin/canterlot-hub" openapi | cmp -s - "$src/docs/api/openapi.json" \
    || die "hub openapi отличается от docs/api/openapi.json"
wss_n=$(grep -c -- "$wss_origin" "$out/csp.txt" || true)
[[ "$wss_n" = "1" ]] || die "в csp.txt не ровно один $wss_origin: $wss_n"
[[ "$(grep -o "'sha256-[^']*'" "$out/csp.txt" | wc -l)" -ge 1 ]] || die "в csp.txt нет хэша скрипта"
[[ "$(jq -r .version "$out/web/_app/version.json")" = "$sha" ]] \
    || die "web _app/version.json не $sha"
syn_prod=$(strings "$out/worker/bin/canterlot-worker" | grep -c 'synthetic-cli' || true)
[[ "$syn_prod" = "0" ]] || die "в боевом воркере есть synthetic-cli"
syn_synth=$(strings "$out/synthetic/bin/canterlot-worker" | grep -c 'synthetic-cli' || true)
[[ "$syn_synth" -ge 1 ]] || die "в синтетическом воркере нет synthetic-cli"
grep -qx 'format=1' "$out/EXPECT" || die "в EXPECT нет format=1"
grep -qx 'capabilities=[^ ].*' "$out/EXPECT" || die "в EXPECT нет или пуст capabilities="
grep -qx 'harnesses=[^ ].*' "$out/EXPECT" || die "в EXPECT нет или пуст harnesses="
expect_of "$out/worker/bin/canterlot-worker" | cmp -s - "$out/EXPECT" \
    || die "EXPECT отличается от вывода боевого воркера capabilities"
(cd "$out" && sha256sum --quiet -c SHA256SUMS) || die "SHA256SUMS не сходится"

# Максимальная версия символа GLIBC: прод — Debian 13 с glibc 2.41, новее нельзя.
max_glibc() {
    objdump -T "$1" | grep -o 'GLIBC_[0-9.]*' | sed 's/^GLIBC_//' | sort -V | tail -1
}
for bin in hub/bin/canterlot-hub hub/bin/canterlot-speech worker/bin/canterlot-worker synthetic/bin/canterlot-worker; do
    version=$(max_glibc "$out/$bin")
    [[ -n "$version" ]] || die "у $bin не найдено версий символов GLIBC"
    highest=$(printf '%s\n2.41\n' "$version" | sort -V | tail -1)
    [[ "$highest" = "2.41" ]] || die "$bin требует GLIBC_$version > 2.41"
    printf '    %s: GLIBC_%s, %s байт\n' "$bin" "$version" "$(stat -c %s "$out/$bin")"
done
phase_done "проверки выпуска"

printf '\nвыпуск %s собран и проверен за %ds\n' "$sha" $((SECONDS - started))
cat "$out/RELEASE"
