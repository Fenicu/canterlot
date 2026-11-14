# shellcheck shell=bash
# Общий стенд cutover: живые сессии, развёрнутый prev, залитый new, dist цели.
# Подключается тестами cutover/rehearse после lib-harness.sh (файл не исполняемый).
# shellcheck disable=SC2034,SC2015
cargo() { echo "cargo $*" >>"${FAKE_ROOT}/state/calls.log"; return 0; }
export -f cargo
sleep() { case "${1:-}" in 1|2|3|5|60) command sleep 0.1;; *) command sleep "$@";; esac; }
export -f sleep

SHA_NEW=aaaaaaaaaaaa
SHA_PREV=bbbbbbbbbbbb
EXEC1=""
EXEC2=""

write_ownership() {
    mkdir -p "${WORKER_DATA}/ownership"
    python3 - "${WORKER_DATA}/ownership/snapshot.toml" "${EXEC1}" <<'EOF'
import sys
path, live = sys.argv[1], sys.argv[2]
def varint(n):
    out = b""
    while True:
        b = n & 0x7F; n >>= 7
        out += bytes([b | (0x80 if n else 0)])
        if not n: return out
def rec(exec_id, phase):
    b = varint(1 << 3 | 2) + varint(len(exec_id)) + exec_id.encode() + varint(14 << 3 | 0) + varint(phase)
    return "[" + ", ".join(str(x) for x in b) + "]"
dead = "deadbeef-dead-4ead-beef-deadbeefdead"
open(path, "w").write("seq = 0\nrecords = [%s, %s]\n" % (rec(live, 0), rec(dead, 3)))
EOF
}

# dist цели: бинари отвечают на --version/openapi/capabilities, RELEASE с source=,
# csp.txt выведен из 200.html и содержит один wss-origin, SHA256SUMS сходится.
make_cutover_dist() {
    DIST_DIR="${FAKE_ROOT}/dist/${SHA_NEW}"
    mkdir -p "${DIST_DIR}/hub/bin" "${DIST_DIR}/worker/bin" "${DIST_DIR}/synthetic/bin" \
        "${DIST_DIR}/web/_app/immutable/chunks"
    cp "${REPO_ROOT}/docs/api/openapi.json" "${FAKE_ROOT}/state/openapi.json"
    cp "${FAKE_WORKER_BIN}" "${DIST_DIR}/hub/bin/canterlot-hub"
    cp "${FAKE_WORKER_BIN}" "${DIST_DIR}/worker/bin/canterlot-worker"
    cp "${FAKE_WORKER_BIN}" "${DIST_DIR}/synthetic/bin/canterlot-worker"
    printf '<html><script>window.x=1</script></html>\n' >"${DIST_DIR}/web/200.html"
    printf '{"version":"%s"}' "${SHA_NEW}" >"${DIST_DIR}/web/_app/version.json"
    printf 'console.log(1)\n' >"${DIST_DIR}/web/_app/immutable/chunks/probe.js"
    # Сборка precompressed: проба verify выбирает только ассет со всеми соседями.
    for ext in br zst gz; do
        printf 'precompressed\n' >"${DIST_DIR}/web/_app/immutable/chunks/probe.js.${ext}"
    done
    printf 'sha=%s\ndate=x\nmigrations_last=0041_fake_harness.sql\nsource=%s%028d\n' "${SHA_NEW}" "${SHA_NEW}" 0 >"${DIST_DIR}/RELEASE"
    printf "default-src 'self'; script-src '%s'; connect-src wss://hub.example.com" \
        'sha256-hUmDl3yY/7GaaX+Zuy8aEy8IzxgpBs7xccqYHs2/7kQ=' >"${DIST_DIR}/csp.txt"
    dist_resync
}
# Пересчитать SHA256SUMS dist после правки сценарием; копия у хаба — тоже.
dist_resync() {
    (cd "${DIST_DIR}" && find . -type f ! -name 'SHA256SUMS*' -print0 | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS)
    if [ -d "${HUB_RELEASES}/${SHA_NEW}" ]; then cp "${DIST_DIR}/SHA256SUMS" "${HUB_RELEASES}/${SHA_NEW}/SHA256SUMS"; fi
}
stage_cutover_dist() {
    mkdir -p "${HUB_RELEASES}/${SHA_NEW}" "${WEB_RELEASES}/${SHA_NEW}" "${WORKER_RELEASES}/${SHA_NEW}"
    cp -r "${DIST_DIR}/hub/." "${HUB_RELEASES}/${SHA_NEW}/"
    cp "${DIST_DIR}/RELEASE" "${DIST_DIR}/SHA256SUMS" "${HUB_RELEASES}/${SHA_NEW}/"
    cp -r "${DIST_DIR}/web/." "${WEB_RELEASES}/${SHA_NEW}/"
    cp -r "${DIST_DIR}/worker/." "${WORKER_RELEASES}/${SHA_NEW}/"
    cp "${DIST_DIR}/RELEASE" "${WORKER_RELEASES}/${SHA_NEW}/"
}
# dist развёрнутого релиза из его залитых каталогов (shadow): RELEASE с source=,
# SHA256SUMS, копии RELEASE/SHA256SUMS у хаба и воркера; веб берётся как есть.
make_dist_from_staged() {
    local sha="$1" d="${FAKE_ROOT}/dist/$1"
    rm -rf "${d}"
    mkdir -p "${d}/hub" "${d}/worker" "${d}/synthetic/bin"
    cp -r "${HUB_RELEASES}/${sha}/bin" "${d}/hub/"
    cp -r "${WORKER_RELEASES}/${sha}/bin" "${d}/worker/"
    cp "${FAKE_WORKER_BIN}" "${d}/synthetic/bin/canterlot-worker"
    cp -r "${WEB_RELEASES}/${sha}" "${d}/web"
    printf 'sha=%s\ndate=x\nmigrations_last=0040_monthly_limits.sql\nsource=%s%028d\n' "${sha}" "${sha}" 0 >"${d}/RELEASE"
    cp "${DIST_DIR}/csp.txt" "${d}/csp.txt"
    (cd "${d}" && find . -type f ! -name 'SHA256SUMS*' -print0 | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS)
    cp "${d}/RELEASE" "${d}/SHA256SUMS" "${HUB_RELEASES}/${sha}/"
    cp "${d}/RELEASE" "${WORKER_RELEASES}/${sha}/"
}
# Бинарь диктовки целевого релиза лежит и в dist (как после настоящей сборки).
eval "$(declare -f install_fake_speech | sed '1s/install_fake_speech/_harness_install_fake_speech/')"
install_fake_speech() {
    _harness_install_fake_speech "$1"
    if [ "$1" = "${SHA_NEW}" ] && [ -d "${DIST_DIR:-}/hub/bin" ]; then
        cp "${HUB_RELEASES}/$1/bin/canterlot-speech" "${DIST_DIR}/hub/bin/canterlot-speech"
        dist_resync
    fi
}
# Убрать файл хаба цели и из dist, и из залитого каталога (релиз без него).
remove_target_hub_file() {
    rm -f "${HUB_RELEASES}/${SHA_NEW}/$1" "${DIST_DIR}/hub/$1"
    dist_resync
}

# readlink, который при FAKE_READLINK_FAIL_EXE=1 не читает /proc/<pid>/exe (сбой
# чтения посреди preflight, как ENOENT у процесса на переходе exec).
install_exe_readlink_fault() {
    local real
    real="$(command -v readlink)"
    cat >"${FAKE_ROOT}/fake-bin/readlink" <<EOF
#!/usr/bin/env bash
if [ "\${FAKE_READLINK_FAIL_EXE:-0}" = "1" ]; then
    for a in "\$@"; do
        case "\${a}" in
            /proc/*/exe) echo "readlink-fault \${a}" >>"${FAKE_ROOT}/state/calls.log"; echo "readlink: \${a}: No such file or directory" >&2; exit 1 ;;
        esac
    done
fi
exec "${real}" "\$@"
EOF
    chmod +x "${FAKE_ROOT}/fake-bin/readlink"
}

setup_cutover() {
    setup_case "cutover-$1"
    export SITE_CODES="meeting.example.com:200 git.example.com:200 caddy.example.com:302 gotify.example.com:200 immich.example.com:200 bookshelf.example.com:200 hub.example.com:200"
    EXEC1="$(new_exec_id)"; EXEC2="$(new_exec_id)"; export EXEC1 EXEC2
    write_configs
    deploy_release hub "${SHA_PREV}"
    deploy_release web "${SHA_PREV}"
    deploy_release worker "${SHA_PREV}"
    # Развёрнутый хаб — тот же фейковый бинарь, что у цели: shadow-репетиция
    # проверяет развёрнутый релиз тем же target-check (--version, openapi).
    cp "${FAKE_WORKER_BIN}" "${HUB_RELEASES}/${SHA_PREV}/bin/canterlot-hub"
    printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
    cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/state/db-pre.txt"
    printf '%s\n%s\n' "${EXEC1}" "${EXEC2}" >"${FAKE_ROOT}/state/live-ids"
    printf 'aaaaaaaa-0000-4000-8000-000000000001\n' >"${FAKE_ROOT}/state/live-convs"
    export FAKE_HUB_MIGRATE_TO=41 FAKE_MIGR_FAILED_AFTER=0
    export FAKE_READYZ_MODE=byhub FAKE_READYZ_NEW_SHA="${SHA_NEW}" FAKE_READYZ_NEW=1
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-worker >/dev/null
    FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-hub >/dev/null
    spawn_session "${EXEC1}" "${SHA_PREV}" 100
    spawn_session "${EXEC2}" "${SHA_PREV}" 200
    mkdir -p "${WORKER_DATA}/cj"
    python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', 50) + b'\0'*4)" >"${WORKER_DATA}/cj/ack"
    write_ownership
    # Цель: полный dist (его проверяет target-check в общей предпроверке) и
    # каталоги на хостах, залитые из него файл в файл, как после stage.sh.
    make_cutover_dist
    stage_cutover_dist
    printf '{"version":"%s"}' "${SHA_PREV}" >"${FAKE_ROOT}/container/version.json"
    printf 'CSP-BODY-PREV' >"${FAKE_ROOT}/container/csp"
    printf 'CANTERLOT_CSP="CSP-BODY-PREV"\n' >"${WEB_ENV}"
    printf 'CANTERLOT_CSP="CSP-BODY-PREV"\n' >"${WEB_ENV}.${SHA_PREV}"
    chmod 0600 "${WEB_ENV}.${SHA_PREV}"
    SUP1="$(sup_pid_of "${EXEC1}")"; SUP2="$(sup_pid_of "${EXEC2}")"; export SUP1 SUP2
}
confirm_for() { issue_token "$1" "${FAKE_ROOT}/dist"; }
run_cutover() {
    bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "$(confirm_for "${SHA_NEW}")" \
        --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1
}
hub_is() { [ "$(readlink "${HUB_CURRENT}" | sed 's|.*/||')" = "$1" ]; }
web_is() { [ "$(readlink "${WEB_BUILD_LINK}" | sed 's|.*/||')" = "$1" ]; }
worker_is() { [ "$(readlink "${WORKER_CURRENT}" | sed 's|.*/||')" = "$1" ]; }
db_migr_is() {
    [ "$(grep '^MIGR_MAX=' "${FAKE_ROOT}/state/db.txt" | cut -d= -f2)" = "$1" ] \
        && [ "$(grep '^FAILED=' "${FAKE_ROOT}/state/db.txt" | cut -d= -f2)" = "$2" ]
}
dropdb_logged() { grep -q '^dropdb ' "${FAKE_ROOT}/state/calls.log"; }
# Любое восстановление базы: dropdb/createdb или pg_restore не в режимах
# проверки (--list / -f /dev/null). Проверки дампа разрешены, restore — нет.
restore_logged() {
    awk '
        /^dropdb / || /^createdb / { found = 1 }
        /^pg_restore / && !/--list/ && !/-f \/dev\/null/ { found = 1 }
        END { exit found ? 0 : 1 }
    ' "${FAKE_ROOT}/state/calls.log"
}
# Первые вхождения образцов (grep -F) идут в файле строго по порядку.
in_order() {
    local file="$1" prev=0 pat n
    shift
    for pat in "$@"; do
        n="$(grep -nF -- "${pat}" "${file}" | head -1 | cut -d: -f1)"
        if [ -z "${n}" ] || [ "${n}" -le "${prev}" ]; then
            printf 'out of order or missing: [%s] (line %s after %s)\n' "${pat}" "${n:-none}" "${prev}" >&2
            return 1
        fi
        prev="${n}"
    done
}
worker_stopped_ever() { grep -q 'stop canterlot-worker' "${FAKE_ROOT}/state/calls.log"; }
# Значение ключа из файла результата cutover (ключ=значение).
result_of() { sed -n "s/^$2=//p" "$1" 2>/dev/null | head -1; }
pgdump_logged() { grep -q '^pg_dump ' "${FAKE_ROOT}/state/calls.log"; }
compose_up_count() { grep -c "compose up -d caddy" "${FAKE_ROOT}/state/calls.log" || true; }
# set -m: фоновые задания не игнорят INT (иначе trap cutover не сработает в harness).
run_cutover_bg() {
    set -m
    bash "${DEPLOY_DIR}/cutover.sh" --execute "${SHA_NEW}" --confirm "$(confirm_for "${SHA_NEW}")" \
        --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1 &
    CUTOVER_PID=$!
}
wait_for_log() {
    local log="$1" marker="$2" _
    for _ in $(seq 1 1800); do
        grep -q "${marker}" "${log}" 2>/dev/null && return 0
        sleep 0.1
    done
    return 1
}

