#!/usr/bin/env bash
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
# Общая обвязка тестов scripts/deploy: hermetic FAKE_ROOT, подменённые ssh и
# демоны, настоящие процессы сессий с подменённым argv[0].
# Использование: source lib-harness.sh; setup_case <name>; ...; teardown_case
# Тесты никогда не трогают прод: fake ssh отказывает настоящим хостам.
# shellcheck disable=SC2034
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"

FAKE_ROOT=""
HARNESS_PIDS=()

harness_log() { printf '[harness] %s\n' "$*"; }

# Детерминированные ворота «вне окна бэкапа»: тесты не должны зависеть от того,
# запущен ли прогон между 02:45 и 03:45 UTC. Остальные вызовы date — настоящие.
date() {
    if [ "$#" -eq 2 ] && [ "$1" = "-u" ] && [ "$2" = "+%H:%M" ]; then
        printf '%s\n' "${HARNESS_UTC_HOUR:-12:00}"
    else
        command date "$@"
    fi
}
export -f date

# Бинарь фейкового воркера собирается один раз на прогон (а не на кейс): cc на
# каждом кейсе стоил секунды. Игнорирует argv (как демон/supervise/relay) и живёт
# до сигнала; `capabilities` печатает список, как настоящий воркер (или файл
# $FAKE_ROOT/state/worker-capabilities, если сценарий его положил).
fake_worker_build() {
    local cache uid
    uid="$(id -u)"
    cache="${HARNESS_CACHE_DIR:-/tmp}/canterlot-fake-worker-v2.${uid}"
    if [ ! -x "${cache}" ]; then
        cat >"${cache}.c" <<'EOF'
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "capabilities") == 0) {
        const char *root = getenv("FAKE_ROOT");
        char path[4096];
        if (root) {
            snprintf(path, sizeof path, "%s/state/worker-capabilities", root);
            FILE *f = fopen(path, "r");
            if (f) { int c; while ((c = fgetc(f)) != EOF) putchar(c); fclose(f); return 0; }
        }
        puts("capability claude_histories_v1");
        puts("capability path_probe_v1");
        puts("harness claude_code");
        puts("harness codex");
        puts("harness opencode");
        puts("harness antigravity");
        return 0;
    }
    for (;;) pause();
}
EOF
        cc -O0 -o "${cache}.tmp.$$" "${cache}.c" && mv -f "${cache}.tmp.$$" "${cache}"
    fi
    printf '%s' "${cache}"
}

# Токен подтверждения для боевого прогона в тестах: тот же план, что посчитает
# cutover.sh (lib.sh в отдельном bash, чтобы set -e библиотеки не попал в тест).
# $1 релиз, $2 каталог dist (база), $3 — epoch (по умолчанию сейчас).
issue_token() {
    bash -c 'source "$1/lib.sh"
        ep="${4:-$(date -u +%s)}"
        ph="$(plan_hash "$2" "$3/$2" "$(deployed_sha hub)" "$(deployed_sha web)" "$(deployed_sha worker)")"
        confirm_token "$2" "${ph}" "${ep}"' _ "${DEPLOY_DIR}" "$1" "$2" "${3:-}"
}

setup_case() {
    FAKE_ROOT="$(mktemp -d /tmp/canterlot-deploy-test.XXXXXX)"
    export FAKE_ROOT
    HARNESS_PIDS=()
    mkdir -p "${FAKE_ROOT}/fake-bin" "${FAKE_ROOT}/state" "${FAKE_ROOT}/tmp"
    cp "${HARNESS_DIR}"/fake-bin/* "${FAKE_ROOT}/fake-bin/" 2>/dev/null || true
    chmod +x "${FAKE_ROOT}"/fake-bin/*
    export PATH="${FAKE_ROOT}/fake-bin:${PATH}"
    export TMPDIR="${FAKE_ROOT}/tmp"
    mkdir -p "${FAKE_ROOT}/hub/opt/canterlot/releases" "${FAKE_ROOT}/hub/etc/canterlot"
    mkdir -p "${FAKE_ROOT}/hub/backups" "${FAKE_ROOT}/web/sites/canterlot/releases"
    mkdir -p "${FAKE_ROOT}/web" "${FAKE_ROOT}/container" "${FAKE_ROOT}/worker/opt/canterlot/releases"
    mkdir -p "${FAKE_ROOT}/worker/config" "${FAKE_ROOT}/worker/data" "${FAKE_ROOT}/dist"
    # Hermetic-скоуп для проверки процессов воркера: на машине воркера живёт
    # настоящий прод-воркер, и rollback-db не должен считать его участником
    # фейкового прогона. Прод-код без этой переменной сканирует /proc целиком.
    export WORKER_PROC_GUARD_ROOT="${FAKE_ROOT}"
    : >"${FAKE_ROOT}/state/calls.log"

    # Настоящий worker-процесс в тестах: отдельный бинарь, который игнорирует
    # argv (как демон/supervise/relay) и живёт до сигнала. Нужен, чтобы argv[1]
    # у fake-процессов был подкомандой, а /proc/<pid>/exe указывал на релиз.
    FAKE_WORKER_BIN="${FAKE_ROOT}/fake-worker"
    export FAKE_WORKER_BIN
    cp "$(fake_worker_build)" "${FAKE_WORKER_BIN}"
    chmod 0755 "${FAKE_WORKER_BIN}"
    # Дамп в тестах крошечный: порог «подозрительно маленького» дампа снижен.
    export DUMP_MIN_BYTES=1
    # Ожидания каталога и переподключения в тестах короче боевых.
    export CATALOG_WAIT="${CATALOG_WAIT:-3}" CONNECT_WAIT="${CONNECT_WAIT:-10}"

    # Конфиг скриптов — на фейковые хосты и пути.
    export HUB_SSH=fakehub WEB_SSH=fakeweb PROXMOX_SSH=fakeproxmox PROXMOX_NODE=bigboy
    export HUB_CTID=130 HUB_IP=10.0.40.64
    export HUB_BASE="${FAKE_ROOT}/hub/opt/canterlot"
    export HUB_RELEASES="${FAKE_ROOT}/hub/opt/canterlot/releases"
    export HUB_CURRENT="${FAKE_ROOT}/hub/opt/canterlot/current"
    export HUB_CONFIG="${FAKE_ROOT}/hub/etc/canterlot/hub.toml"
    export HUB_UNIT=canterlot-hub
    export HUB_BACKUP_DIR="${FAKE_ROOT}/hub/backups"
    export HUB_PG_SOCKET=/var/run/postgresql HUB_PG_DB=canterlot HUB_PG_USER=canterlot
    export PBS_STORAGE=backup_storage
    export SITE_ORIGIN=https://hub.fenicu.com WSS_ORIGIN=wss://hub.fenicu.com
    export WEB_SITES_BASE="${FAKE_ROOT}/web/sites/canterlot"
    export WEB_RELEASES="${FAKE_ROOT}/web/sites/canterlot/releases"
    export WEB_BUILD_LINK="${FAKE_ROOT}/web/sites/canterlot/build"
    export WEB_ENV="${FAKE_ROOT}/web/canterlot.env"
    export WEB_COMPOSE_DIR="${FAKE_ROOT}/web/compose"
    export WEB_CONTAINER=caddy
    export WEB_CONTAINER_VERSION_JSON=/srv/canterlot/web/build/_app/version.json
    export WORKER_BASE="${FAKE_ROOT}/worker/opt/canterlot"
    export WORKER_RELEASES="${FAKE_ROOT}/worker/opt/canterlot/releases"
    export WORKER_CURRENT="${FAKE_ROOT}/worker/opt/canterlot/current"
    export WORKER_CONFIG="${FAKE_ROOT}/worker/config/worker.toml"
    export WORKER_DATA="${FAKE_ROOT}/worker/data"
    export WORKER_UNIT=canterlot-worker
    export WORKER_SOCKET_DIR="${FAKE_ROOT}/worker/run/canterlot"
    # Юнит воркера установлен локально, как в документированной установке
    # (~/.config/systemd/user/canterlot-worker.service): настоящий systemd отказывает
    # `mask` поверх существующего файла юнита, и фейк обязан это воспроизводить.
    export WORKER_UNIT_DIR="${FAKE_ROOT}/worker/unit-dir"
    mkdir -p "${WORKER_UNIT_DIR}"
    printf '[Unit]\nDescription=fake canterlot-worker\n[Service]\nKillMode=process\nExecStart=/bin/true\n' >"${WORKER_UNIT_DIR}/canterlot-worker.service"
    export SITE_CODES="a.example:200 b.example:302"
    XDG_RUNTIME_DIR="/run/user/$(id -u)"
    export XDG_RUNTIME_DIR
}

# Минимальные валидные конфиги: только известные ключи (проверяется config-check.py
# против настоящих config.rs).
write_configs() {
    cat >"${HUB_CONFIG}" <<'EOF'
database_url_file = "database-url"
grpc_listen = "10.0.40.64:7443"
tls_cert = "tls/hub.pem"
tls_key = "tls/hub.key"
manifest = "/var/lib/canterlot-manifest/installation.toml"

[http]
listen = "10.0.40.64:8080"
trusted_proxies = ["10.10.40.3/32"]
owner_login = "fenicu"
allowed_origins = ["https://hub.fenicu.com"]
proxy_secret_file = "proxy-secret"
logout_url = "https://auth.fenicu.com/logout"
EOF
    cat >"${WORKER_CONFIG}" <<'EOF'
hub_url = "https://10.0.40.64:7443"
ca_cert = "/tmp/fake-ca.pem"
token_file = "/tmp/fake-token"
machine_id = "01a18e6d-87d5-7217-9425-60460504f09d"
installation_id = "01a18c86-e8a2-74da-86da-26eaa3fe6eaa"
restore_generation = "01a18c86-e8a2-74da-86da-a040f2ac7f5f"
data_dir = "/tmp/fake-data"

[execution]
allowed_workdir_roots = ["/"]
EOF
    mkdir -p "${FAKE_ROOT}/web/compose"
}

# Деплоенный релиз компонента: каталог releases/<sha> + ссылка current/build.
deploy_release() {
    local component="$1" sha="$2"
    case "${component}" in
        hub)
            mkdir -p "${HUB_RELEASES}/${sha}/bin"
            cp /bin/sleep "${HUB_RELEASES}/${sha}/bin/canterlot-hub"
            printf 'sha=%s\ndate=x\nmigrations_last=0040_monthly_limits.sql\n' "${sha}" >"${HUB_RELEASES}/${sha}/RELEASE"
            printf 'sha256-placeholder\n' >"${HUB_RELEASES}/${sha}/SHA256SUMS"
            ln -sfn "releases/${sha}" "${HUB_CURRENT}"
            ;;
        web)
            mkdir -p "${WEB_RELEASES}/${sha}/_app/immutable/chunks"
            printf '{"version":"%s"}' "${sha}" >"${WEB_RELEASES}/${sha}/_app/version.json"
            # Ассет нужен precompressed-проверке verify (в т.ч. после PWA flip-back).
            printf 'console.log(1)\n' >"${WEB_RELEASES}/${sha}/_app/immutable/chunks/probe.js"
            ln -sfn "releases/${sha}" "${WEB_BUILD_LINK}"
            ;;
        worker)
            mkdir -p "${WORKER_RELEASES}/${sha}/bin"
            cp "${FAKE_WORKER_BIN}" "${WORKER_RELEASES}/${sha}/bin/canterlot-worker"
            printf 'sha=%s\ndate=x\nmigrations_last=0040_monthly_limits.sql\n' "${sha}" >"${WORKER_RELEASES}/${sha}/RELEASE"
            ln -sfn "releases/${sha}" "${WORKER_CURRENT}"
            ;;
    esac
}

# Живая сессия: настоящие процессы (supervisor + CLI + relay) с подменённым argv[0].
# exe лежат в releases/<rel>/bin, тики настоящие из /proc.
spawn_session() {
    local exec_id="$1" rel="$2" cursor="${3:-100}"
    local bindir="${WORKER_RELEASES}/${rel}/bin"
    mkdir -p "${bindir}" "${WORKER_DATA}/executions/${exec_id}/sj"
    [ -x "${bindir}/canterlot-worker" ] || cp "${FAKE_WORKER_BIN}" "${bindir}/canterlot-worker"
    [ -x "${bindir}/sleep-cli" ] || cp /bin/sleep "${bindir}/sleep-cli"
    cp "${REPO_ROOT}/crates/canterlot-worker/tests/fixtures/spec/1dbdd180ffce.toml" \
        "${WORKER_DATA}/executions/${exec_id}/spec.toml"
    python3 -c "import struct,sys; sys.stdout.buffer.write(struct.pack('<Q', ${cursor}) + b'\0'*4)" \
        >"${WORKER_DATA}/executions/${exec_id}/sj/ack"
    # argv[0] — путь бинаря, argv[1] — подкоманда, как в проде. Иначе проверка
    # процессов в rollback-db.sh видела бы не тот argv и ошибалась.
    # SESSION_TOOL_CHURN=1: CLI как у настоящего агента с инструментами — стабильный
    # процесс, у которого постоянно сменяются короткоживущие дочерние (вызовы инструментов).
    local cli_cmd="( exec -a 'opencode --session ${exec_id}' '${bindir}/sleep-cli' 1000 ) &"
    if [ "${SESSION_TOOL_CHURN:-0}" = "1" ]; then
        cli_cmd="( exec -a 'opencode --session ${exec_id}' bash -c 'while :; do /bin/sleep 0.4 & wait \$!; done' ) &"
    fi
    setsid bash -c "
        ${cli_cmd}
        ( exec -a '${bindir}/canterlot-worker' '${bindir}/canterlot-worker' mcp-relay --socket '${WORKER_DATA}/executions/${exec_id}/mcp.sock' ) &
        exec -a '${bindir}/canterlot-worker' '${bindir}/canterlot-worker' supervise --exec-dir '${WORKER_DATA}/executions/${exec_id}'
    " >/dev/null 2>&1 <&- &
    HARNESS_PIDS+=("$!")
    # Дождаться появления супервизора в /proc: точный argv (argv[1]=supervise и
    # нужный --exec-dir), а не pgrep -f. Прежний pgrep с хвостовым пробелом в
    # образце не совпадал никогда и стоил полные 5 с на сессию; к тому же pgrep -f
    # ловит обёртку bash -c, в чьей командной строке тот же текст.
    harness_wait_supervisor "${WORKER_DATA}/executions/${exec_id}"
    # Сокет сессии, как у настоящего воркера (проверка обслуживания ждёт его).
    mkdir -p "${WORKER_SOCKET_DIR}"
    python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.close()' \
        "${WORKER_SOCKET_DIR}/${exec_id}.sock" 2>/dev/null || true
}

harness_wait_supervisor() {
    local want="$1" d i
    local -a argv=()
    for i in $(seq 1 200); do
        for d in /proc/[0-9]*; do
            argv=()
            mapfile -d '' -t argv 2>/dev/null <"${d}/cmdline" || continue
            if [ "${argv[1]:-}" = "supervise" ] && [ "${argv[3]:-}" = "${want}" ]; then
                return 0
            fi
        done
        command sleep 0.02
    done
    harness_log "supervisor for ${want} did not appear after ${i} polls"
    return 1
}

cli_pid_of() {
    local exec_id="$1" sup="$2"
    ps -o pid= --ppid "${sup}" 2>/dev/null | tr -s ' ' '\n' | while read -r cand; do
        [ -n "${cand}" ] || continue
        case "$(tr '\0' ' ' <"/proc/${cand}/cmdline" 2>/dev/null)" in
            *mcp-relay*) ;;
            *) printf '%s' "${cand}"; break ;;
        esac
    done
}

sup_pid_of() {
    local exec_id="$1"
    ps -eo pid=,args= | awk -v id="${exec_id}" '$0 ~ "supervise --exec-dir" && $0 ~ id {print $1; exit}'
}

kill_session() {
    local exec_id="$1"
    for pid in $(ps -eo pid=,args= | awk -v id="${exec_id}" '$0 ~ id {print $1}'); do
        kill "${pid}" 2>/dev/null || true
    done
}

teardown_case() {
    harness_cleanup normal
}

# Уникальные exec id на каждый кейс: осиротевшие процессы прошлых прогонов
# никогда не совпадут с текущими и не попадут в proc-скан.
new_exec_id() { python3 -c "import uuid; print(uuid.uuid4())"; }

# Финальная зачистка на любом выходе: убиваем ТОЛЬКО процессы, чей exe лежит
# внутри FAKE_ROOT (прод-сессии этой VM никогда не трогаем: их exe — в
# ~/.local/opt/canterlot). Без этого упавший тест оставлял живых слипов.
harness_cleanup() {
    # $1 = normal при явном вызове из teardown_case; из ловушки EXIT — авария.
    local mode="${1:-trap}"
    if [ -n "${FAKE_ROOT:-}" ]; then
        # Один find на всю таблицу процессов (а не readlink на процесс): цели
        # /proc/*/exe, лежащие внутри FAKE_ROOT.
        while IFS=' ' read -r pid exe; do
            case "${exe}" in
                # SIGKILL: воркеры, поднятые из обработчика cutover, унаследовали
                # SIG_IGN для INT/TERM и обычным kill не умирают.
                "${FAKE_ROOT}"/*)
                    kill -KILL "${pid}" 2>/dev/null || true
                    wait "${pid}" 2>/dev/null || true ;;
            esac
        done < <(find /proc -mindepth 2 -maxdepth 2 -name exe -printf '%h %l\n' 2>/dev/null | sed 's|^/proc/||')
    fi
    HARNESS_PIDS=()
    # Упавший кейс не трогаем: каталог остаётся для разбора. Из ловушки EXIT
    # сохраняем только если умерли посреди кейса (IN_CASE); штатный конец файла
    # при IN_CASE=0 чистит как обычно.
    if [ "${CASE_FAILED:-0}" != "0" ] || { [ "${mode}" = "trap" ] && [ "${IN_CASE:-0}" = "1" ]; }; then
        if [ -n "${FAKE_ROOT:-}" ]; then
            harness_log "keeping failed case dir ${FAKE_ROOT}"
            FAKE_ROOT=""
        fi
        return 0
    fi
    if [ -n "${FAKE_ROOT:-}" ] && [ -d "${FAKE_ROOT}" ]; then
        if [ "${KEEP_FAKE_ROOT:-0}" = "1" ]; then
            harness_log "keeping ${FAKE_ROOT}"
        else
            rm -rf "${FAKE_ROOT}"
        fi
    fi
    FAKE_ROOT=""
}
trap harness_cleanup EXIT

# --- утверждения ---
PASS_COUNT=0
FAIL_COUNT=0
CASE_FAILED=0

case_begin() { CASE_FAILED=0; IN_CASE=1; printf '=== case: %s ===\n' "$1"; }
case_ok() { PASS_COUNT=$((PASS_COUNT + 1)); printf 'PASS: %s\n' "$1"; }
case_fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1)); CASE_FAILED=1
    printf 'FAIL: %s\n' "$1" >&2
}
case_end() { IN_CASE=0; [ "${CASE_FAILED}" = "0" ] && printf 'case OK\n'; }

assert_eq() {
    if [ "$1" = "$2" ]; then case_ok "$3"; else case_fail "$3 (got [$1], want [$2])"; fi
}
assert_contains() {
    if grep -qF -- "$2" "$1"; then case_ok "$3"; else case_fail "$3 (no [$2] in $1)"; fi
}
assert_not_contains() {
    if grep -qF -- "$2" "$1"; then case_fail "$3 (found [$2] in $1)"; else case_ok "$3"; fi
}
assert_proc_alive() {
    if kill -0 "$1" 2>/dev/null; then case_ok "$2"; else case_fail "$2 (pid $1 dead)"; fi
}
assert_proc_dead() {
    if kill -0 "$1" 2>/dev/null; then case_fail "$2 (pid $1 alive)"; else case_ok "$2"; fi
}
