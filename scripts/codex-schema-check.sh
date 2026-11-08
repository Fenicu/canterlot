#!/usr/bin/env bash
# Проверка контракта Codex: свежая схема app-server против манифестов stage-0 той же версии.
# Нужен настоящий CLI; запросов к модели нет. Владелец запускает при обновлении CLI.
#
#   CODEX_BIN=<путь к codex>   проверить не тот codex, что в PATH (например, новый CLI
#                              из изолированного префикса: /tmp/cli-codex/node_modules/.bin/codex)
#   CODEX_SCHEMA_MANIFEST_DIR  сверить с другим каталогом манифестов; по умолчанию
#                              docs/stage0/evidence/codex/schema-<версия CLI>
set -euo pipefail

source ~/.cargo/env 2>/dev/null || true

codex="${CODEX_BIN:-codex}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/codex-schema.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

version="$("$codex" --version)"
echo "cli: $version"
number="$(printf '%s\n' "$version" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?' | head -n1)"
manifest_dir="${CODEX_SCHEMA_MANIFEST_DIR:-$root/docs/stage0/evidence/codex/schema-$number}"
if [ ! -d "$manifest_dir" ]; then
    echo "no manifests for $number: $manifest_dir"
    echo "known: $(cd "$root/docs/stage0/evidence/codex" && ls -d schema-* | tr '\n' ' ')"
    exit 1
fi
echo "manifests: ${manifest_dir#"$root"/}"
"$codex" app-server generate-json-schema --out "$tmp/stable" >/dev/null
"$codex" app-server generate-json-schema --experimental --out "$tmp/experimental" >/dev/null

fail=0
for variant in stable experimental; do
    manifest="$manifest_dir/json-schema-$variant.sha256"
    if [ ! -f "$manifest" ]; then
        echo "missing manifest: $manifest"
        fail=1
        continue
    fi
    : >"$tmp/$variant.listed"
    while read -r expected file; do
        # Строки с # в манифесте: заголовок с версией и командой, не записи.
        case "$expected" in \#* | "") continue ;; esac
        echo "$file" >>"$tmp/$variant.listed"
        if [ ! -f "$tmp/$variant/$file" ]; then
            echo "missing file: $variant/$file"
            fail=1
            continue
        fi
        actual="$(sha256sum "$tmp/$variant/$file" | cut -d' ' -f1)"
        if [ "$actual" != "$expected" ]; then
            echo "changed file: $variant/$file"
            fail=1
        fi
    done <"$manifest"
    # Файл схемы, которого нет в манифесте: новый метод или тип той же версии.
    while read -r file; do
        echo "new file: $variant/$file"
        fail=1
    done < <(cd "$tmp/$variant" && find . -type f | LC_ALL=C sort | LC_ALL=C comm -23 - <(LC_ALL=C sort "$tmp/$variant.listed"))
done
if [ "$fail" -eq 0 ]; then
    echo "schema matches stage-0 manifests"
fi
exit "$fail"
