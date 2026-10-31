#!/usr/bin/env bash
# Проверка контракта OpenCode: схема работающего сервера против манифеста stage-0.
# Нужен настоящий CLI; каталоги изолированы, запросов к модели нет. Владелец запускает при обновлении CLI.
set -euo pipefail

opencode="${OPENCODE_BIN:-opencode}"
manifest="docs/stage0/evidence/opencode/openapi-2.1.4.sha256"
expected="$(grep 'GET /openapi.json' "$manifest" | cut -d' ' -f1)"
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

echo "cli: $("$opencode" --version)"
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
