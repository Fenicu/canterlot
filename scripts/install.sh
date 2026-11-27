#!/usr/bin/env bash
# Тонкий загрузчик утилиты canterlot: один тег, одна контрольная сумма, один запуск.
# Публикуется как install.sh; работает и через `curl … | bash -s -- args`.
set -euo pipefail

prog=install.sh
fail() { printf '%s: %s\n' "$prog" "$1" >&2; exit 1; }

for tool in curl tar sha256sum uname mktemp grep sed getconf; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing_tool $tool"
done
[ "$(uname -m)" = x86_64 ] || fail unsupported_arch

# Утилита собрана под glibc 2.41: на старом дистрибутиве она не запустится.
glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null || true)
glibc=${glibc#glibc }
major=${glibc%%.*}
minor_part=${glibc#*.}
minor=${minor_part%%.*}
case "$major" in '' | *[!0-9]*) fail glibc_too_old ;; esac
case "$minor" in '' | *[!0-9]*) fail glibc_too_old ;; esac
if ((major < 2 || (major == 2 && minor < 41))); then
    printf '%s: glibc_too_old\n' "$prog" >&2
    printf 'glibc 2.41 or newer is required; build canterlot from source: cargo build --release -p canterlot-ctl\n' >&2
    exit 1
fi

api_base=${CANTERLOT_API_BASE:-https://api.github.com/repos/Fenicu/canterlot}
release_base=${CANTERLOT_RELEASE_BASE:-https://github.com/Fenicu/canterlot/releases/download}

# --version — флаг подкоманды утилиты: убираем его из начала и добавляем после подкоманды.
original=("$@")
args=()
version=""
i=0
while ((i < ${#original[@]})); do
    if [[ ${original[$i]} == --version ]]; then
        ((i + 1 < ${#original[@]})) || fail bad_version
        version=${original[$((i + 1))]}
        ((i += 2))
        continue
    fi
    args+=("${original[$i]}")
    i=$((i + 1))
done
((${#args[@]})) || args=(install)

if [[ -n "$version" ]]; then
    printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || fail bad_version
else
    json=$(curl -fsSL "$api_base/releases/latest") || fail download_failed
    tag=$(printf '%s\n' "$json" | grep -m1 -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' \
        | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]*)".*/\1/')
    printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || fail bad_tag
    version=${tag#v}
fi
tag="v$version"

ctl_name="canterlot-$version-x86_64-linux.tar.gz"
tmp=$(mktemp -d) || fail mktemp_failed
trap 'rm -rf -- "$tmp"' EXIT
curl -fsSL -o "$tmp/SHA256SUMS" "$release_base/$tag/SHA256SUMS" || fail download_failed
curl -fsSL -o "$tmp/$ctl_name" "$release_base/$tag/$ctl_name" || fail download_failed

# Берём только точную строку для архива: в SHA256SUMS перечислены и другие файлы.
pattern="^[[:xdigit:]]{64}[[:space:]]+\\*?${ctl_name//./[.]}$"
line=$(grep -Em1 "$pattern" "$tmp/SHA256SUMS" || true)
[[ -n "$line" ]] || fail checksum_missing
printf '%s\n' "$line" >"$tmp/one.sha256"
(cd "$tmp" && sha256sum -c one.sha256 >/dev/null) || fail checksum_mismatch

listing=$(tar -tzf "$tmp/$ctl_name") || fail unsafe_archive
[[ "$listing" == canterlot ]] || fail unsafe_archive
tar -xzf "$tmp/$ctl_name" -C "$tmp" || fail unsafe_archive
[[ -x "$tmp/canterlot" ]] || fail unsafe_archive

args+=(--version "$version")
"$tmp/canterlot" "${args[@]}"
