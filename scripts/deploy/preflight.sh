#!/usr/bin/env bash
# При sourcing ничего не исполнять: скрипт запускает боевой выкат только как
# программа (урок 1.16.1: source cutover-скрипта остановил воркер).
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Обязательные проверки перед выкаткой: инвентаризация живых исполнений из трёх
# источников (хаб, процессы сопровождения в /proc, неосвобождённые записи ownership);
# PIDs и стартовые тики CLI и релея; spec.toml против фикстур, spec_compat и декодирование
# ownership новым ipc.proto; имена ВСЕХ таблиц и ключей worker.toml и hub.toml против
# структур нового исходника (и speech.toml, если он есть: мягко); нет висячих stop/interrupt; нет открытой компактификации;
# KillMode=process; нет vzdump и блокировки CT (провал осмотра — жёсткая ошибка);
# новые миграции с пометкой о риске блокировок.
# Использование: scripts/deploy/preflight.sh SHA --source DIR [--dist DIR] [--state FILE] [--dry-run]
# --dry-run: SHA цели обязателен (развёрнутый релиз никогда не подставляется молча);
#   все проверки только читают, spec_compat выполняется по-настоящему против каждого
#   релиза живого супервизора (пишет лишь артефакты сборки cargo в SOURCE/target).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

SHA=""
SOURCE_DIR=""
DIST_BASE=""
STATE_FILE=""
DRY_RUN=0
while (($#)); do
    case "$1" in
        --source) SOURCE_DIR="$2"; shift 2 ;;
        --dist) DIST_BASE="$2"; shift 2 ;;
        --state) STATE_FILE="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -*) die "unknown flag: $1" ;;
        *) SHA="$(short_sha "$1")"; shift ;;
    esac
done

# Цель не подменяется развёрнутым релизом: в dry-run проверяется именно SHA цели
# (её dist и миграции), а живые сессии и конфиги читаются как есть.
if [ "${DRY_RUN}" = "1" ] && [ -z "${SHA}" ]; then
    die "dry-run needs the TARGET sha: preflight.sh SHA --dry-run (the deployed release is never substituted)"
fi
[ -n "${SHA}" ] || die "usage: preflight.sh SHA --source DIR [--dist DIR] [--state FILE] [--dry-run]"
require_release_id "${SHA}"
[ "${DRY_RUN}" = "0" ] || log "dry-run mode: checking TARGET ${SHA} (deployed hub is $(deployed_sha hub), the baseline); read-only"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SOURCE_DIR="${SOURCE_DIR:-${REPO_ROOT}}"
DIST="${DIST_BASE:-${REPO_ROOT}/dist}/${SHA}"
XDG_RUNTIME_DIR="/run/user/$(id -u)"
export XDG_RUNTIME_DIR

# Провал осмотра — громкая смерть, а не пустой результат (урок: пустота читалась как «всё чисто»).
hub_psql() { ssh_hub "runuser -u canterlot -- psql -h '${HUB_PG_SOCKET}' -d '${HUB_PG_DB}' -tAc \"$1\"" || die "hub DB query failed"; }

log "gate 1: live executions from hub, /proc supervisors and ownership journal"
# Каждый живой супервизор из /proc, а не только строки хаба (урок: пустой список
# при живых процессах проходил всухую). Супервизор, чья строка ещё переходная
# (created/starting, а также stopping/owner_lost), ждём до SUPERVISOR_SETTLE_WAIT
# секунд: делегат, стартовавший за миг до preflight, не ложный HARD (урок 1.24.0).
# Хаб и /proc перечитываются вместе, снимок ниже строится по последнему чтению.
settle_supervisor_rows "${WORKER_DATA}/executions" || die "hub DB query failed while reconciling supervisors"
LIVE_EXECS="${SETTLED_LIVE}"
PROC_IDS="${SETTLED_PROC}"
[ -n "${LIVE_EXECS}" ] || log "no live executions in hub (first deploy or idle hub)"
# shellcheck disable=SC2086 # newline list, intentional word splitting
printf 'live execution: %s\n' ${LIVE_EXECS:-none}
LIVE_CONVS="$(hub_psql "select distinct conversation_id::text from executions where state='running'")"
[ -n "${PROC_IDS}" ] && log "supervisors in /proc:" && printf '%s\n' "${PROC_IDS}" | sed 's/^/  supervise: /'
# Каждую неосвобождённую запись ownership на диске декодирует новый ipc.proto
# (номера полей известны, значения не печатаются).
OWNERSHIP_OUT="$(python3 "${SCRIPT_DIR}/ownership-check.py" \
    "${SOURCE_DIR}/crates/canterlot-proto/proto/canterlot/worker/v1/ipc.proto" \
    "${WORKER_DATA}" 2>&1)" && OWNERSHIP_RC=0 || OWNERSHIP_RC=$?
printf "%s\n" "${OWNERSHIP_OUT%%$'\n'*}"
if [ "${OWNERSHIP_RC}" -ne 0 ]; then
    hard "ownership journal decode failed: $(printf '%s' "${OWNERSHIP_OUT}" | tr '\n' ';' | cut -c1-300)"
    OWNERSHIP_UNRELEASED=""
else
    OWNERSHIP_UNRELEASED="$(printf '%s\n' "${OWNERSHIP_OUT}" | awk '$1 == "UNRELEASED" {print $2}')"
fi
# Супервизор без строки running: при переходной строке (ожидание выше истекло) —
# мягко, с состоянием; без строки или с терминальной строкой — жёстко. Такой
# супервизор не входит в снимок тиков: cutover его не сторожит, как и супервизор,
# появившийся после preflight (snapshot_drift сверяет только записанные процессы).
TRANSIENT_IDS=""
UNEXPLAINED_IDS=""
while read -r extra_id extra_state; do
    [ -n "${extra_id:-}" ] || continue
    if is_transient_exec_state "${extra_state}"; then
        TRANSIENT_IDS+="${extra_id}"$'\n'
        soft "supervisor runs for execution ${extra_id} whose hub row is still ${extra_state} after ${SUPERVISOR_SETTLE_WAIT} s (not in the tick snapshot)"
    else
        UNEXPLAINED_IDS+="${extra_id}"$'\n'
        hard "supervisor runs for execution ${extra_id} with no live hub row (hub row: ${extra_state})"
    fi
done <<<"${SETTLED_EXTRA}"
# Пустой список хаба при живых процессах — жёсткая ошибка, а не пропуск. Переходные
# строки при живом супервизоре уже записаны мягко выше.
UNEXPLAINED_OWN=""
for own_id in ${OWNERSHIP_UNRELEASED:-}; do
    in_lines "${own_id}" "${TRANSIENT_IDS}" || UNEXPLAINED_OWN+="${own_id}"$'\n'
done
if [ -z "${LIVE_EXECS}" ] && { [ -n "${UNEXPLAINED_IDS}" ] || [ -n "${UNEXPLAINED_OWN}" ]; }; then
    hard "hub reports no live executions while supervisors or unreleased ownership exist"
fi
# Живая строка без супервизора — жёстко.
# shellcheck disable=SC2086
for exec_id in ${LIVE_EXECS:-}; do
    in_lines "${exec_id}" "${PROC_IDS}" \
        || hard "live hub execution ${exec_id} has no supervisor process"
done
# Неосвобождённая запись без строки хаба и без процесса — утечка: жёстко.
# Запись супервизора с переходной строкой уже учтена мягко выше.
# shellcheck disable=SC2086
for own_id in ${OWNERSHIP_UNRELEASED:-}; do
    if ! in_lines "${own_id}" "${LIVE_EXECS}"; then
        if in_lines "${own_id}" "${TRANSIENT_IDS}"; then
            log "unreleased ownership for ${own_id} belongs to a supervisor with a transitional hub row (reported above)"
        elif ! in_lines "${own_id}" "${PROC_IDS}"; then
            hard "orphan unreleased ownership for ${own_id}: no hub row, no supervisor"
        else
            soft "unreleased ownership for ${own_id} with supervisor but no hub row"
        fi
    fi
done

# Снимок PIDs и стартовых тиков: сопровождение, CLI и релей отдельно;
# PID воркера — отдельным множеством (урок: остановленный воркер ронял проверку тиков).
SNAPSHOT=""
for exec_id in ${LIVE_EXECS:-}; do
    EXEC_DIR="${WORKER_DATA}/executions/${exec_id}"
    [ -d "${EXEC_DIR}" ] || { hard "exec dir missing for live execution ${exec_id}"; continue; }
    # Точное совпадение: любой агентский процесс (playwright-профиль в exec-dir)
    # может содержать exec id в командной строке и затмить сопровождение в head -1.
    SUP_LINE="$(proc_ticks "${exec_id}" | grep supervise | grep -v mcp-relay | sed -n 1p)"
    [ -n "${SUP_LINE}" ] || { hard "no supervisor process for live execution ${exec_id}"; continue; }
    SUP_PID="${SUP_LINE%% *}"
    SUP_TICK="$(printf '%s' "${SUP_LINE}" | cut -d' ' -f2)"
    SUP_EXE="$(readlink "/proc/${SUP_PID}/exe")"
    # Релеи — все настоящие (точная подкоманда mcp-relay и exec id в --socket), а не
    # первая строка с подстрокой: приманка в argv иначе подменяла бы настоящий релей.
    REL_LINE=""
    REL_FIELD=""
    for rel_pid in $(relay_pids_for_exec "${exec_id}"); do
        rel_tick="$(tr ' ' '\n' <"/proc/${rel_pid}/stat" 2>/dev/null | sed -n '22p')"
        REL_LINE+="${REL_LINE:+, }${rel_pid} ${rel_tick:-?}"
        REL_FIELD+="${REL_FIELD:+,}${rel_pid}:${rel_tick:-?}"
    done
    # CLI — точная роль (прямой ребёнок сопровождения, самый старший, не релей),
    # а не любой процесс; тот же помощник использует verify.
    CLI_LINE="$(session_cli_pid "${SUP_PID}")"
    [ -n "${REL_LINE}" ] || soft "no mcp-relay for live execution ${exec_id}"
    [ -n "${CLI_LINE}" ] || { hard "no CLI child of supervisor ${SUP_PID} for ${exec_id}"; continue; }
    log "exec ${exec_id}: supervisor ${SUP_LINE} exe=${SUP_EXE}; CLI ${CLI_LINE}; relay ${REL_LINE:-none}"
    SNAPSHOT+="${exec_id} sup=${SUP_PID}:${SUP_TICK} cli=$(printf '%s' "${CLI_LINE}" | tr ' ' ':') relay=${REL_FIELD:-none} sup_exe=${SUP_EXE}"$'\n'
done

WORKER_PID="$(systemctl --user show "${WORKER_UNIT}" -p MainPID --value 2>/dev/null || true)"
if [ -n "${WORKER_PID}" ] && [ "${WORKER_PID}" != "0" ]; then
    WORKER_TICK="$(tr ' ' '\n' <"/proc/${WORKER_PID}/stat" 2>/dev/null | sed -n '22p')"
    log "worker control service PID ${WORKER_PID} tick ${WORKER_TICK}"
    SNAPSHOT+="worker pid=${WORKER_PID}:${WORKER_TICK:-?}"$'\n'
fi
if [ -n "${STATE_FILE}" ] && [ "${DRY_RUN}" = "0" ]; then
    printf '%s' "${SNAPSHOT}" >"${STATE_FILE}"
    printf '%s\n' "${PROC_IDS}" >"${STATE_FILE}.procids"
    log "snapshot written to ${STATE_FILE} (+ .procids)"
fi

log "gate 2: spec.toml shapes and binary compatibility"
# Живая спека обязана совпасть с известной формой (урок: формы разных релизов отличаются
# отставными ключами; эталон — не всегда самый новый файл) и быть допустима структурам
# ЦЕЛИ. Необязательные ключи, которые цель знает (например opencode.provider_models с
# 1.4.0), форму не ломают: допустимые ключи выводит spec-shape.py из кода цели.
FIXTURES="$(find "${SOURCE_DIR}/crates/canterlot-worker/tests/it/fixtures/spec" -maxdepth 1 -name '*.toml' -printf '%p\n' | sort)"
[ -n "${FIXTURES}" ] || die "no spec fixtures in ${SOURCE_DIR}"
for exec_id in ${LIVE_EXECS:-}; do
    SPEC="${WORKER_DATA}/executions/${exec_id}/spec.toml"
    # shellcheck disable=SC2086 # newline list, intentional word splitting
    if SHAPE_OUT="$(python3 -I "${SCRIPT_DIR}/spec-shape.py" \
            "${SOURCE_DIR}/crates/canterlot-worker/src/supervisor/mod.rs" \
            "${SOURCE_DIR}/crates/canterlot-worker/src/config.rs" "${SPEC}" ${FIXTURES} 2>&1)"; then
        log "spec ${exec_id}: ${SHAPE_OUT}"
    else
        hard "spec ${exec_id} matches no known fixture shape: $(printf '%s' "${SHAPE_OUT}" | tr '\n' ';' | cut -c1-400)"
    fi
done
# spec_compat с бинарником каждого релиза, из которого живёт сопровождение
# (урок: короткая TMPDIR, иначе путь сокета длиннее 108 байт).
OLD_BINS="$(printf '%s\n' "${SNAPSHOT}" | grep -o 'sup_exe=[^ ]*' | cut -d= -f2 | sort -u || true)"
for old_bin in ${OLD_BINS:-}; do
    [ -x "${old_bin}" ] || { hard "old supervisor binary not executable: ${old_bin}"; continue; }
    # spec_compat только читает (бинарь старого релиза и собственные тест-каталоги в
    # коротком TMPDIR), поэтому в dry-run он ВЫПОЛНЯЕТСЯ: пишутся только артефакты
    # сборки cargo в SOURCE/target, ни одного хоста и ни одной сессии это не касается.
    log "spec_compat against ${old_bin}"
    (
        cd "${SOURCE_DIR}"
        # shellcheck source=/dev/null
        source ~/.cargo/env
        CANTERLOT_OLD_RELEASE_BIN="${old_bin}" TMPDIR=/tmp \
            cargo test --locked -p canterlot-worker --all-features --test it spec_compat::
    ) || hard "spec_compat failed against ${old_bin}"
done

log "gate 3: every table and key of worker.toml, hub.toml and speech.toml against the new structs"
if ! python3 "${SCRIPT_DIR}/config-check.py" \
    "${SOURCE_DIR}/crates/canterlot-worker/src/config.rs" WorkerConfig \
    "${WORKER_CONFIG}"; then
    hard "worker.toml has unknown tables or keys"
fi
# hub.toml читается конвейером, без файлов (секретов в нём нет, только имена файлов).
if ! ssh_hub "cat '${HUB_CONFIG}'" | python3 "${SCRIPT_DIR}/config-check.py" \
    "${SOURCE_DIR}/crates/canterlot-hub/src/config.rs" HubConfig /dev/stdin; then
    hard "hub.toml has unknown tables or keys"
fi
# speech.toml службы диктовки: та же строгость, что у самой службы (неизвестные ключи,
# обязательные ключи, sha256 модели и VAD — 64 hex). Нет файла — диктовка выключена.
# Сбой только печатается (SPEECH-SOFT): диктовка выкат не останавливает.
SPEECH_RS="${SOURCE_DIR}/crates/canterlot-speech/src/config.rs"
if [ ! -f "${SPEECH_RS}" ]; then
    log "speech.toml: the release checkout has no canterlot-speech, not checked"
elif ! ssh_hub "test -e $(shq "${SPEECH_CONFIG}")"; then
    log "speech.toml: no ${SPEECH_CONFIG} on the hub, dictation off (fine)"
elif ssh_hub "cat $(shq "${SPEECH_CONFIG}")" | python3 "${SCRIPT_DIR}/config-check.py" --require \
        --hex64 model.sha256 --hex64 vad.sha256 "${SPEECH_RS}" Config /dev/stdin; then
    log "speech.toml: OK against the new canterlot-speech Config"
else
    speech_soft "${SPEECH_CONFIG} does not match the new canterlot-speech Config (see above): the service will refuse to start"
fi

log "gate 4: no pending stop or interrupt, no open compaction"
if [ -n "${LIVE_CONVS}" ]; then
    # shellcheck disable=SC2086 # LIVE_CONVS is a newline list, intentional word splitting
    CONV_LIST="$(printf "'%s'," ${LIVE_CONVS} | sed 's/,$//')"
    OPEN_SI="$(hub_psql "select count(*) from commands where conversation_id in (${CONV_LIST}) and kind in ('interrupt','stop_execution') and delivery_state != 'completed'")"
    [ "${OPEN_SI}" = "0" ] || hard "open stop/interrupt commands on live executions: ${OPEN_SI}"
else
    OPEN_SI=0
fi
log "open stop/interrupt on live: ${OPEN_SI}"
COMPACTIONS_TABLE=""
if ! COMPACTIONS_TABLE="$(hub_psql "select count(*) from information_schema.tables where table_name='compactions'")"; then
    hard "cannot query compactions table"
elif [ "${COMPACTIONS_TABLE}" = "1" ]; then
    OPEN_COMP="$(hub_psql "select count(*) from compactions where finished_at is null")"
    [ "${OPEN_COMP}" = "0" ] || hard "open compactions: ${OPEN_COMP}"
    log "open compactions: ${OPEN_COMP}"
else
    log "no compactions table yet"
fi

log "gate 5: worker KillMode, no vzdump, no CT lock"
[ "$(systemctl --user show "${WORKER_UNIT}" -p KillMode --value 2>/dev/null)" = "process" ] \
    || hard "worker unit KillMode is not process"
log "worker KillMode=process"
if assert_no_remote_proc "${PROXMOX_SSH}" "vzdump" "vzdump"; then
    log "no vzdump running"
elif [ "$?" -eq 2 ]; then
    hard "cannot inspect for vzdump on ${PROXMOX_SSH}"
else
    hard "vzdump running on ${PROXMOX_SSH}"
fi
CT_LOCK=""
CT_STATUS=""
if ! CT_NODE_OUT="$(check_ct_node 2>&1)"; then
    hard "${CT_NODE_OUT}"
else
    log "${CT_NODE_OUT}"
    if ! CT_STATUS="$(ssh_pmx "pvesh get /nodes/${PROXMOX_NODE}/lxc/${HUB_CTID}/status/current --output-format json")"; then
        hard "cannot query CT ${HUB_CTID} lock state (pvesh/ssh failed)"
    else
        CT_LOCK="$(printf '%s' "${CT_STATUS}" | grep -o 'lock[^,]*' | sed -n 1p || true)"
        if [ -n "${CT_LOCK}" ]; then
            hard "CT ${HUB_CTID} locked: ${CT_LOCK}"
        else
            log "no CT lock"
        fi
    fi
fi

log "gate 6: new migrations with a lock risk note"
DEPLOYED_HUB_SHA="$(deployed_sha hub)"
OLD_MIGR="$(ssh_hub "grep '^migrations_last=' $(shq "${HUB_RELEASES}/${DEPLOYED_HUB_SHA}/RELEASE")" | cut -d= -f2)" \
    || die "cannot read deployed RELEASE"
NEW_MIGR="$(grep '^migrations_last=' "${DIST}/RELEASE" 2>/dev/null | cut -d= -f2 || find "${SOURCE_DIR}/crates/canterlot-hub/migrations" -maxdepth 1 -name '*.sql' -printf '%f\n' | sort | tail -1)"
log "migrations: deployed ends at ${OLD_MIGR}, new ends at ${NEW_MIGR}"
if [ "${OLD_MIGR}" != "${NEW_MIGR}" ]; then
    for mig in $(find "${SOURCE_DIR}/crates/canterlot-hub/migrations" -maxdepth 1 -name '*.sql' -printf '%f\n' | sort | awk -v old="${OLD_MIGR}" '$0 > old'); do
        RISKS="$(grep -ioE 'lock[^;]*|drop[^;]*|create +(unique +)?index[^;]*' "${SOURCE_DIR}/crates/canterlot-hub/migrations/${mig}" | tr '\n' ';' || true)"
        log "new migration ${mig} lock-risk: ${RISKS:-none spotted (hub is down during migration, no writers)}"
    done
else
    log "no new migrations"
fi

# Ежедневное PBS-задание в 03:30 чистит старые vzdump (keep-last=1): не стартовать рядом.
UTC_HOUR="$(date -u +%H:%M)"
if [[ "${UTC_HOUR}" > "02:45" && "${UTC_HOUR}" < "03:45" ]]; then
    hard "inside the 03:30 backup window (${UTC_HOUR} UTC); wait until 03:45"
fi
log "outside the 03:30 backup window (${UTC_HOUR} UTC)"

[ "${SPEECH_ISSUES}" = "0" ] || log "speech: ${SPEECH_ISSUES} speech soft issue(s) above (dictation only, not counted)"
check_summary && log "preflight PASS for ${SHA}"
