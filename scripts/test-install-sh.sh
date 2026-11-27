#!/usr/bin/env bash
# Офлайн-проверки install.sh: локальный HTTP-сервер и поддельная утилита в архиве.
# Внешней сети, docker и реальных путей установки нет.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
install=scripts/install.sh

# Литеральный путь: тест удаляет только свой каталог и только с меткой.
tmp=/tmp/canterlot-install-sh-test
if [[ -e "$tmp" ]]; then
    [[ -f "$tmp/.canterlot-install-sh-test" ]] || {
        echo "FATAL: $tmp занят не этим тестом" >&2
        exit 1
    }
    rm -rf /tmp/canterlot-install-sh-test
fi
mkdir -p "$tmp"
: >"$tmp/.canterlot-install-sh-test"
server_pid=""
trap 'if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true; fi; rm -rf /tmp/canterlot-install-sh-test' EXIT

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
expect() {
    local want=$1 got=$2
    [[ "$got" == "$want" ]] || fail "ожидалось «$want», получено «$got»"
}

serve=$tmp/serve
api=$serve/api/repos/Fenicu/canterlot/releases
assets=$serve/download/v1.2.3
pristine=$tmp/pristine
mkdir -p "$api" "$assets" "$pristine" "$tmp/fake"
# Постороннее поле с похожим именем: разбор должен взять именно tag_name.
printf '{"tag_name": "v1.2.3", "name": "canterlot 1.2.3"}\n' >"$api/latest"

printf '#!/bin/sh\nprintf "%%s\\n" "$@"\n' >"$tmp/fake/canterlot"
chmod 0755 "$tmp/fake/canterlot"
ctl_name=canterlot-1.2.3-x86_64-linux.tar.gz
tar -czf "$pristine/$ctl_name" -C "$tmp/fake" canterlot
(
    cd "$pristine"
    sha256sum -- "$ctl_name" >SHA256SUMS
)
line=$(head -n1 "$pristine/SHA256SUMS")
printf '%s  %s\n' "${line%% *}" canterlot-hub-1.2.3-x86_64-linux-gnu.tar.gz >>"$pristine/SHA256SUMS"

reset_assets() {
    rm -rf "$assets"
    mkdir -p "$assets"
    cp -a "$pristine/." "$assets/"
}

port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$serve" >"$tmp/http.log" 2>&1 &
server_pid=$!
curl --retry 30 --retry-connrefused --retry-delay 0 -fsS -o /dev/null 2>/dev/null \
    "http://127.0.0.1:$port/api/repos/Fenicu/canterlot/releases/latest" || fail "локальный сервер не поднялся"

api_hits() { grep -c 'GET /api/repos/Fenicu/canterlot/releases/latest' "$tmp/http.log" || true; }
run_install() {
    local out=$1 status=0
    shift
    CANTERLOT_API_BASE="http://127.0.0.1:$port/api/repos/Fenicu/canterlot" \
    CANTERLOT_RELEASE_BASE="http://127.0.0.1:$port/download" \
        bash "$install" "$@" >"$out" 2>&1 || status=$?
    return "$status"
}
expect_code() {
    local code=$1
    grep -qxF "install.sh: $code" "$tmp/out" || fail "нет строки install.sh: $code: $(cat "$tmp/out")"
}

# Загруженный код нельзя ни eval, ни source.
if grep -Eq '^[[:space:]]*(eval|source|\.)[[:space:]]' "$install"; then
    fail 'install.sh выполняет загруженный код'
fi

reset_assets
before=$(api_hits)
status=0
run_install "$tmp/out" status || status=$?
expect 0 "$status"
expect "$(printf '%s\n' status --version 1.2.3)" "$(cat "$tmp/out")"
after=$(api_hits)
expect 1 "$((after - before))"
printf 'latest_resolved_once: OK\n'

reset_assets
sed -i 's/canterlot-1\.2\.3-x86_64-linux\.tar\.gz/canterlot-1.2.3-x86_64-linux-gnu.tar.gz/' "$assets/SHA256SUMS"
status=0
run_install "$tmp/out" status || status=$?
expect 1 "$status"
expect_code checksum_missing
printf 'exact_checksum_line_required: OK\n'

reset_assets
printf '\n# испорченный архив\n' >>"$assets/$ctl_name"
status=0
run_install "$tmp/out" status || status=$?
expect 1 "$status"
expect_code checksum_mismatch
printf 'tampered_tarball_refused: OK\n'

reset_assets
cp "$tmp/fake/canterlot" "$tmp/fake/extra"
tar -czf "$assets/$ctl_name" -C "$tmp/fake" canterlot extra
(
    cd "$assets"
    sha256sum -- "$ctl_name" >SHA256SUMS
)
rm -f "$tmp/fake/extra"
status=0
run_install "$tmp/out" status || status=$?
expect 1 "$status"
expect_code unsafe_archive
printf 'extra_entry_in_tarball_refused: OK\n'

reset_assets
before=$(api_hits)
status=0
run_install "$tmp/out" --version 1.2.3 join --hub hub.example.internal:7443 --pin sha256:ab --code - || status=$?
expect 0 "$status"
expect "$(printf '%s\n' join --hub hub.example.internal:7443 --pin sha256:ab --code - --version 1.2.3)" "$(cat "$tmp/out")"
after=$(api_hits)
expect 0 "$((after - before))"
printf 'args_passed_through: OK\n'

printf 'install.sh: все проверки прошли\n'
