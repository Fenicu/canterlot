#!/usr/bin/env bash
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
# Тесты preflight.sh в hermetic-окружении: снапшот, сверки, ownership, pipe-конфиги.
set -euo pipefail
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

cargo() { echo "cargo $*" >>"${FAKE_ROOT}/state/calls.log"; return 0; }
export -f cargo
sleep() { case "${1:-}" in 1|60) command sleep 0.1;; *) command sleep "$@";; esac; }
export -f sleep

SHA_PREV="bbbbbbbbbbbb"
EXEC1=""
EXEC2=""

# Минимальный валидный ownership snapshot: EXEC1 неосвобождён, мёртвая спека освобождена.
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

setup_live() {
    setup_case "preflight-$1"
    EXEC1="$(new_exec_id)"
    EXEC2="$(new_exec_id)"
    export EXEC1 EXEC2
    write_configs
    deploy_release hub "${SHA_PREV}"
    deploy_release web "${SHA_PREV}"
    deploy_release worker "${SHA_PREV}"
    printf 'MIGR_MAX=40\nFAILED=0\npayload\n' >"${FAKE_ROOT}/state/db.txt"
    printf '%s\n%s\n' "${EXEC1}" "${EXEC2}" >"${FAKE_ROOT}/state/live-ids"
    printf 'aaaaaaaa-0000-4000-8000-000000000001\n' >"${FAKE_ROOT}/state/live-convs"
    FAKE_ROOT_BIN="${FAKE_ROOT}/fake-bin" PATH="${FAKE_ROOT}/fake-bin:${PATH}" \
        FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-worker >/dev/null
    FAKE_ROOT_BIN="${FAKE_ROOT}/fake-bin" PATH="${FAKE_ROOT}/fake-bin:${PATH}" \
        FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-hub >/dev/null
    # Крючок теста: процессы-приманки должны появиться ДО настоящих сессий.
    if declare -F pre_spawn_hook >/dev/null; then pre_spawn_hook; fi
    spawn_session "${EXEC1}" "${SHA_PREV}" 100
    spawn_session "${EXEC2}" "${SHA_PREV}" 200
    mkdir -p "${WORKER_DATA}/cj"
    python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', 50) + b'\0'*4)" \
        >"${WORKER_DATA}/cj/ack"
    write_ownership
    printf '{"version":"%s"}' "${SHA_PREV}" >"${FAKE_ROOT}/container/version.json"
    printf 'CSP-BODY' >"${FAKE_ROOT}/container/csp"
    printf 'CANTERLOT_CSP="CSP-BODY"\n' >"${WEB_ENV}"
}

# 1. happy path: снапшот пишется, сверки проходят, конфиги через pipe читаются.
case_begin "preflight happy"
setup_live happy
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/out.log"
fi
assert_contains "${FAKE_ROOT}/out.log" "MATCH 1dbdd180ffce.toml" "spec fixtures matched"
assert_contains "${FAKE_ROOT}/out.log" "config OK against WorkerConfig" "worker config parsed"
assert_contains "${FAKE_ROOT}/out.log" "config OK against HubConfig" "hub config via pipe parsed"
assert_contains "${FAKE_ROOT}/out.log" "ownership OK: 1 unreleased" "ownership decoded, 1 unreleased"
[ -f "${FAKE_ROOT}/snap.state" ] && [ -f "${FAKE_ROOT}/snap.state.procids" ] \
    && case_ok "snapshot files written" || case_fail "snapshot files missing"
# shellcheck disable=SC2015
[ -f "${FAKE_ROOT}/snap.state" ] && grep -q "sup=" "${FAKE_ROOT}/snap.state" \
    && case_ok "snapshot has sup lines" || case_fail "snapshot lacks sup lines"
[ ! -f "/tmp/hub.toml.${SHA_PREV}.preflight" ] && case_ok "no hub.toml temp file" \
    || case_fail "hub.toml temp file leaked"
teardown_case
case_end

# Источник «цели новее репозитория»: те же крейты, но OpenCodeConfig знает provider_models
# (как с 1.4.0). Копируется только config.rs воркера, остальное — ссылки.
make_newer_source() {
    local root="$1" crate
    mkdir -p "${root}/crates/canterlot-worker/src"
    for crate in "${REPO_ROOT}"/crates/*; do
        [ "${crate##*/}" = "canterlot-worker" ] || ln -s "${crate}" "${root}/crates/${crate##*/}"
    done
    ln -s "${REPO_ROOT}/crates/canterlot-worker/tests" "${root}/crates/canterlot-worker/tests"
    for entry in "${REPO_ROOT}"/crates/canterlot-worker/src/*; do
        [ "${entry##*/}" = "config.rs" ] || ln -s "${entry}" "${root}/crates/canterlot-worker/src/${entry##*/}"
    done
    # Текущий исходник уже знает provider_models (1.4.0+); копируем как есть.
    cp "${REPO_ROOT}/crates/canterlot-worker/src/config.rs" "${root}/crates/canterlot-worker/src/config.rs"
    grep -q 'pub provider_models' "${root}/crates/canterlot-worker/src/config.rs"
}

# Источник «старее живой спеки»: тот же config.rs без поля provider_models.
make_older_source() {
    local root="$1" crate
    mkdir -p "${root}/crates/canterlot-worker/src"
    for crate in "${REPO_ROOT}"/crates/*; do
        [ "${crate##*/}" = "canterlot-worker" ] || ln -s "${crate}" "${root}/crates/${crate##*/}"
    done
    ln -s "${REPO_ROOT}/crates/canterlot-worker/tests" "${root}/crates/canterlot-worker/tests"
    for entry in "${REPO_ROOT}"/crates/canterlot-worker/src/*; do
        [ "${entry##*/}" = "config.rs" ] || ln -s "${entry}" "${root}/crates/canterlot-worker/src/${entry##*/}"
    done
    sed '/^    pub provider_models: /d' "${REPO_ROOT}/crates/canterlot-worker/src/config.rs" \
        >"${root}/crates/canterlot-worker/src/config.rs"
    ! grep -q 'pub provider_models:' "${root}/crates/canterlot-worker/src/config.rs"
}

# 1b. необязательный ключ, известный цели (opencode.provider_models, 1.4.0), форму не ломает.
case_begin "preflight spec with an optional key known to the target"
setup_live optkey
printf '\n[opencode.provider_models]\nopencode = ["m1"]\n' >>"${WORKER_DATA}/executions/${EXEC1}/spec.toml"
make_newer_source "${FAKE_ROOT}/newsrc" || case_fail "newer source not built"
make_older_source "${FAKE_ROOT}/oldsrc" || case_fail "older source not built"
# Источник без ключа (старее живой спеки) обязан отказать: допустимость выводится из кода цели.
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${FAKE_ROOT}/oldsrc" >"${FAKE_ROOT}/old.log" 2>&1; then
    case_fail "exit 0 although the target does not know the key"
else
    case_ok "target without the key refuses the spec"
fi
assert_contains "${FAKE_ROOT}/old.log" "unknown key 'opencode.provider_models'" "refusal names the key"
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${FAKE_ROOT}/newsrc" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/out.log"
fi
assert_contains "${FAKE_ROOT}/out.log" "optional in target: opencode.provider_models" "optional key accepted and named"
assert_not_contains "${FAKE_ROOT}/out.log" "m1" "map values are never printed"
teardown_case
case_end

# 1c. ключ, неизвестный цели, по-прежнему провал, и значение не печатается.
case_begin "preflight spec with a key the target does not know"
setup_live badkey
{ printf 'retired_key_zz = "SECRET-VALUE-1"\n'; cat "${WORKER_DATA}/executions/${EXEC1}/spec.toml"; } >"${FAKE_ROOT}/spec.new"
cp "${FAKE_ROOT}/spec.new" "${WORKER_DATA}/executions/${EXEC1}/spec.toml"
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_fail "exit 0 with an unknown spec key"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/out.log" "matches no known fixture shape" "unknown key reported"
assert_contains "${FAKE_ROOT}/out.log" "retired_key_zz" "key name reported"
assert_not_contains "${FAKE_ROOT}/out.log" "SECRET-VALUE-1" "value not printed"
teardown_case
case_end

# 2. супервизор без строки хаба: жёсткая ошибка.
case_begin "supervisor without hub row"
setup_live norow
printf '%s\n' "${EXEC1}" >"${FAKE_ROOT}/state/live-ids"
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_fail "exit 0 despite orphan supervisor"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/out.log" "no live hub row" "orphan supervisor reported"
teardown_case
case_end

# 2b. сопровождение тестового воркера из worktree под WORKER_DATA (стек-тест
# делегата): его exec-dir не прямой каталог WORKER_DATA/executions/<id>, и
# preflight не принимает его за прод-сессию без строки хаба.
case_begin "nested test-stack supervisor is not a production session"
setup_live nestedsup
NESTED_ID="$(new_exec_id)"
NESTED_BIN="${WORKER_DATA}/worktrees/0000/target/debug/canterlot-worker"
NESTED_DIR="${WORKER_DATA}/worktrees/0000/stack/executions/${NESTED_ID}"
mkdir -p "${NESTED_BIN%/*}" "${NESTED_DIR}"
cp "${FAKE_WORKER_BIN}" "${NESTED_BIN}"
setsid bash -c "exec -a '${NESTED_BIN}' '${NESTED_BIN}' supervise --exec-dir '${NESTED_DIR}'" >/dev/null 2>&1 <&- &
HARNESS_PIDS+=("$!")
harness_wait_supervisor "${NESTED_DIR}"
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_ok "exit 0 with a nested test-stack supervisor"
else
    case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/out.log"
fi
assert_not_contains "${FAKE_ROOT}/out.log" "${NESTED_ID}" "nested supervisor not listed as a production session"
assert_contains "${FAKE_ROOT}/out.log" "supervise: ${EXEC1}" "production supervisors still listed"
teardown_case
case_end

# 3. битый ownership журнал: жёсткая ошибка декодирования.
case_begin "corrupt ownership"
setup_live corrupt
printf 'seq = 0\nrecords = [[999]]\n' >"${WORKER_DATA}/ownership/snapshot.toml"
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_fail "exit 0 despite corrupt ownership"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/out.log" "ownership journal decode failed" "decode failure reported"
teardown_case
case_end

# 4. неизвестный ключ в hub.toml: жёсткая ошибка (ловит и таблицы с отступом).
case_begin "unknown hub key"
setup_live badkey
sed -i 's/^\[http\]/  [http]/' "${HUB_CONFIG}"
printf '  bogus_indented = 1\n' >>"${HUB_CONFIG}"
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_fail "exit 0 despite unknown key"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/out.log" "unknown key" "unknown key reported"
teardown_case
case_end

# 4b. провал запроса compactions — жёстко, а не «таблицы нет».
case_begin "compactions query fails"
setup_live compactionsfail
export FAKE_PSQL_FAIL_MATCH="information_schema.tables"
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_fail "exit 0 despite failed compactions query"
else
    case_ok "exit nonzero"
fi
unset FAKE_PSQL_FAIL_MATCH
assert_contains "${FAKE_ROOT}/out.log" "cannot query compactions table" "query failure reported"
assert_not_contains "${FAKE_ROOT}/out.log" "no compactions table yet" "not treated as a missing table"
teardown_case
case_end

# 5. процесс-пустышка с exec id в командной строке не затмевает сопровождение.
case_begin "supervisor not shadowed by decoy"
setup_case decoy
export SITE_CODES="a.example:200 b.example:302"
write_configs
EXEC1="$(new_exec_id)"; export EXEC1
deploy_release hub "bbbbbbbbbbbb"
deploy_release web "bbbbbbbbbbbb"
deploy_release worker "bbbbbbbbbbbb"
printf 'MIGR_MAX=40\nFAILED=0\npayload\n' >"${FAKE_ROOT}/state/db.txt"
printf '%s\n' "${EXEC1}" >"${FAKE_ROOT}/state/live-ids"
printf 'aaaaaaaa-0000-4000-8000-000000000001\n' >"${FAKE_ROOT}/state/live-convs"
FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-worker >/dev/null
# Пустышка раньше сопровождения (меньший PID): её командная строка содержит exec id,
# как профиль playwright у живого агента. Без 'grep supervise' она бы выиграла head -1.
cp /bin/sleep "${FAKE_ROOT}/fake-bin/decoy-sleep" && chmod +x "${FAKE_ROOT}/fake-bin/decoy-sleep"
setsid bash -c "exec -a 'profile-holder ${EXEC1}' ${FAKE_ROOT}/fake-bin/decoy-sleep 1000" >/dev/null 2>&1 <&- &
HARNESS_PIDS+=("$!")
mkdir -p "${WORKER_DATA}/executions/${EXEC1}/sj"
cp "${REPO_ROOT}/crates/canterlot-worker/tests/fixtures/spec/1dbdd180ffce.toml" \
    "${WORKER_DATA}/executions/${EXEC1}/spec.toml"
spawn_session "${EXEC1}" "bbbbbbbbbbbb" 100
mkdir -p "${WORKER_DATA}/cj"
python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', 50) + b'\0'*4)" >"${WORKER_DATA}/cj/ack"
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
open(path, "w").write("seq = 0\nrecords = [%s]\n" % rec(live, 0))
EOF
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_ok "exit 0 despite decoy"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/out.log"
fi
SUP_EXE="$(grep -o 'sup_exe=[^ ]*' "${FAKE_ROOT}/snap.state" | cut -d= -f2)"
case "${SUP_EXE}" in
    *"/bin/canterlot-worker") case_ok "supervisor identified, not the decoy" ;;
    *) case_fail "wrong process snapshotted as supervisor: ${SUP_EXE}" ;;
esac
teardown_case
case_end

# 6. config-check понимает serde-атрибуты поля, ownership-check — диапазоны.
case_begin "checkers honour serde attrs and integer ranges"
setup_case checkers
mkdir -p "${FAKE_ROOT}/check"
cat >"${FAKE_ROOT}/check/structs.rs" <<'EOF'
#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Root {
    #[serde(rename = "renamed")]
    pub original: String,
    #[serde(alias = "legacy")]
    pub canonical: String,
    #[serde(skip, default)]
    pub hidden: String,
    #[serde(flatten)]
    pub extra: Extra,
}
#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extra {
    pub extra_key: String,
}
EOF
printf 'renamed = "a"\ncanonical = "b"\nextra_key = "c"\n' >"${FAKE_ROOT}/check/valid.toml"
printf 'renamed = "a"\nlegacy = "b"\nextra_key = "c"\n' >"${FAKE_ROOT}/check/alias.toml"
printf 'renamed = "a"\ncanonical = "b"\nextra_key = "c"\nhidden = "nope"\n' >"${FAKE_ROOT}/check/skip.toml"
printf 'original = "a"\ncanonical = "b"\nextra_key = "c"\n' >"${FAKE_ROOT}/check/oldname.toml"
if python3 "${DEPLOY_DIR}/config-check.py" "${FAKE_ROOT}/check/structs.rs" Root "${FAKE_ROOT}/check/valid.toml" >/dev/null 2>&1; then
    case_ok "rename + flatten accepted"
else
    case_fail "rename/flatten valid config rejected"
fi
if python3 "${DEPLOY_DIR}/config-check.py" "${FAKE_ROOT}/check/structs.rs" Root "${FAKE_ROOT}/check/alias.toml" >/dev/null 2>&1; then
    case_ok "alias accepted"
else
    case_fail "alias rejected"
fi
if python3 "${DEPLOY_DIR}/config-check.py" "${FAKE_ROOT}/check/structs.rs" Root "${FAKE_ROOT}/check/skip.toml" >"${FAKE_ROOT}/check/skip.log" 2>&1; then
    case_fail "serde(skip) field accepted"
else
    case_ok "serde(skip) field rejected"
fi
assert_contains "${FAKE_ROOT}/check/skip.log" "unknown key 'hidden'" "skip field reported unknown"
if python3 "${DEPLOY_DIR}/config-check.py" "${FAKE_ROOT}/check/structs.rs" Root "${FAKE_ROOT}/check/oldname.toml" >/dev/null 2>&1; then
    case_fail "unrenamed field accepted"
else
    case_ok "unrenamed field rejected"
fi
mkdir -p "${FAKE_ROOT}/own-neg/ownership"
printf 'seq = -1\nrecords = []\n' >"${FAKE_ROOT}/own-neg/ownership/snapshot.toml"
if python3 "${DEPLOY_DIR}/ownership-check.py" \
        "${REPO_ROOT}/crates/canterlot-proto/proto/canterlot/worker/v1/ipc.proto" \
        "${FAKE_ROOT}/own-neg" >"${FAKE_ROOT}/check/own-neg.log" 2>&1; then
    case_fail "negative ownership seq accepted"
else
    case_ok "negative ownership seq rejected"
fi
assert_contains "${FAKE_ROOT}/check/own-neg.log" "seq is not a u64" "negative seq reported"
mkdir -p "${FAKE_ROOT}/own-range/ownership"
python3 - "${FAKE_ROOT}/own-range/ownership/snapshot.toml" <<'PYEOF'
import sys
def varint(n):
    out = b""
    while True:
        b = n & 0x7F; n >>= 7
        out += bytes([b | (0x80 if n else 0)])
        if not n: return out
def rec(eid, pid, phase):
    return (varint(1 << 3 | 2) + varint(len(eid)) + eid.encode()
            + varint(6 << 3 | 0) + varint(pid)
            + varint(14 << 3 | 0) + varint(phase))
blob = rec("11111111-1111-4111-8111-111111111111", 2**32, 0)
open(sys.argv[1], "w").write("seq = 0\nrecords = [%s]\n" % list(blob))
PYEOF
if python3 "${DEPLOY_DIR}/ownership-check.py" \
        "${REPO_ROOT}/crates/canterlot-proto/proto/canterlot/worker/v1/ipc.proto" \
        "${FAKE_ROOT}/own-range" >"${FAKE_ROOT}/check/own-range.log" 2>&1; then
    case_fail "uint32 overflow accepted"
else
    case_ok "uint32 overflow rejected"
fi
assert_contains "${FAKE_ROOT}/check/own-range.log" "outside uint32 range" "uint32 range reported"

# Имя сегмента несёт first_seq и обязано совпасть с первым рекордом.
mkdir -p "${FAKE_ROOT}/own-seg/ownership"
python3 - "${FAKE_ROOT}/own-seg/ownership" <<'PYEOF'
import os
import struct
import sys

def crc32c(data, crc=0):
    poly = 0x82F63B78
    table = []
    for i in range(256):
        c = i
        for _ in range(8):
            c = (c >> 1) ^ poly if c & 1 else c >> 1
        table.append(c & 0xFFFFFFFF)
    crc ^= 0xFFFFFFFF
    for byte in data:
        crc = table[(crc ^ byte) & 0xFF] ^ (crc >> 8)
    return (crc ^ 0xFFFFFFFF) & 0xFFFFFFFF

def checksum(seq, payload):
    return crc32c(payload, crc32c(struct.pack("<Q", seq)))

def varint(n):
    out = b""
    while True:
        b = n & 0x7F
        n >>= 7
        out += bytes([b | (0x80 if n else 0)])
        if not n:
            return out

eid = b"22222222-2222-4222-8222-222222222222"
payload = (varint(1 << 3 | 2) + varint(len(eid)) + eid
           + varint(14 << 3 | 0) + varint(0))
seq = 6
record = struct.pack("<IIQ", len(payload), checksum(seq, payload), seq) + payload
# Имя говорит first_seq=5, а первый рекорд — 6: без сверки имени это проходит.
open(os.path.join(sys.argv[1], "seg-00000000000000000005.log"), "wb").write(record)
open(os.path.join(sys.argv[1], "snapshot.toml"), "w").write("seq = 5\nrecords = []\n")
PYEOF
if python3 "${DEPLOY_DIR}/ownership-check.py" \
        "${REPO_ROOT}/crates/canterlot-proto/proto/canterlot/worker/v1/ipc.proto" \
        "${FAKE_ROOT}/own-seg" >"${FAKE_ROOT}/check/own-seg.log" 2>&1; then
    case_fail "segment first-seq mismatch accepted"
else
    case_ok "segment first-seq mismatch rejected"
fi
assert_contains "${FAKE_ROOT}/check/own-seg.log" "name says 5" "segment name mismatch reported"

# Настоящий WorkerConfig: serde(skip) у [codex] owner_home означает, что в TOML
# этого ключа нет — он обязан падать так же, как у бинаря.
write_configs
cp "${WORKER_CONFIG}" "${FAKE_ROOT}/check/worker-valid.toml"
printf '\n[codex]\nhome_dir = "/tmp/codex-home"\n' >>"${FAKE_ROOT}/check/worker-valid.toml"
if python3 "${DEPLOY_DIR}/config-check.py" \
        "${REPO_ROOT}/crates/canterlot-worker/src/config.rs" WorkerConfig \
        "${FAKE_ROOT}/check/worker-valid.toml" >/dev/null 2>&1; then
    case_ok "real WorkerConfig accepts a known [codex] key"
else
    case_fail "real WorkerConfig rejected a known [codex] key"
fi
cp "${WORKER_CONFIG}" "${FAKE_ROOT}/check/worker-ownerhome.toml"
printf '\n[codex]\nowner_home = "/tmp/owner-codex"\n' >>"${FAKE_ROOT}/check/worker-ownerhome.toml"
if python3 "${DEPLOY_DIR}/config-check.py" \
        "${REPO_ROOT}/crates/canterlot-worker/src/config.rs" WorkerConfig \
        "${FAKE_ROOT}/check/worker-ownerhome.toml" >"${FAKE_ROOT}/check/ownerhome.log" 2>&1; then
    case_fail "serde(skip) worker key codex.owner_home accepted"
else
    case_ok "serde(skip) worker key codex.owner_home rejected"
fi
assert_contains "${FAKE_ROOT}/check/ownerhome.log" "unknown key 'codex.owner_home'" "[codex] owner_home reported unknown"
teardown_case
case_end

# 8. имя развёрнутого релиза с хоста с shell-метасимволами: громкий отказ,
# ничего не исполняется ни в preflight, ни в verify/retain/cutover --dry-run.
case_begin "hostile deployed release name from a host"
setup_live hostilecurrent
ln -sfn "releases/';>PWN;#xxxx" "${HUB_CURRENT}"
if ( cd "${FAKE_ROOT}" && bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" ) >"${FAKE_ROOT}/out.log" 2>&1; then
    case_fail "preflight passed with a hostile deployed release name"
else
    case_ok "preflight exit nonzero"
fi
assert_contains "${FAKE_ROOT}/out.log" "invalid release id" "invalid release id reported"
[ ! -e "${FAKE_ROOT}/PWN" ] && case_ok "preflight injected nothing" || case_fail "preflight executed the release name"
rm -f "${FAKE_ROOT}/PWN"
for tool in "verify.sh --dry-run" "retain.sh" "cutover.sh --dry-run"; do
    # shellcheck disable=SC2086
    if ( cd "${FAKE_ROOT}" && bash "${DEPLOY_DIR}/${tool%% *}" ${tool#* } ) >"${FAKE_ROOT}/out2.log" 2>&1 && [ "${tool%% *}" != "retain.sh" ]; then
        case_fail "${tool} passed with a hostile release name"
    elif [ "${tool%% *}" = "retain.sh" ] && ( cd "${FAKE_ROOT}" && bash "${DEPLOY_DIR}/retain.sh" ) >"${FAKE_ROOT}/out2.log" 2>&1; then
        case_fail "retain passed with a hostile release name"
    else
        case_ok "${tool} refused"
    fi
    [ ! -e "${FAKE_ROOT}/PWN" ] && case_ok "${tool} injected nothing" || case_fail "${tool} executed the release name"
    rm -f "${FAKE_ROOT}/PWN"
done
teardown_case
case_end

# 9. приманка со словом mcp-relay и exec id в argv раньше настоящего релея:
# снимок обязан держать настоящий релей (точная подкоманда), а не приманку.
case_begin "relay snapshot is exact, not a substring match"
pre_spawn_hook() {
    # Три приманки: подстрока mcp-relay + exec id, но argv[1] — не mcp-relay.
    for n in 1 2 3; do
        setsid bash -c "exec -a 'decoy-mcp-relay${n} ${WORKER_DATA}/executions/${EXEC1}/mcp.sock' sleep 1000" >/dev/null 2>&1 <&- &
        HARNESS_PIDS+=("$!")
    done
    sleep 0.3
}
setup_live relaydecoy
unset -f pre_spawn_hook
REAL_RELAY="$(ps -eo pid=,args= | awk -v id="${EXEC1}" '$3 == "mcp-relay" && index($0, id) {print $1}' | head -1)"
DECOY_PIDS="$(ps -eo pid=,args= | awk -v id="${EXEC1}" '$2 ~ /^decoy-mcp-relay/ && index($0, id) {print $1}' | tr '\n' ' ')"
[ -n "${REAL_RELAY}" ] && [ -n "${DECOY_PIDS}" ] && case_ok "real relay and decoys exist" || case_fail "setup: relay=${REAL_RELAY:-none} decoys=${DECOY_PIDS:-none}"
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_ok "preflight exit 0"
else
    case_fail "preflight exit nonzero"; tail -5 "${FAKE_ROOT}/out.log"
fi
SNAP_LINE="$(grep "^${EXEC1} " "${FAKE_ROOT}/snap.state" || true)"
case "${SNAP_LINE}" in
    *"relay=${REAL_RELAY}:"*) case_ok "snapshot holds the real relay ${REAL_RELAY}" ;;
    *) case_fail "snapshot lacks the real relay ${REAL_RELAY}: ${SNAP_LINE}" ;;
esac
for d in ${DECOY_PIDS}; do
    case "${SNAP_LINE}" in
        *"relay=${d}:"*|*",${d}:"*) case_fail "decoy ${d} recorded as a relay" ;;
        *) ;;
    esac
done
# Убитый настоящий релей обязан завалить verify по снимку (приманки живы).
kill "${REAL_RELAY}" 2>/dev/null || true
sleep 0.3
if bash "${DEPLOY_DIR}/verify.sh" "${SHA_PREV}" --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/verify.log" 2>&1; then
    case_fail "verify passed after the real relay was killed"
else
    case_ok "verify failed after the real relay was killed"
fi
assert_contains "${FAKE_ROOT}/verify.log" "session process gone" "killed relay detected via the snapshot"
teardown_case
case_end

# 14. нода CT: настоящий CT 130 живёт на bigboy. Умолчание deploy.conf обязано
# совпадать с ней (раньше было proxmox-main: pvesh падал на hostname lookup), а
# заданная вручную чужая нода — громкая ошибка с названием настоящей.
case_begin "PROXMOX_NODE default matches the real node and a disagreeing one fails loudly"
setup_live pvenode
unset PROXMOX_NODE
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_ok "deploy.conf default node: exit 0"
else
    case_fail "deploy.conf default node: exit nonzero"; grep -E 'HARD|FATAL|hostname' "${FAKE_ROOT}/out.log" | head -3
fi
assert_contains "${FAKE_ROOT}/out.log" "CT 130 runs on node bigboy" "node discovered from the cluster resources"
assert_contains "${FAKE_ROOT}/out.log" "no CT lock" "lock queried on the discovered node"
export PROXMOX_NODE=proxmox-main
if bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1; then
    case_fail "exit 0 although PROXMOX_NODE disagrees with the real node"
else
    case_ok "disagreeing PROXMOX_NODE: exit nonzero"
fi
assert_contains "${FAKE_ROOT}/out.log" "PROXMOX_NODE=proxmox-main but CT 130 runs on node bigboy" "both names reported"
export PROXMOX_NODE=bigboy
FAKE_PVES_FAIL=1 bash "${DEPLOY_DIR}/preflight.sh" "${SHA_PREV}" --source "${REPO_ROOT}" >"${FAKE_ROOT}/out.log" 2>&1 \
    && case_fail "exit 0 although the node discovery failed" || case_ok "failed discovery: exit nonzero"
assert_contains "${FAKE_ROOT}/out.log" "cannot discover the node of CT 130" "discovery failure reported"
teardown_case
case_end

printf 'preflight harness: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
