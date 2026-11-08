#!/usr/bin/env bash
# Проверка контракта OpenCode: схема работающего сервера против манифеста stage-0 той же версии.
# Нужен настоящий CLI; каталоги изолированы, запросов к модели нет. Владелец запускает при обновлении CLI.
#
#   OPENCODE_BIN=<путь к opencode>  проверить не тот opencode, что в PATH (например, новый CLI
#                                   из изолированного префикса: /tmp/cli-opencode/node_modules/.bin/opencode)
#   OPENCODE_SCHEMA_MANIFEST        сверить с другим манифестом; по умолчанию
#                                   docs/stage0/evidence/opencode/openapi-<версия CLI>.sha256
set -euo pipefail

opencode="${OPENCODE_BIN:-opencode}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
server=""
cleanup() {
    [ -n "$server" ] && kill "$server" 2>/dev/null || true
    rm -rf "$tmp"
}
trap cleanup EXIT

export HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/config" XDG_DATA_HOME="$tmp/data"
export XDG_STATE_HOME="$tmp/state" XDG_CACHE_HOME="$tmp/cache"
export OPENCODE_PASSWORD="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
export OPENCODE_DISABLE_MODELS_FETCH=1 OPENCODE_DISABLE_AUTOUPDATE=1
mkdir -p "$HOME"

version="$("$opencode" --version)"
echo "cli: $version"
number="$(printf '%s\n' "$version" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?' | head -n1)"
manifest="${OPENCODE_SCHEMA_MANIFEST:-$root/docs/stage0/evidence/opencode/openapi-$number.sha256}"
if [ ! -f "$manifest" ]; then
    echo "no manifest for $number: $manifest"
    echo "known: $(cd "$root/docs/stage0/evidence/opencode" && ls openapi-*.sha256 | tr '\n' ' ')"
    exit 1
fi
echo "manifest: ${manifest#"$root"/}"
expected="$(grep 'GET /openapi.json' "$manifest" | cut -d' ' -f1)"

mkfifo "$tmp/stdin" "$tmp/stdout"
"$opencode" serve --stdio --port 0 <"$tmp/stdin" >"$tmp/stdout" 2>/dev/null &
server=$!
exec 3>"$tmp/stdin"
read -r ready <"$tmp/stdout"
url="$(printf '%s' "$ready" | sed -E 's/.*"url":"([^"]+)".*/\1/')"
actual="$(curl -sf --noproxy '*' -u "opencode:$OPENCODE_PASSWORD" "$url/openapi.json" | sha256sum | cut -d' ' -f1)"
exec 3>&-
wait "$server" 2>/dev/null || true
server=""

if [ "$actual" = "$expected" ]; then
    echo "schema matches stage-0 manifest"
else
    echo "changed schema: $actual, manifest $expected"
    exit 1
fi
