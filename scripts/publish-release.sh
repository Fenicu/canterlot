#!/usr/bin/env bash
# Публикует собранный заранее выпуск на Forgejo: архивы хаба, воркера и PWA, CSP, SHA256SUMS
# и раздел CHANGES.rst как описание. Ничего не собирает: берёт готовый каталог
# .superpowers/deploy/dist/<sha12>/, который кладёт scripts/build-release.sh.
#
#   scripts/publish-release.sh v1.11.0             собрать архивы и опубликовать
#   scripts/publish-release.sh v1.11.0 --dry-run   только собрать архивы и показать описание
#
# Каталог сборок можно указать в CANTERLOT_DIST_ROOT (по умолчанию в этой рабочей копии).
# Токен Forgejo с правом write:repository читается из FORGEJO_TOKEN_FILE
# (по умолчанию ~/.config/forgejo/canterlot-release.token) и никуда не печатается.
set -euo pipefail

tag=${1:-}
dry_run=0
[[ "${2:-}" == --dry-run ]] && dry_run=1
if [[ ! "$tag" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    echo 'Использование: scripts/publish-release.sh vX.Y.Z [--dry-run]' >&2
    exit 2
fi
version=${BASH_REMATCH[1]}
api=${FORGEJO_API:-https://git.fenicu.com/api/v1/repos/Fenicu/canterlot}
token_file=${FORGEJO_TOKEN_FILE:-$HOME/.config/forgejo/canterlot-release.token}

cd "$(dirname "${BASH_SOURCE[0]}")/.."
sha=$(git rev-parse --verify "$tag^{commit}")
dist=${CANTERLOT_DIST_ROOT:-.superpowers/deploy/dist}/${sha:0:12}
[[ -f "$dist/SHA256SUMS" ]] || { echo "нет сборки $dist: соберите выпуск scripts/build-release.sh $tag" >&2; exit 1; }
grep -qx "source=$sha" "$dist/RELEASE" || { echo "$dist/RELEASE собран не из $tag" >&2; exit 1; }
(cd "$dist" && sha256sum --quiet -c SHA256SUMS) || { echo "$dist не сходится с SHA256SUMS" >&2; exit 1; }
for bin in hub/bin/canterlot-hub worker/bin/canterlot-worker; do
    got=$("$dist/$bin" --version | awk '{print $NF}')
    [[ "$got" == "$version" ]] || { echo "$bin --version = $got, ожидалась $version" >&2; exit 1; }
done

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
mtime=$(git log -1 --format=%ct "$sha")
# Без корневой записи «./»: распаковка в /usr/local/bin не трогает права самого каталога.
pack() {
    local name=$1 dir=$2
    local -a entries
    mapfile -t entries < <(cd "$dir" && LC_ALL=C ls -A)
    tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$mtime" --mode="u+rwX,go+rX,go-w" \
        -C "$dir" -cf - -- "${entries[@]}" | gzip -n -9 >"$out/$name"
}
triple=x86_64-linux-gnu
pack "canterlot-hub-$version-$triple.tar.gz" "$dist/hub/bin"
pack "canterlot-worker-$version-$triple.tar.gz" "$dist/worker/bin"
pack "canterlot-web-$version.tar.gz" "$dist/web"
# CSP с примером домена: при развёртывании chat.example.com заменяется на свой адрес PWA.
node --input-type=module -e '
    import { readFileSync } from "node:fs";
    import { createHash } from "node:crypto";
    import { productionCsp } from "./web/csp.mjs";
    const shell = readFileSync(process.argv[1], "utf8");
    const hashes = [...shell.matchAll(/<script\b(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)]
        .map(m => `sha256-${createHash("sha256").update(m[1]).digest("base64")}`);
    process.stdout.write(productionCsp({ websocketOrigin: "wss://chat.example.com", scriptHashes: hashes }) + "\n");
' "$dist/web/200.html" >"$out/canterlot-csp-$version.txt"
(cd "$out" && sha256sum -- *.tar.gz *.txt >SHA256SUMS)

# Раздел выпуска из CHANGES.rst: заголовки разделов RST превращаются в заголовки Markdown.
body=$(awk -v start=".. _$tag:" '
    $0 == start { inside = 1; skip = 3; next }
    inside && /^\.\. _v/ { exit }
    inside && skip > 0 { skip--; next }
    inside { print }
' CHANGES.rst | awk '
    /^~+$/ { if (prev != "") { lines[n-1] = "### " prev }; prev = ""; next }
    { lines[n++] = $0; prev = $0 }
    END { for (i = 0; i < n; i++) print lines[i] }
')
[[ -n "${body//[[:space:]]/}" ]] || { echo "в CHANGES.rst нет раздела $tag" >&2; exit 1; }
body+=$'\n\n### Файлы\n\nХаб и воркер собраны для x86_64 под glibc Debian 13 (2.41) и новее. PWA — статические файлы для Caddy, `canterlot-csp-*.txt` — заголовок CSP для них с примером домена chat.example.com. Контрольные суммы — в `SHA256SUMS`.'

if ((dry_run)); then
    ls -l "$out"
    printf '%s\n' "$body"
    exit 0
fi

[[ -s "$token_file" ]] || { echo "нет токена $token_file" >&2; exit 1; }
auth=(-H "Authorization: token $(tr -d '\n' <"$token_file")")

release=$(curl -fsS "${auth[@]}" "$api/releases/tags/$tag" 2>/dev/null || true)
if [[ -z "$release" ]]; then
    release=$(jq -n --arg tag "$tag" --arg name "canterlot $version" --arg body "$body" \
        '{tag_name: $tag, name: $name, body: $body, draft: false, prerelease: false}' |
        curl -fsS "${auth[@]}" -H 'Content-Type: application/json' -X POST "$api/releases" -d @-)
    echo "создан выпуск $tag"
fi
id=$(jq -r .id <<<"$release")
have=$(jq -r '.assets[]?.name' <<<"$release")
for file in "$out"/*; do
    name=$(basename "$file")
    if grep -qxF "$name" <<<"$have"; then
        echo "уже есть: $name"
        continue
    fi
    curl -fsS "${auth[@]}" -X POST -F "attachment=@$file" \
        "$api/releases/$id/assets?name=$name" >/dev/null
    echo "загружен: $name"
done
echo "https://git.fenicu.com/Fenicu/canterlot/releases/tag/$tag"
