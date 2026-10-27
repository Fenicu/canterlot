#!/usr/bin/env bash
set -euo pipefail

# Перегенерирует docs/api/openapi.json из кода хаба; тест openapi_is_served_and_committed_file_matches_generated
# падает, пока закоммиченный файл отличается от сгенерированного.
# shellcheck source=/dev/null
source ~/.cargo/env
cd "$(dirname "$0")/.."
temporary=$(mktemp docs/api/openapi.json.XXXXXX)
trap 'rm -f "$temporary"' EXIT
cargo run --quiet -p canterlot-hub -- openapi > "$temporary"
chmod 644 "$temporary"
mv "$temporary" docs/api/openapi.json
trap - EXIT
printf 'docs/api/openapi.json updated\n'
