#!/usr/bin/env bash
# Проверка контракта Codex: свежая схема app-server против манифестов stage-0.
# Нужен настоящий CLI; запросов к модели нет. Владелец запускает при обновлении CLI.
set -euo pipefail

source ~/.cargo/env 2>/dev/null || true

codex="${CODEX_BIN:-codex}"
manifest_dir="docs/stage0/evidence/codex/schema-0.164.0"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/codex-schema.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

version="$("$codex" --version)"
echo "cli: $version"
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
    while read -r expected file; do
        # Строки с # в манифесте: заголовок с версией и командой, не записи.
        case "$expected" in \#* | "") continue ;; esac
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
    done < "$manifest"
done
if [ "$fail" -eq 0 ]; then
    echo "schema matches stage-0 manifests"
fi
exit "$fail"
