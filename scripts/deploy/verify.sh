#!/usr/bin/env bash
# При sourcing ничего не исполнять: скрипт запускает боевой выкат только как
# программа (урок 1.16.1: source cutover-скрипта остановил воркер).
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Проверка релиза: два чтения. Версии, миграции, живость воркера, счётчики ошибок
# журнала с обеих сторон, PIDs и тики сессий, курсоры SJ и CJ, version.json PWA,
# CSP, коды остальных сайтов.
# Каждое отклонение — жёсткая ошибка (счётчик, не печать). Тики сверяются на обоих
# чтениях: против --state и против собственного снимка первого чтения, так что
# PASS без сверки тиков невозможен. Снимки — в переменных shell, не в файлах:
# verify ничего не пишет даже во временные файлы.
# Сравнение, которое нельзя выполнить (нет dist, нечего читать), печатается как
# "NOT RUN: ..." и считается провалом, если оно не разрешено явно флагом --allow-skipped.
# Использование: scripts/deploy/verify.sh [SHA] [--dist DIR] [--state FILE] [--web-sha X] [--dry-run] [--allow-skipped]
# Без SHA читается текущее состояние (все три current/build). --dry-run: то же самое,
# только чтение (verify и так ничего не пишет).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

SHA=""
DIST_BASE=""
STATE_FILE=""
WEB_SHA_OVERRIDE=""
DRY_RUN=0
ALLOW_SKIPPED=0
while (($#)); do
    case "$1" in
        --dist) DIST_BASE="$2"; shift 2 ;;
        --state) STATE_FILE="$2"; shift 2 ;;
        --web-sha) WEB_SHA_OVERRIDE="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --allow-skipped) ALLOW_SKIPPED=1; shift ;;
        -*) die "unknown flag: $1" ;;
        *) SHA="$(short_sha "$1")"; shift ;;
    esac
done
XDG_RUNTIME_DIR="/run/user/$(id -u)"
export XDG_RUNTIME_DIR
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
[ -z "${SHA}" ] || require_release_id "${SHA}"
[ -z "${WEB_SHA_OVERRIDE}" ] || require_release_id "${WEB_SHA_OVERRIDE}"
HUB_SHA="${SHA:-$(deployed_sha hub)}"
WEB_SHA="${WEB_SHA_OVERRIDE:-${SHA:-$(deployed_sha web)}}"
WORKER_SHA="${SHA:-$(deployed_sha worker)}"
require_release_id "${HUB_SHA}"
require_release_id "${WEB_SHA}"
require_release_id "${WORKER_SHA}"
DIST_HUB="${DIST_BASE:-${REPO_ROOT}/dist}/${HUB_SHA}"
# Базовая проверка развёрнутого релиза (dry-run или репетиция): dist базы может
# не быть в сборочном worktree, тогда CSP сверяется с записью самого релиза.
BASELINE=0
if [ "${DRY_RUN}" = "1" ] || is_rehearse; then BASELINE=1; fi
if [ "${DRY_RUN}" = "1" ]; then
    log "dry-run mode: read-only verification of deployed releases"
    # Dry-run не пишет никогда: любая запись (а их здесь одна — loopback-проба)
    # проходит через run_write в режиме репетиции.
    DEPLOY_MODE=rehearse
fi

# Сравнения, которые нельзя выполнить: список строк (без повторов между чтениями).
NOT_RUN_LIST=""
not_run() {
    case "${NOT_RUN_LIST}" in *"${1}"$'\n'*) return ;; esac
    NOT_RUN_LIST+="${1}"$'\n'
    log "NOT RUN: $1"
}

# Ожидаемая миграция: из RELEASE в dist этого релиза, а без dist — из RELEASE
# развёрнутого релиза хаба (тот же файл, что положил stage). Если нет ни того, ни
# другого, сравнение не выполняется (NOT RUN), а не пропадает молча.
EXPECTED_MIGR=""
parse_migr_last() { grep '^migrations_last=' | cut -d= -f2 | grep -oE '^[0-9]+' | sed 's/^0*//' || true; }
if [ -f "${DIST_HUB}/RELEASE" ]; then
    EXPECTED_MIGR="$(parse_migr_last <"${DIST_HUB}/RELEASE")"
    [ -n "${EXPECTED_MIGR}" ] || die "bad migrations_last in ${DIST_HUB}/RELEASE"
elif DEPLOYED_RELEASE_RAW="$(ssh_hub "cat $(shq "${HUB_RELEASES}/${HUB_SHA}/RELEASE")" 2>/dev/null)"; then
    EXPECTED_MIGR="$(printf '%s\n' "${DEPLOYED_RELEASE_RAW}" | parse_migr_last)"
    if [ -n "${EXPECTED_MIGR}" ]; then
        log "no dist RELEASE for ${HUB_SHA}: expected migrations ${EXPECTED_MIGR} taken from the deployed hub RELEASE"
    fi
fi

hub_psql() { ssh_hub "runuser -u canterlot -- psql -h '${HUB_PG_SOCKET}' -d '${HUB_PG_DB}' -tAc \"$1\"" || die "hub DB query failed"; }

# Собственный снимок тиков первого чтения — в переменной, не в файле.
OWN_SNAP=""
# Снимок курсоров первого чтения: строки "kind id value".
ACKS_1=""

# Процесс пропал: сверяем со строкой хаба. Нормально завершённая сессия (строка
# терминальная: stopped с исходом user_stop, normal_exit или force_stopped) —
# это "ended", а не провал: выкат её не вызывал. Всё остальное (строка жива, строки
# нет, исход crashed, запрос не удался) — жёстко.
gone_is_normal_end() {
    local exec_id="$1" row state outcome
    row="$(hub_psql "select state || ':' || coalesce(outcome, 'null') from executions where id = '${exec_id}'" 2>/dev/null || echo UNKNOWN)"
    [ "${row}" != "UNKNOWN" ] || return 1
    state="${row%%:*}"; outcome="${row##*:}"
    [ "${state}" = "stopped" ] || return 1
    case "${outcome}" in user_stop|normal_exit|force_stopped) return 0 ;; *) return 1 ;; esac
}
# Одна пометка на сессию, сколько бы ролей и курсоров ни пропало.
ENDED_SEEN=" "
note_session_ended() {
    local exec_id="$1" label="$2"
    case "${ENDED_SEEN}" in *" ${exec_id} "*) return 0 ;; esac
    ENDED_SEEN+="${exec_id} "
    log "session ended between reads: ${exec_id} (${label}); execution row is terminal, not a failure of the rollout"
}
# Перечитывание курсора не вышло: нормальный конец сессии — не провал.
ack_gone_is_end() {
    local exec_id="$1" label="$2" sup
    sup="$(sup_pid_of_exec "${exec_id}")"
    # Живой супервизор — сессия не закончилась, а курсор пропал: это провал.
    if [ -n "${sup}" ] && proc_is_live "${sup}"; then return 1; fi
    gone_is_normal_end "${exec_id}" || return 1
    note_session_ended "${exec_id}" "${label}"
}
# Живой супервизор исполнения по /proc (пусто — нет).
sup_pid_of_exec() {
    local exec_id="$1" line
    line="$(proc_ticks "${exec_id}" | grep supervise | grep -v mcp-relay | sed -n 1p || true)"
    printf '%s' "${line%% *}"
}

# Сверка тиков против файла снимка: любое изменение — жёстко.
check_ticks_file() {
    local file="$1" label="$2"
    [ -f "${file}" ] || { hard "tick snapshot file missing: ${file}"; return; }
    while IFS= read -r line; do
        case "${line}" in
            worker\ * | "") continue ;;
            *) exec_id="${line%% *}"
               ok=1
               for role in sup cli relay; do
                   for old in $(snapshot_role_values "${line}" "${role}"); do
                       old_pid="${old%%:*}"; old_tick="${old##*:}"
                       if ! proc_is_live "${old_pid}"; then
                           if gone_is_normal_end "${exec_id}"; then
                               note_session_ended "${exec_id}" "${label}"
                           else
                               ok=0; hard "session process gone (${label}): ${exec_id} ${role} ${old_pid}"
                           fi
                           continue
                       fi
                       new_tick="$(tr ' ' '\n' <"/proc/${old_pid}/stat" 2>/dev/null | sed -n '22p' || echo gone)"
                       [ "${new_tick}" = "${old_tick}" ] || { ok=0; hard "tick changed (${label}): ${exec_id} ${role} ${old_pid}"; }
                   done
               done
               [ "${ok}" = "1" ] && log "exec ${exec_id}: ticks unchanged (${label})" ;;
        esac
    done <"${file}"
}

# Сверка тиков против собственного снимка первого чтения.
check_ticks_own() {
    local label="$1" line
    while IFS= read -r line; do
        [ -n "${line}" ] || continue
        # shellcheck disable=SC2086
        set -- ${line}
        [ "$#" -eq 4 ] || { hard "bad own snapshot line: ${line}"; continue; }
        exec_id="$1"; role="$2"; old_pid="$3"; old_tick="$4"
        if ! proc_is_live "${old_pid}"; then
            if gone_is_normal_end "${exec_id}"; then
                note_session_ended "${exec_id}" "${label}"
            else
                hard "session process gone (${label}): ${exec_id} ${role} ${old_pid}"
            fi
            continue
        fi
        new_tick="$(tr ' ' '\n' <"/proc/${old_pid}/stat" 2>/dev/null | sed -n '22p' || echo gone)"
        [ "${new_tick}" = "${old_tick}" ] || hard "tick changed (${label}): ${exec_id} ${role} ${old_pid}"
    done <<<"${OWN_SNAP}"
    log "own tick snapshot unchanged (${label})"
}

take_own_snapshot() {
    local live_ids
    OWN_SNAP=""
    if ! live_ids="$(hub_psql "select id::text from executions where state='running' order by 1")"; then
        hard "cannot list live executions for the own snapshot"
        return
    fi
    for exec_id in ${live_ids:-}; do
        sup_line="$(proc_ticks "${exec_id}" | grep supervise | grep -v mcp-relay | sed -n 1p)"
        [ -n "${sup_line}" ] || { hard "no supervisor for live execution ${exec_id}"; continue; }
        sup_pid="${sup_line%% *}"; sup_tick="$(printf '%s' "${sup_line}" | cut -d' ' -f2)"
        OWN_SNAP+="${exec_id} sup ${sup_pid} ${sup_tick}"$'\n'
        # Роли точные: супервизор, его CLI и релеи. Потомки CLI (вызовы инструментов
        # параллельных сессий) приходят и уходят и в снимок не входят.
        if ! cli_line="$(session_cli_pid "${sup_pid}")"; then
            hard "cannot list children of supervisor ${sup_pid} for ${exec_id}"
            continue
        fi
        [ -n "${cli_line}" ] || { hard "no CLI child of supervisor ${sup_pid} for ${exec_id}"; continue; }
        OWN_SNAP+="${exec_id} cli ${cli_line}"$'\n'
    done
    # Релеи: по всей таблице процессов, а не только среди прямых детей. Иначе
    # убитый между чтениями релей не попадал в снимок и verify давал PASS.
    for d in /proc/[0-9]*; do
        cand="${d#/proc/}"
        _relay_exec_id "${cand}"
        relay_id="${RELAY_ID}"
        [ -n "${relay_id}" ] || continue
        in_lines "${relay_id}" "${live_ids}" || continue
        is_zombie "${cand}" && continue
        tick="$(tr ' ' '\n' <"/proc/${cand}/stat" 2>/dev/null | sed -n '22p')"
        OWN_SNAP+="${relay_id} relay ${cand} ${tick:-?}"$'\n'
    done
}

# Курсор не убывает между чтениями: сверка снимка ack первого чтения с текущими.
compare_acks() {
    local line
    while IFS= read -r line; do
        [ -n "${line}" ] || continue
        # shellcheck disable=SC2086
        set -- ${line}
        [ "$#" -eq 3 ] || { hard "bad ack snapshot line: ${line}"; continue; }
        id="$1"; kind="$2"; old="$3"
        if [ "${id}" = "cj" ]; then
            new="$(read_ack "${WORKER_DATA}/cj/ack" || echo unreadable)"
        else
            new="$(read_ack "${WORKER_DATA}/executions/${id}/sj/ack" || echo unreadable)"
        fi
        case "${new}" in
            ''|unreadable)
                # Сессия нормально завершилась между чтениями: каталог исполнения
                # убран, перечитывать нечего. Живая или упавшая — провал.
                if [ "${id}" != "cj" ] && ack_gone_is_end "${id}" "${kind} ack not re-read"; then
                    continue
                fi
                hard "cannot re-read ${kind} ack for ${id}"
                continue ;;
            *) ;;
        esac
        case "${old}" in ''|unreadable) continue;; *) ;;
        esac
        if [ "${new}" -ge "${old}" ] 2>/dev/null; then
            log "${kind} ack ${id}: ${old} -> ${new}"
        else
            hard "${kind} ack decreased for ${id}: ${old} -> ${new}"
        fi
    done <<<"${ACKS_1}"
}

snap_acks() {
    ACKS_1=""
    local exec_id sj cj live_ids
    if ! live_ids="$(hub_psql "select id::text from executions where state='running' order by 1")"; then
        hard "cannot list live executions for ack snapshots"
        return
    fi
    for exec_id in ${live_ids:-}; do
        sj="$(read_ack "${WORKER_DATA}/executions/${exec_id}/sj/ack" || echo unreadable)"
        if [ "${sj}" = "unreadable" ] || [ -z "${sj}" ]; then
            ack_gone_is_end "${exec_id}" "sj ack not readable" || hard "cannot read sj ack for ${exec_id}"
        else
            ACKS_1+="${exec_id} sj ${sj}"$'\n'
        fi
    done
    cj="$(read_ack "${WORKER_DATA}/cj/ack" || echo unreadable)"
    if [ "${cj}" = "unreadable" ] || [ -z "${cj}" ]; then
        hard "cannot read cj ack"
    else
        ACKS_1+="cj cj ${cj}"$'\n'
    fi
}

# Воркер обязан не просто быть active, а соединиться с хабом В ТЕКУЩЕМ запуске:
# 1) журнал юнита с момента его старта (ActiveEnterTimestamp) — последний
#    `session state state="..."` (рендер tracing fmt, session.rs transition) равен live;
# 2) хаб видит машину: availability=available и reported_at свежее запуска воркера
#    (возраст считает сам хаб, так что расхождение часов не влияет).
# Соединение занимает секунды, поэтому ждём до CONNECT_WAIT секунд.
CONNECT_WAIT="${CONNECT_WAIT:-60}"
# Что релиз ждёт от воркера, объявляет сам релиз: dist/<worker>/EXPECT (его пишет
# scripts/release-build/in-container.sh из вывода `canterlot-worker capabilities`,
# файл входит в SHA256SUMS). Формат: format=1, capabilities=…, harnesses=… через
# пробел. Требуемые харнессы — объявленные релизом и включённые в worker.toml
# (таблица есть и enabled не false; читаются только имена таблиц и ключ enabled).
# Релиз, который намеренно убирает харнесс или возможность, не требует правки
# toolkit. Dist без EXPECT (старые релизы) — прежний список с предупреждением.
REQUIRED_CAPABILITIES_FALLBACK="${REQUIRED_CAPABILITIES:-claude_histories_v1 path_probe_v1}"
REQUIRED_HARNESSES_FALLBACK="${REQUIRED_HARNESSES:-claude_code codex opencode}"
EXPECT_FILE="${DIST_BASE:-${REPO_ROOT}/dist}/${WORKER_SHA}/EXPECT"
EXPECT_SOURCE=""
REQUIRED_CAPABILITIES=""
REQUIRED_HARNESSES=""
load_expectations() {
    local line key value fmt="" caps="" harnesses="" enabled
    if [ ! -f "${EXPECT_FILE}" ]; then
        REQUIRED_CAPABILITIES="${REQUIRED_CAPABILITIES_FALLBACK}"
        REQUIRED_HARNESSES="${REQUIRED_HARNESSES_FALLBACK}"
        EXPECT_SOURCE="fallback"
        log "WARNING: no ${EXPECT_FILE} (release built before EXPECT existed): falling back to the built-in list (capabilities: ${REQUIRED_CAPABILITIES}; harnesses: ${REQUIRED_HARNESSES})"
        return 0
    fi
    while IFS= read -r line || [ -n "${line}" ]; do
        key="${line%%=*}"
        value="${line#*=}"
        case "${key}" in
            format) fmt="${value}" ;;
            capabilities) caps="${value}" ;;
            harnesses) harnesses="${value}" ;;
            '') ;;
            *) die "unknown key in ${EXPECT_FILE}: ${key}" ;;
        esac
    done <"${EXPECT_FILE}"
    [ "${fmt}" = "1" ] || die "unsupported ${EXPECT_FILE} format '${fmt}' (this toolkit knows format=1)"
    [ -n "${caps}" ] || die "${EXPECT_FILE} declares no capabilities"
    [[ "${caps} ${harnesses}" =~ ^[a-z0-9_\ ]+$ ]] || die "${EXPECT_FILE} has odd names"
    # shellcheck disable=SC2086 # harnesses — имена через пробел, проверены выше
    if ! enabled="$(python3 -I - "${WORKER_CONFIG}" ${harnesses} <<'PY'
import sys, tomllib
try:
    cfg = tomllib.load(open(sys.argv[1], "rb"))
except (OSError, tomllib.TOMLDecodeError) as exc:
    print(f"cannot parse worker config: {type(exc).__name__}", file=sys.stderr)
    sys.exit(1)
on = []
for name in sys.argv[2:]:
    table = cfg.get(name)
    if isinstance(table, dict) and table.get("enabled", True) is not False:
        on.append(name)
print(" ".join(on))
PY
    )"; then
        die "cannot read harness tables from ${WORKER_CONFIG}"
    fi
    REQUIRED_CAPABILITIES="${caps}"
    REQUIRED_HARNESSES="${enabled}"
    EXPECT_SOURCE="${EXPECT_FILE}"
    log "expectations from the release ${EXPECT_FILE}: capabilities ${caps}; harnesses declared ${harnesses}, enabled in ${WORKER_CONFIG##*/}: ${enabled:-none}"
}
load_expectations
# Каталог машины наполняется через секунды после соединения: ждём до CATALOG_WAIT.
CATALOG_WAIT="${CATALOG_WAIT:-60}"
worker_machine_id() {
    local mid
    mid="$(sed -n 's/^machine_id[ ]*=[ ]*"\([0-9a-fA-F-]*\)".*/\1/p' "${WORKER_CONFIG}" 2>/dev/null | sed -n 1p)"
    [[ "${mid}" =~ ^[0-9a-fA-F-]{36}$ ]] || return 1
    printf '%s' "${mid}"
}
check_worker_connected() {
    local since since_epoch uptime wlog last_state machine_id row avail age waited=0 problem=""
    if ! since="$(systemctl --user show "${WORKER_UNIT}" -p ActiveEnterTimestamp --value 2>/dev/null)" || [ -z "${since}" ]; then
        hard "no fresh worker connection: cannot read the start time of ${WORKER_UNIT}"
        return
    fi
    if ! since_epoch="$(date -d "${since}" +%s 2>/dev/null)"; then
        hard "no fresh worker connection: cannot parse the start time '${since}'"
        return
    fi
    if ! machine_id="$(worker_machine_id)"; then
        hard "no fresh worker connection: cannot read machine_id from ${WORKER_CONFIG}"
        return
    fi
    while :; do
        problem=""
        uptime=$(( $(date +%s) - since_epoch ))
        if ! wlog="$(journalctl --user -u "${WORKER_UNIT}" --since "${since}" --no-pager)"; then
            problem="cannot read the worker journal since ${since}"
        else
            last_state="$(printf '%s\n' "${wlog}" | grep -E 'session state state="' | tail -1 | sed -n 's/.*state="\([a-z_]*\)".*/\1/p')" || last_state=""
            if [ "${last_state}" != "live" ]; then
                problem="worker journal since ${since} shows no live session (last state: ${last_state:-none})"
            fi
        fi
        if [ -z "${problem}" ]; then
            row="$(hub_psql "select availability || ':' || coalesce(floor(extract(epoch from (now() - reported_at)))::bigint::text, 'never') from machines where id = '${machine_id}'")" \
                || { hard "no fresh worker connection: cannot read the machine row"; return; }
            avail="${row%%:*}"; age="${row##*:}"
            if [ "${avail}" != "available" ]; then
                problem="hub does not see the worker: machine availability is '${avail:-missing}'"
            elif ! [[ "${age}" =~ ^[0-9]+$ ]] || [ "${age}" -gt $((uptime + 5)) ]; then
                problem="hub does not see the worker: machine reported_at is '${age}' s old, worker started ${uptime} s ago"
            fi
        fi
        [ -n "${problem}" ] || break
        if [ "${waited}" -ge "${CONNECT_WAIT}" ]; then
            hard "no fresh worker connection: ${problem}"
            return
        fi
        sleep 1
        waited=$((waited + 1))
    done
    log "worker connected in this invocation (live since ${since}); hub sees the machine (available, reported ${age} s ago)"
}

# Возможности и каталог харнессов после переподключения: имена, без значений.
# Требования — из EXPECT релиза (load_expectations). Повтор до CATALOG_WAIT секунд.
check_machine_catalog() {
    local machine_id caps harnesses cap h missing waited=0
    if ! machine_id="$(worker_machine_id)"; then
        hard "capability check: cannot read machine_id from ${WORKER_CONFIG}"
        return
    fi
    while :; do
        missing=""
        if ! caps="$(hub_psql "select array_to_string(capabilities, ',') from machines where id = '${machine_id}'")"; then
            hard "capability check: cannot read machines.capabilities"
            return
        fi
        if ! harnesses="$(hub_psql "select string_agg(harness, ',' order by harness) from machine_harnesses where machine_id = '${machine_id}'")"; then
            hard "harness catalog: cannot read machine_harnesses"
            return
        fi
        for cap in ${REQUIRED_CAPABILITIES}; do
            case ",${caps}," in
                *",${cap},"*) ;;
                *) missing+="capability ${cap}; " ;;
            esac
        done
        for h in ${REQUIRED_HARNESSES}; do
            case ",${harnesses}," in
                *",${h},"*) ;;
                *) missing+="harness ${h}; " ;;
            esac
        done
        [ -n "${missing}" ] || break
        if [ "${waited}" -ge "${CATALOG_WAIT}" ]; then
            for cap in ${REQUIRED_CAPABILITIES}; do
                case ",${caps}," in *",${cap},"*) ;; *) hard "capability check: machine lacks ${cap}" ;; esac
            done
            for h in ${REQUIRED_HARNESSES}; do
                case ",${harnesses}," in *",${h},"*) ;; *) hard "harness catalog: machine lacks ${h}" ;; esac
            done
            return
        fi
        sleep 1
        waited=$((waited + 1))
    done
    log "capabilities present: ${REQUIRED_CAPABILITIES// /, } (source: ${EXPECT_SOURCE})"
    log "harness catalog has ${REQUIRED_HARNESSES// /, }${REQUIRED_HARNESSES:+ }(machine reports: ${harnesses:-none})"
}

# Precompressed Content-Encoding: тот же Caddyfile-фрагмент, что у живого сайта,
# но на одноразовом loopback-процессе ВНУТРИ контейнера (решение владельца:
# допустимо в боевом прогоне). Живой слушатель отдаёт статику без входа, но проба
# на loopback не зависит от DNS, TLS и правил сайта (в первом выкате собственного
# входа перед сайтом ещё стоит basic_auth) и видит ровно файлы релиза. Процесс один на все три
# кодировки, слушает только 127.0.0.1:18080, живёт под timeout и гасится
# ловушкой на EXIT/INT/TERM/HUP; его старт и останов попадают в журнал, а после
# пробы отдельное чтение доказывает, что порт закрыт. Запуск — запись через
# run_write: в репетиции он только печатается, в dry-run — SKIPPED с причиной.
# shellcheck disable=SC2016 # скрипт раскрывается внутри контейнера
PROBE_SCRIPT='set -eu
d="$(mktemp -d)"
pid=""
cleanup() {
    if [ -n "$pid" ]; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
        echo "probe: loopback caddy pid $pid stopped"
    fi
    rm -rf "$d"
}
trap cleanup EXIT
trap "exit 1" INT TERM HUP
if curl -s -o /dev/null http://127.0.0.1:18080/ 2>/dev/null; then echo "probe: port 18080 already in use"; exit 4; fi
printf "%s\n" "{" "  admin off" "  auto_https off" "}" ":18080 {" "  bind 127.0.0.1" "  root * /srv/canterlot/web/build" "  file_server {" "    precompressed zstd br gzip" "  }" "}" >"$d/Caddyfile"
if command -v timeout >/dev/null 2>&1; then
    timeout "$LIFETIME" caddy run --config "$d/Caddyfile" --adapter caddyfile >/dev/null 2>&1 &
else
    caddy run --config "$d/Caddyfile" --adapter caddyfile >/dev/null 2>&1 &
fi
pid=$!
echo "probe: loopback caddy pid $pid started on 127.0.0.1:18080"
i=0
while [ "$i" -lt 50 ]; do
    if curl -fsS -o /dev/null http://127.0.0.1:18080/200.html 2>/dev/null; then break; fi
    i=$((i + 1))
    sleep 0.1
done
for enc in $ENCS; do
    echo "probe-enc: $enc"
    curl -sS -D - -o /dev/null -H "Accept-Encoding: $enc" "http://127.0.0.1:18080/$ASSET"
done'
PROBE_LIFETIME="${PROBE_LIFETIME:-60}"
check_precompressed() {
    # Мягкая проверка, как всё PWA: недоступная кодировка не должна валить выкат
    # после flip хаба и тем более гасить воркер, на котором живут отчёты.
    local asset enc got out list
    if [ "${DRY_RUN}" = "1" ]; then
        log "SKIPPED: precompressed Content-Encoding (dry-run: would start a one-off loopback Caddy inside the container)"
        return 0
    fi
    if ! list="$(ssh_web "cd '${WEB_RELEASES}/${WEB_SHA}' && find _app/immutable -type f")"; then
        soft "precompressed check: cannot pick an asset from release ${WEB_SHA}"
        return
    fi
    # Проба идёт по первому по LC_ALL=C *.js, у которого в релизе есть все три
    # сжатых соседа: сборка не сжимает совсем маленькие файлы, и Caddy честно
    # отдаёт их без Content-Encoding (инцидент 1.19.1: --RoKY8p.js, 22 байта).
    # Список читается одним ssh; фильтрация локальная: имена файлов — только
    # данные для sort/awk, они не попадают в текст удалённой команды и не могут
    # стать опциями (сам путь обязан начинаться с _app/immutable/).
    asset="$(printf '%s\n' "${list}" | LC_ALL=C sort | awk '
        { present[$0] = 1; if ($0 ~ /[.]js$/) js[++n] = $0 }
        END {
            for (i = 1; i <= n; i++) {
                f = js[i]
                if (found == "" && present[f ".br"] && present[f ".zst"] && present[f ".gz"]) found = f
            }
            print found
        }')"
    if [ -z "${asset}" ]; then
        soft "precompressed check: no JS asset with br/zstd/gzip siblings in release ${WEB_SHA}"
        return
    fi
    case "${asset}" in
        _app/immutable/*) ;;
        *)
            soft "precompressed check: unexpected asset path '${asset}' in release ${WEB_SHA}"
            return
            ;;
    esac
    log "precompressed probe: starting a one-off loopback Caddy in ${WEB_CONTAINER} (127.0.0.1:18080, timeout ${PROBE_LIFETIME}s, trap-stopped) for ${asset}"
    # Скрипт фиксирован, значения уходят в контейнер через -e: никакой
    # интерполяции имён файлов в shell-текст.
    if ! out="$(printf '%s\n' "${PROBE_SCRIPT}" | run_write_pipe "loopback caddy probe" \
            ssh -o BatchMode=yes -o ConnectTimeout=10 "${WEB_SSH}" \
            "docker exec -i -e ASSET=$(shq "${asset}") -e ENCS='br zstd gzip' -e LIFETIME=$(shq "${PROBE_LIFETIME}") $(shq "${WEB_CONTAINER}") sh -s")"; then
        soft "precompressed check: probe failed ($(printf '%s\n' "${out}" | grep '^probe:' | tr '\n' ' '))"
        return
    fi
    if is_rehearse; then
        log "rehearse: loopback Caddy probe printed above, not started"
        return 0
    fi
    printf '%s\n' "${out}" | grep '^probe:' | while IFS= read -r line; do log "${line}"; done
    for enc in br zstd gzip; do
        got="$(printf '%s\n' "${out}" | tr -d '\r' | awk -v enc="${enc}" '
            /^probe-enc: / { cur = $2; next }
            cur == enc && tolower($0) ~ /^content-encoding:/ { sub(/^[^:]*:[ \t]*/, ""); if (val == "") val = $0 }
            END { print val }')"
        if [ "${got}" = "${enc}" ]; then
            log "precompressed ${enc}: Content-Encoding ${got} on ${asset}"
        else
            soft "precompressed ${enc}: got '${got:-none}' on ${asset}"
        fi
    done
    case "${out}" in
        *"probe: loopback caddy pid "*" stopped"*) ;;
        *) soft "precompressed check: the loopback Caddy did not report its stop" ;;
    esac
    if out="$(ssh_web "docker exec $(shq "${WEB_CONTAINER}") sh -c 'if curl -s -o /dev/null http://127.0.0.1:18080/ 2>/dev/null; then echo open; else echo closed; fi'")" \
            && [ "${out}" = "closed" ]; then
        log "precompressed probe: port 18080 closed again inside ${WEB_CONTAINER}"
    else
        soft "precompressed check: port 18080 still answers inside ${WEB_CONTAINER} after the probe (${out:-unreadable})"
    fi
}

# WARN/ERROR хаба сравниваются с базой, а не с нулём: у прода есть собственный
# поток WARN от старого хаба (machine_metrics_ignored execution_counter, ~1000 в
# сутки). База — журнал хаба ДО окна проверки (за сутки перед ним, т.е. до flip);
# окно проверки — последние HUB_WINDOW_MIN минут. Вид сообщения = поле kind="..."
# (плюс reason="..."), а без kind — текст сообщения до первого поля. Новый вид
# или рост скорости выше базы в WARN_RATE_FACTOR раз (плюс WARN_RATE_SLACK
# строк на всплеск) — жёстко. Известная база проходит, виды печатаются.
HUB_WINDOW_MIN="${HUB_WINDOW_MIN:-30}"
HUB_BASELINE_MIN="${HUB_BASELINE_MIN:-1440}"
WARN_RATE_FACTOR="${WARN_RATE_FACTOR:-3}"
WARN_RATE_SLACK="${WARN_RATE_SLACK:-5}"
# Виды WARN, которые ожидаются и без базы (через пробел): не жёсткая ошибка ни как
# новый вид, ни как рост, только строка журнала. config_key_deprecated хаб пишет при
# каждом старте, пока в hub.toml остаются устаревшие ключи (спека входа §11: устаревшее
# значение не должно останавливать выкат). Первый релиз собственного входа добавляет
# client_ip_missing и identity_header_forwarded до правки Caddy (pilot.md §11.8).
VERIFY_WARN_KINDS_OK="${VERIFY_WARN_KINDS_OK:-config_key_deprecated}"
warn_kind_ok() {
    local k
    for k in ${VERIFY_WARN_KINDS_OK}; do
        if [ "${k}" = "$1" ]; then return 0; fi
    done
    return 1
}
# stdin — журнал хаба; stdout — "<число>\t<УРОВЕНЬ вид>" по каждому виду.
level_kind_counts() {
    sed -nE 's/.*[0-9]Z +(WARN|ERROR) (.*)$/\1 \2/p' | awk '{
        level = $1; $1 = ""; rest = $0; kind = ""; reason = ""
        if (match(rest, /kind="[^"]*"/)) kind = substr(rest, RSTART + 6, RLENGTH - 7)
        if (match(rest, /reason="?[A-Za-z0-9_.:-]+"?/)) { reason = substr(rest, RSTART + 7, RLENGTH - 7); gsub(/"/, "", reason) }
        if (kind == "") {
            msg = rest; sub(/ [A-Za-z_][A-Za-z0-9_.]*=.*$/, "", msg); gsub(/^ +| +$/, "", msg); gsub(/ +/, "_", msg); kind = msg
        }
        print level " " kind (reason != "" ? "/" reason : "")
    }' | LC_ALL=C sort | uniq -c | sed -E 's/^ *([0-9]+) /\1\t/'
}
check_hub_levels() {
    local -A base_n=() cur_n=()
    local n key b allowed level kind summary=""
    while IFS=$'\t' read -r n key; do
        if [ -n "${key}" ]; then base_n["${key}"]="${n}"; fi
    done < <(printf '%s\n' "${HUB_BASELOG}" | level_kind_counts)
    while IFS=$'\t' read -r n key; do
        if [ -n "${key}" ]; then cur_n["${key}"]="${n}"; fi
    done < <(printf '%s\n' "${HUB_ERRLOG}" | level_kind_counts)
    while IFS= read -r key; do
        [ -n "${key}" ] || continue
        n="${cur_n["${key}"]}"; b="${base_n["${key}"]:-0}"
        level="${key%% *}"; kind="${key#* }"
        if [ "${level}" = "WARN" ] && warn_kind_ok "${kind%%/*}"; then
            summary+="${n} ${level} ${kind} (expected, VERIFY_WARN_KINDS_OK); "
            continue
        fi
        if [ "${b}" = "0" ]; then
            hard "new hub ${level} kind not in the baseline: ${kind} (${n} in ${HUB_WINDOW_MIN} min)"
            summary+="${n} ${level} ${kind} (NEW); "
            continue
        fi
        allowed=$(( (b * HUB_WINDOW_MIN * WARN_RATE_FACTOR + HUB_BASELINE_MIN - 1) / HUB_BASELINE_MIN + WARN_RATE_SLACK ))
        if [ "${n}" -gt "${allowed}" ]; then
            hard "hub ${level} rate increase: ${kind}: ${n} in ${HUB_WINDOW_MIN} min, baseline ${b} per ${HUB_BASELINE_MIN} min (allowed up to ${allowed})"
            summary+="${n} ${level} ${kind} (RATE UP); "
        else
            summary+="${n} ${level} ${kind} (baseline ${b}/${HUB_BASELINE_MIN} min, allowed ${allowed}); "
        fi
    done < <(printf '%s\n' "${!cur_n[@]}" | LC_ALL=C sort)
    if [ -n "${summary}" ]; then
        log "hub WARN/ERROR kinds: ${summary%; }"
        log "hub WARN/ERROR within the baseline unless reported as HARD-FAIL above"
    else
        log "hub WARN/ERROR kinds: none"
    fi
}

# Один проход чтения; префикс отличает первое и второе чтение.
verify_read() {
    local tag="$1" live_ids live_summary
    log "=== verification read ${tag} ==="
    # Каждое имя сначала читается и проверяется (deployed_sha), потом сравнивается и
    # только потом может попасть в команду на хосте.
    HUB_NOW="$(deployed_sha hub)" || { hard "cannot read the deployed hub release"; HUB_NOW="UNREADABLE"; }
    WEB_NOW="$(deployed_sha web)" || { hard "cannot read the deployed web release"; WEB_NOW="UNREADABLE"; }
    WORKER_NOW="$(deployed_sha worker)" || { hard "cannot read the deployed worker release"; WORKER_NOW="UNREADABLE"; }
    [ "${HUB_NOW}" = "${HUB_SHA}" ] || hard "hub current is ${HUB_NOW}, expected ${HUB_SHA}"
    [ "${WEB_NOW}" = "${WEB_SHA}" ] || hard "web build is ${WEB_NOW}, expected ${WEB_SHA}"
    [ "${WORKER_NOW}" = "${WORKER_SHA}" ] || hard "worker current is ${WORKER_NOW}, expected ${WORKER_SHA}"
    log "versions: hub=${HUB_SHA} web=${WEB_SHA} worker=${WORKER_SHA}"
    HUB_REL_SHA="$(ssh_hub "grep '^sha=' $(shq "${HUB_RELEASES}/${HUB_NOW}/RELEASE")" | cut -d= -f2 || echo UNKNOWN)"
    [ "${HUB_REL_SHA}" = "${HUB_SHA}" ] || hard "hub RELEASE sha is ${HUB_REL_SHA}, expected ${HUB_SHA}"
    [ "$(grep '^sha=' "${WORKER_CURRENT}/RELEASE" 2>/dev/null | cut -d= -f2 || echo UNKNOWN)" = "${WORKER_SHA}" ] \
        || hard "worker RELEASE sha mismatch"
    MIGR_MAX="$(hub_psql "select coalesce(max(version),0) from _sqlx_migrations where success")"
    MIGR_FAILED="$(hub_psql "select count(*) from _sqlx_migrations where not success")"
    log "migrations: max=${MIGR_MAX} failed=${MIGR_FAILED}"
    [ "${MIGR_FAILED}" = "0" ] || hard "failed migrations present: ${MIGR_FAILED}"
    if [ -n "${EXPECTED_MIGR}" ]; then
        [ "${MIGR_MAX}" = "${EXPECTED_MIGR}" ] || hard "migrations max ${MIGR_MAX}, expected ${EXPECTED_MIGR}"
    else
        not_run "migrations vs RELEASE (neither dist ${DIST_HUB}/RELEASE nor the deployed hub RELEASE is readable)"
    fi
    [ "$(systemctl --user is-active "${WORKER_UNIT}" 2>/dev/null)" = "active" ] \
        || hard "worker unit not active"
    WORKER_PID="$(systemctl --user show "${WORKER_UNIT}" -p MainPID --value 2>/dev/null || echo 0)"
    WORKER_EXE="$(readlink "/proc/${WORKER_PID}/exe" 2>/dev/null || echo unknown)"
    [ "${WORKER_EXE}" = "${WORKER_RELEASES}/${WORKER_SHA}/bin/canterlot-worker" ] \
        || hard "worker process exe is ${WORKER_EXE}, expected release ${WORKER_SHA}"
    log "worker unit: active, exe=${WORKER_EXE}, restarts=$(systemctl --user show "${WORKER_UNIT}" -p NRestarts --value 2>/dev/null || echo unknown)"
    check_worker_connected
    check_machine_catalog
    [ "$(ssh_hub "systemctl is-active '${HUB_UNIT}'" || echo unknown)" = "active" ] \
        || hard "hub unit not active"
    HUB_PID="$(ssh_hub "systemctl show '${HUB_UNIT}' -p MainPID --value" 2>/dev/null || echo 0)"
    HUB_EXE="$(ssh_hub "readlink '/proc/${HUB_PID}/exe'" 2>/dev/null || echo unknown)"
    [ "${HUB_EXE}" = "${HUB_RELEASES}/${HUB_SHA}/bin/canterlot-hub" ] \
        || hard "hub process exe is ${HUB_EXE}, expected release ${HUB_SHA}"
    READYZ="$(ssh_hub "curl -fsS -m 2 'http://${HUB_IP}:8080/readyz'" 2>/dev/null || echo FAIL)"
    [ "${READYZ}" = '{"status":"ok"}' ] || hard "hub readyz not ok: ${READYZ}"
    log "hub unit: active, exe=${HUB_EXE}, readyz=${READYZ}"
    # Журнал читается целиком; провал самого чтения — жёстко (а не «ноль ошибок»).
    if ! HUB_ERRLOG="$(ssh_hub "journalctl -u '${HUB_UNIT}' --since -${HUB_WINDOW_MIN}min --no-pager")"; then
        hard "cannot read hub journal"
        HUB_ERRLOG=""
    fi
    if ! HUB_BASELOG="$(ssh_hub "journalctl -u '${HUB_UNIT}' --since -$((HUB_BASELINE_MIN + HUB_WINDOW_MIN))min --until -${HUB_WINDOW_MIN}min --no-pager")"; then
        hard "cannot read the hub journal baseline (before the verification window)"
        HUB_BASELOG=""
    fi
    MIGR_ERR="$(printf '%s' "${HUB_ERRLOG}" | grep -c migration_error || true)"
    PANIC="$(printf '%s' "${HUB_ERRLOG}" | grep -c panic || true)"
    # Формат хаба — tracing_subscriber::fmt (crates/canterlot-hub/src/main.rs):
    # "<ISO-время>Z  WARN сообщение ...", без level=. Уровень стоит сразу после
    # метки времени, так что слова WARN/ERROR внутри текста не считаются.
    WARN_N="$(printf '%s' "${HUB_ERRLOG}" | grep -cE '[0-9]Z +WARN ' || true)"
    ERROR_N="$(printf '%s' "${HUB_ERRLOG}" | grep -cE '[0-9]Z +ERROR ' || true)"
    log "hub journal: ${MIGR_ERR} migration_error, ${PANIC} panic, ${WARN_N} WARN, ${ERROR_N} ERROR"
    [ "${MIGR_ERR}" = "0" ] || hard "hub migration_error in journal"
    [ "${PANIC}" = "0" ] || hard "hub panic in journal"
    check_hub_levels
    if ! WORKER_ERRLOG="$(journalctl --user -u "${WORKER_UNIT}" --since -30min --no-pager)"; then
        hard "cannot read worker journal"
        WORKER_ERRLOG=""
    fi
    WORKER_ERR="$(printf '%s' "${WORKER_ERRLOG}" | grep -cE 'journal_refused|journal_stopped|state_divergence|ownership not verified|lost ownership|execution_fact_rejected|journal_record_mismatch|attach_rejected|journal_withheld|auth_required|local_state_reset|supervisor_protocol_incompatible|untested_cli_version|codexbar_poll_failed|runtime sweep failed|worktree build gc failed|panic|ERROR' || true)"
    log "worker journal errors: ${WORKER_ERR}"
    [ "${WORKER_ERR}" = "0" ] || hard "worker journal errors: ${WORKER_ERR}"
    if ! live_summary="$(hub_psql "select string_agg(left(id::text,8) || ':' || state, ' ' order by id) from executions where state='running'")"; then
        hard "cannot summarize live executions"
        live_summary="UNKNOWN"
    fi
    if ! live_ids="$(hub_psql "select id::text from executions where state='running' order by 1")"; then
        hard "cannot list live executions"
        live_ids=""
    fi
    log "live executions: ${live_summary:-none}"
    for exec_id in ${live_ids:-}; do
        log "exec ${exec_id}: sup=[$(proc_ticks "${exec_id}" | grep supervise | sed -n 1p)]"
        SJ="$(read_ack "${WORKER_DATA}/executions/${exec_id}/sj/ack" || echo unreadable)"
        if [ "${SJ}" = "unreadable" ] || [ -z "${SJ}" ]; then
            ack_gone_is_end "${exec_id}" "sj ack not readable" || hard "cannot read sj ack for ${exec_id}"
        else
            log "exec ${exec_id}: sj_ack=${SJ}"
        fi
    done
    CJ="$(read_ack "${WORKER_DATA}/cj/ack" || echo unreadable)"
    if [ "${CJ}" = "unreadable" ] || [ -z "${CJ}" ]; then
        hard "cannot read cj ack"
    else
        log "cj_ack=${CJ}"
    fi
    CONTAINER_SHA="$(container_pwa_sha)"
    if ! sha_match "${CONTAINER_SHA:-}" "${WEB_SHA}"; then
        hard "pwa version.json is not ${WEB_SHA} (container says ${CONTAINER_SHA:-unreadable})"
    else
        log "pwa version.json ok (${CONTAINER_SHA} matches ${WEB_SHA})"
    fi
    # CSP в контейнере обязан равняться env-файлу (CSP публичен в заголовках, не секрет).
    # Провал чтения — жёсткая ошибка: два UNREADABLE не должны сравниться как равные.
    ENV_CSP_READ=0
    CONTAINER_CSP_READ=0
    ENV_CSP=""
    CONTAINER_CSP=""
    if ! ENV_RAW="$(ssh_web "cat '${WEB_ENV}'")"; then
        hard "cannot read web env CSP from ${WEB_ENV}"
    else
        ENV_CSP="$(printf '%s' "${ENV_RAW}" | sed 's/^CANTERLOT_CSP="//; s/"$//')"
        ENV_CSP_READ=1
    fi
    if ! CONTAINER_CSP="$(ssh_web "docker exec caddy printenv CANTERLOT_CSP")"; then
        hard "cannot read container CSP"
    else
        CONTAINER_CSP_READ=1
    fi
    if [ "${ENV_CSP_READ}" = "1" ] && [ "${CONTAINER_CSP_READ}" = "1" ]; then
        if [ "${CONTAINER_CSP}" = "${ENV_CSP}" ]; then
            log "container CSP matches env file"
        else
            hard "container CSP differs from env file"
        fi
    fi
    if [ "${WEB_SHA}" != "${HUB_SHA}" ]; then
        log "no dist CSP compare: PWA is on ${WEB_SHA}, hub on ${HUB_SHA} (the container is compared with the env file only)"
    elif [ "${CONTAINER_CSP_READ}" != "1" ]; then
        log "dist CSP compare skipped: the container CSP was not readable (already counted)"
    elif [ ! -d "${DIST_HUB}" ] && [ "${BASELINE}" = "1" ]; then
        # Dry-run смотрит на развёрнутую базу, чьего dist в сборочном worktree может не
        # быть. Тогда сверяем контейнер с записью самого развёрнутого релиза
        # (canterlot.env.<sha>, её CSP пишет cutover из dist этого релиза); значения не
        # печатаются. Нет и записи — SKIPPED с причиной, а не провал здорового целевого релиза.
        if REL_RAW="$(ssh_web "cat '${WEB_ENV}.${WEB_SHA}'" 2>/dev/null)"; then
            REL_CSP="$(printf '%s' "${REL_RAW}" | sed 's/^CANTERLOT_CSP="//; s/"$//')"
            if [ -n "${REL_CSP}" ] && [ "${CONTAINER_CSP}" = "${REL_CSP}" ]; then
                log "container CSP matches the deployed release's own record ${WEB_ENV##*/}.${WEB_SHA} (no dist ${DIST_HUB} to compare)"
            else
                hard "container CSP differs from the deployed release's own record ${WEB_ENV##*/}.${WEB_SHA}"
            fi
        else
            log "SKIPPED: container CSP vs dist (no ${DIST_HUB}, and no release record ${WEB_ENV##*/}.${WEB_SHA}); the container was compared with the env file only"
        fi
    elif [ ! -d "${DIST_HUB}" ]; then
        not_run "container CSP vs dist (no ${DIST_HUB})"
    else
        DIST_CSP=""
        if ! DIST_CSP="$(cat "${DIST_HUB}/csp.txt" 2>/dev/null)" || [ -z "${DIST_CSP}" ]; then
            hard "cannot read dist CSP from ${DIST_HUB}/csp.txt"
        elif [ "${CONTAINER_CSP}" = "${DIST_CSP}" ]; then
            log "container CSP matches dist"
        else
            hard "container CSP differs from dist"
        fi
    fi
    check_precompressed
    # Коды остальных сайтов (только коды, без тел и заголовков).
    # shellcheck disable=SC2086
    for pair in ${SITE_CODES}; do
        site="${pair%%:*}"; want="${pair##*:}"
        code="$(curl -sS -o /dev/null -w '%{http_code}' -m 10 "https://${site}/" 2>/dev/null || echo FAIL)"
        [ "${code}" = "${want}" ] || hard "site ${site}: ${code}, expected ${want}"
        log "site ${site}: ${code}"
    done
}

verify_read "1"
[ -n "${STATE_FILE}" ] || log "no --state given: ticks checked against own snapshot only"
if [ -n "${STATE_FILE}" ]; then
    [ -f "${STATE_FILE}" ] || hard "state file missing: ${STATE_FILE}"
    [ -f "${STATE_FILE}" ] && check_ticks_file "${STATE_FILE}" "read 1"
fi
take_own_snapshot
snap_acks
# Пауза между чтениями настраивается (VERIFY_READ_GAP); боевое значение — 60 с.
VERIFY_READ_GAP="${VERIFY_READ_GAP:-60}"
log "waiting ${VERIFY_READ_GAP}s between reads"
# VERIFY_GAP_RELEASE (только для тестов): пауза кончается раньше, как только этот
# файл появится, — событие вместо гонки с фиксированным окном.
if [ -n "${VERIFY_GAP_RELEASE:-}" ]; then
    GAP_TICKS=0
    while [ ! -e "${VERIFY_GAP_RELEASE}" ] && [ "${GAP_TICKS}" -lt $((VERIFY_READ_GAP * 20)) ]; do
        sleep 0.05
        GAP_TICKS=$((GAP_TICKS + 1))
    done
else
    sleep "${VERIFY_READ_GAP}"
fi
verify_read "2"
if [ -n "${STATE_FILE}" ] && [ -f "${STATE_FILE}" ]; then
    check_ticks_file "${STATE_FILE}" "read 2"
fi
check_ticks_own "read 2"
compare_acks

if [ -n "${NOT_RUN_LIST}" ]; then
    if [ "${ALLOW_SKIPPED}" = "1" ]; then
        log "comparisons NOT RUN were explicitly allowed (--allow-skipped)"
    else
        hard "comparisons NOT RUN (use --allow-skipped only to acknowledge a known gap)"
    fi
fi
check_summary && log "verify PASS"
