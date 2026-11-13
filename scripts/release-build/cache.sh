#!/usr/bin/env bash
# Кэш нативной части сборочного контейнера для scripts/build-release.sh.
#
#   cache.sh key IMAGE_ID CMAKE_ARGS   ключ каталога target/<ключ>
#   cache.sh prune ROOT KEEP           удалить каталоги-ключи, кроме KEEP
#
# Ключ — хэш схемы, id образа и настроек CMake для transcribe.cpp/ggml. Образ
# меняется вместе с Dockerfile, rust-toolchain.toml и базой (CUDA, glibc, nvcc),
# поэтому нативные объекты под другим образом и с другими флагами лежат в другом
# каталоге и не переиспользуются. Схема в ключе — на случай смены самого правила.
#
# prune трогает только каталоги, имя которых — 16 шестнадцатеричных символов
# (наш ключ), и никогда не удаляет KEEP: чужие файлы и старый расклад кэша
# (target/release, target/debug до этой схемы) остаются на месте.
set -euo pipefail

die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

case "${1:-}" in
    key)
        [[ $# -eq 3 ]] || die "использование: $0 key IMAGE_ID CMAKE_ARGS"
        [[ "$2" =~ ^sha256:[0-9a-f]{64}$ ]] || die "не id образа: '$2'"
        printf 'canterlot-native-v1\n%s\n%s\n' "$2" "$3" | sha256sum | cut -c1-16
        ;;
    prune)
        [[ $# -eq 3 ]] || die "использование: $0 prune ROOT KEEP"
        root=$2
        keep=$3
        [[ "$keep" =~ ^[0-9a-f]{16}$ ]] || die "не ключ кэша: '$keep'"
        [[ -d "$root" ]] || exit 0
        while IFS= read -r path; do
            name=${path##*/}
            [[ "$name" != "$keep" ]] || continue
            [[ "$name" =~ ^[0-9a-f]{16}$ ]] || continue
            [[ -d "$path" && ! -L "$path" ]] || continue
            rm -rf -- "$path"
            printf '    старый кэш нативной сборки удалён: %s\n' "$path"
        done < <(find "$root" -mindepth 1 -maxdepth 1 -type d -print)
        ;;
    *)
        die "использование: $0 key IMAGE_ID CMAKE_ARGS | prune ROOT KEEP"
        ;;
esac
