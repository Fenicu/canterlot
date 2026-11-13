#!/usr/bin/env bash
# shellcheck disable=SC2016 # remote scripts expand their own positional args
# При sourcing ничего не исполнять: скрипт запускает боевой выкат только как
# программа (урок 1.16.1: source cutover-скрипта остановил воркер).
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Выкат релиза одним прогоном: ворота и общая предпроверка, свежий vzdump при
# работающих хабе и воркере, ворота и инвентарь сессий заново, стоп воркера,
# pg_dump при остановленном воркере (проверенный чтением каждого байта), flip и
# рестарт хаба, flip и старт воркера до state="live", затем flip PWA и один
# recreate Caddy (мягко), диктовка, слияние старых чанков PWA, обслуживание,
# проверка, чистка.
#
# Три режима, один путь:
#   cutover.sh SHA --dry-run   [--shadow] [--allow-skipped] [--source DIR] [--dist DIR]
#       только чтение: preflight и target-check против ЦЕЛИ, verify против
#       развёрнутой базы, обслуживание, план; печатает токен подтверждения.
#   cutover.sh SHA --rehearse  [--shadow] [--source DIR] [--dist DIR] [--state FILE]
#       репетиция: ВЕСЬ путь записи против живой системы, но каждая запись идёт
#       через run_write и только печатается (WOULD-WRITE); все чтения, ожидания,
#       разборы и циклы — настоящие, включая ожидание старта воркера с пустым
#       MainPID, MainPID=0 и ENOENT на переходе exec. Общая с боевым прогоном
#       предпроверка — preflight и target-check цели, поэтому репетиция строгое
#       надмножество dry-run. Печатает токен.
#   cutover.sh --execute SHA --confirm TOKEN [--source DIR] [--dist DIR] [--state FILE]
#       боевой прогон. Токен — из dry-run или репетиции того же релиза и того же
#       плана (хэш plan_text), не старше 2 часов.
#
# Политика отката (решение владельца: никакой автоматики по базе), по записанным
# фактам (vzdump, стоп воркера, проверенный дамп, ссылка хаба, живой новый воркер):
# - До flip хаба: любая неудача или сигнал — только рестарт воркера на его
#   собственном предыдущем релизе (хаб не тронут, пара заведомо совместимая);
#   если воркер ещё не останавливали (vzdump и повторные ворота), — ничего.
# - С flip хаба и дальше: СТОП и доклад. Никаких действий с базой, никакого
#   flip назад; воркер останавливаем и оставляем остановленным (его сессии живут
#   в сопровождениях). Печатаем точное состояние и ручные команды восстановления
#   (таблица Task 12), включая восстановление дампа этого прогона.
#   Ручной откат базы — только scripts/deploy/rollback-db.sh, никогда отсюда.
# - Откат хаба на релиз без собственного входа (1.29 и раньше): сначала вернуть
#   блок входа в Caddy (basic_auth или forward_auth), потом flip. Старый хаб верит
#   Remote-User от прокси с секретом, и без блока входа его подделка — вход владельцем.
#   Перед стартом старого хаба из hub.toml убираются [auth] и
#   [http.ws].session_check_interval (deny_unknown_fields: иначе он не стартует), а
#   ждущие пуши входа отменяются: старый хаб не читает строку пуша без разговора.
#   Воркер ниже этого релиза не стартует с [execution].delegate_mcp_servers.
# - PWA идёт после доказанно живого воркера; её ошибки мягкие всегда: записать,
#   вернуть предыдущий build, продолжить; воркер ради неё не гасится никогда.
# - Диктовка (canterlot-speech) мягкая всегда: после старта воркера юнит
#   перезапускается (не дольше SPEECH_RESTART_TIMEOUT) или, если в релизе нет
#   бинаря, останавливается и выключается; итог только в отчёте, токен и откат
#   от неё не зависят.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

SHA=""
EXECUTE_ID=""
CONFIRM=""
SOURCE_DIR=""
DIST_BASE=""
STATE_FILE=""
DRY_RUN=0
REHEARSE=0
ALLOW_SKIPPED=0
SHADOW=0
ALLOW_SAME_TARGET=0
PLAN_HASH=""
PHASE=""
# --- файл результата: машиночитаемый итог любого выхода (его читает release.sh) ---
# Путь: --result FILE; иначе рядом с логом, если stdout — файл; иначе в $TMPDIR.
RESULT_FILE=""
RESULT_WRITTEN=0
RESULT_SIGNAL=""
RESULT_TOKEN=""
RESULT_TOKEN_UNTIL=""
RESULT_RECOVERY=""
RESULT_STOP_BLOCK=""
RESULT_STARTED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
RESULT_LOG="$(readlink "/proc/$$/fd/1" 2>/dev/null)" || RESULT_LOG=""
[ -f "${RESULT_LOG}" ] || RESULT_LOG=""
DEPLOY_SOFT_LOG=""
VZDUMP_STARTED=0
VZDUMP_DONE=0
WORKER_STOPPED=0
DUMP_VERIFIED=0
HUB_FLIPPED=0
WORKER_PROVEN=0
result_mode() {
    if [ "${DRY_RUN:-0}" = "1" ]; then printf dry-run
    elif [ "${REHEARSE:-0}" = "1" ]; then printf rehearse
    else printf execute; fi
}
result_path() {
    if [ -n "${RESULT_FILE}" ]; then return 0; fi
    if [ -n "${RESULT_LOG}" ]; then
        RESULT_FILE="${RESULT_LOG%.log}.result"
    else
        RESULT_FILE="${TMPDIR:-/tmp}/canterlot-cutover-${SHA:-unknown}-$(result_mode)-$(date -u +%Y%m%dT%H%M%SZ).result"
    fi
}
# Мягкие ошибки этого прогона и дочерних скриптов (verify, retain) пишутся по одной
# строке в файл рядом с результатом: soft() — вид шага, speech_soft() — speech.
result_soft_log_start() {
    result_path
    DEPLOY_SOFT_LOG="${RESULT_FILE}.soft"
    export DEPLOY_SOFT_LOG
    : >"${DEPLOY_SOFT_LOG}"
}
result_soft_kind() {
    case "$1" in
        pwa | pwa-done) printf pwa ;;
        merging-chunks) printf chunk_merge ;;
        verifying) printf verify ;;
        worker-*) printf worker ;;
        *) printf '%s' "$1" ;;
    esac
}
one_line() { printf '%s' "$1" | tr '\n\r=' '  :'; }
# $1 статус, $2 код выхода. Пишется один раз; провал записи только печатается.
write_result() {
    local status="$1" code="$2" soft_n=0 speech_n=0 kinds="" k n restore=0
    [ "${RESULT_WRITTEN}" = "0" ] || return 0
    RESULT_WRITTEN=1
    result_path
    if [ -n "${DEPLOY_SOFT_LOG}" ] && [ -f "${DEPLOY_SOFT_LOG}" ]; then
        speech_n="$(grep -cx speech "${DEPLOY_SOFT_LOG}" || true)"
        soft_n="$(grep -cvx speech "${DEPLOY_SOFT_LOG}" || true)"
        while read -r n k; do
            [ -n "${k:-}" ] || continue
            kinds="${kinds:+${kinds},}${k}:${n}"
        done < <(grep -vx speech "${DEPLOY_SOFT_LOG}" | while read -r k; do result_soft_kind "${k}"; printf '\n'; done | LC_ALL=C sort | uniq -c)
    fi
    # Простой и точки восстановления — только у боевого прогона: репетиция ничего не останавливает.
    if [ "$(result_mode)" = "execute" ]; then restore=1; fi
    {
        printf '%s=%s\n' \
            format 1 \
            mode "$(result_mode)" \
            status "${status}" \
            exit_code "${code}" \
            signal "${RESULT_SIGNAL}" \
            release "${SHA:-}" \
            plan "${PLAN_HASH:-}" \
            token "${RESULT_TOKEN}" \
            token_valid_until "${RESULT_TOKEN_UNTIL}" \
            started_at "${RESULT_STARTED}" \
            finished_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            phase "${PHASE:-}" \
            vzdump_done "$(yn "$((VZDUMP_DONE * restore))")" \
            worker_stopped "$(yn "$((WORKER_STOPPED * restore))")" \
            dump_verified "$(yn "$((DUMP_VERIFIED * restore))")" \
            hub_flipped "$(yn "$((HUB_FLIPPED * restore))")" \
            worker_live "$(yn "$((WORKER_PROVEN * restore))")" \
            recovery "${RESULT_RECOVERY}" \
            hard_failures "${HARD_FAILURES:-0}" \
            soft_failures "${soft_n}" \
            soft_by_kind "${kinds}" \
            speech_soft_failures "${speech_n}" \
            speech_summary "$(one_line "${SPEECH_SUMMARY:-}")" \
            web_release "${WEB_SHA_EFF:-}" \
            worker_downtime_s "$([ "${restore}" = "1" ] && printf '%s' "${WORKER_DOWNTIME:-}")" \
            hub_interruption_s "$([ "${restore}" = "1" ] && printf '%s' "${HUB_INTERRUPTION:-}")" \
            worker_stop_at "$([ "${restore}" = "1" ] && printf '%s' "${WORKER_STOP_ISO:-}")" \
            vzdump_notes "$([ "$((VZDUMP_DONE * restore))" = "1" ] && printf 'pre-%s-%s' "${SHA}" "${RUN_TS}")" \
            vzdump_archive "$([ "$((VZDUMP_DONE * restore))" = "1" ] && printf '%s' "${VZ_ARCHIVE:-}")" \
            dump "$([ "$((DUMP_VERIFIED * restore))" = "1" ] && printf '%s/%s' "${HUB_BACKUP_DIR}" "${RUN_DUMP}")" \
            stop_block "${RESULT_STOP_BLOCK}" \
            log "${RESULT_LOG}"
    } >"${RESULT_FILE}" 2>/dev/null || log "could not write the result file ${RESULT_FILE}"
    if [ -n "${DEPLOY_SOFT_LOG}" ]; then rm -f "${DEPLOY_SOFT_LOG}"; fi  # lint: local-temp
    log "result file: ${RESULT_FILE} (status ${status})"
}
yn() { if [ "$1" = "1" ]; then printf yes; else printf no; fi; }
# Выход до ловушки прогона (аргументы, токен, предпроверка, ворота): ничего на
# проде не тронуто — боевой прогон отказал, dry-run и репетиция не прошли.
result_exit() {
    local rc=$?
    trap '' INT TERM HUP
    set +e
    if declare -F release_reservations >/dev/null; then release_reservations; fi
    if [ -n "${REHEARSE_COUNT_FILE:-}" ]; then rm -f "${REHEARSE_COUNT_FILE}"; fi  # lint: local-temp
    if [ "${rc}" = "0" ]; then
        write_result "$([ "$(result_mode)" = "execute" ] && printf "done" || printf pass)" 0
    elif [ "$(result_mode)" = "execute" ]; then
        write_result refused "${rc}"
    else
        write_result fail "${rc}"
    fi
    exit "${rc}"
}
trap result_exit EXIT
trap 'RESULT_SIGNAL=INT; exit 130' INT
trap 'RESULT_SIGNAL=TERM; exit 143' TERM
trap 'RESULT_SIGNAL=HUP; exit 129' HUP
while (($#)); do
    case "$1" in
        --source) SOURCE_DIR="$2"; shift 2 ;;
        --dist) DIST_BASE="$2"; shift 2 ;;
        --state) STATE_FILE="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --rehearse) REHEARSE=1; shift ;;
        --allow-skipped) ALLOW_SKIPPED=1; shift ;;
        --shadow) SHADOW=1; shift ;;
        --execute) EXECUTE_ID="$2"; shift 2 ;;
        --confirm) CONFIRM="$2"; shift 2 ;;
        --result) RESULT_FILE="$2"; shift 2 ;;
        -*) die "unknown flag: $1" ;;
        *) SHA="$(short_sha "$1")"; shift ;;
    esac
done
XDG_RUNTIME_DIR="/run/user/$(id -u)"
export XDG_RUNTIME_DIR
result_soft_log_start

[ "${DRY_RUN}${REHEARSE}" != "11" ] || die "--dry-run and --rehearse are separate modes: pick one"
# Целевой релиз НИКОГДА не подменяется развёрнутым: dry-run и репетиция проверяют
# именно цель, а развёрнутый релиз — это база.
if [ "${DRY_RUN}" = "1" ] || [ "${REHEARSE}" = "1" ]; then
    [ -z "${EXECUTE_ID}" ] || SHA="$(short_sha "${EXECUTE_ID}")"
    [ -n "${SHA}" ] || die "dry-run needs the TARGET sha: cutover.sh SHA --dry-run|--rehearse (the deployed release is only the baseline, never a substitute)"
    [ -z "${CONFIRM}" ] || die "--confirm belongs to the real run (--execute), not to --dry-run/--rehearse"
else
    # Боевой прогон — только явным --execute и только с токеном свежего dry-run или
    # репетиции. Позиционный SHA в боевом режиме не принимается: опечатка в
    # аргументе не должна останавливать прод.
    [ -n "${EXECUTE_ID}" ] || die "real cutover requires --execute <release-id>; run --dry-run or --rehearse first and pass its CONFIRM token with --confirm"
    [ -z "${SHA}" ] || [ "$(short_sha "${SHA}")" = "$(short_sha "${EXECUTE_ID}")" ] \
        || die "--execute ${EXECUTE_ID} contradicts positional sha ${SHA}"
    SHA="$(short_sha "${EXECUTE_ID}")"
    require_release_id "${SHA}"
    [ -n "${CONFIRM}" ] || die "real cutover requires --confirm <token> printed by --dry-run or --rehearse of the same plan within $((CONFIRM_TTL / 3600)) h"
    [ "${SHADOW}" = "0" ] || die "--shadow is a dry-run/rehearsal flag and cannot be used for a real cutover"
fi
require_release_id "${SHA}"
if [ "${SHADOW}" = "1" ]; then
    ALLOW_SAME_TARGET=1
    log "shadow mode: the target ${SHA} may already be deployed; equality is allowed and noted"
fi
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SOURCE_DIR="${SOURCE_DIR:-${REPO_ROOT}}"
DIST="${DIST_BASE:-${REPO_ROOT}/dist}/${SHA}"
if [ "${REHEARSE}" = "1" ]; then
    STATE_FILE="${STATE_FILE:-/tmp/canterlot-rehearse-${SHA}.state}"
else
    STATE_FILE="${STATE_FILE:-/tmp/canterlot-deploy-${SHA}.state}"
fi

# План и его хэш считаются ДО любого действия, одинаково во всех режимах.
BASE_HUB="$(deployed_sha hub)"
BASE_WEB="$(deployed_sha web)"
BASE_WORKER="$(deployed_sha worker)"
PLAN_HASH="$(plan_hash "${SHA}" "${DIST}" "${BASE_HUB}" "${BASE_WEB}" "${BASE_WORKER}")"
if [ "${DRY_RUN}" = "0" ] && [ "${REHEARSE}" = "0" ]; then
    if ! TOKEN_PROBLEM="$(confirm_token_check "${CONFIRM}" "${SHA}" "${PLAN_HASH}")"; then
        die "refusing the real cutover: ${TOKEN_PROBLEM}"
    fi
    log "confirm token accepted: release ${SHA}, plan ${PLAN_HASH}, issued $(date -u -d "@${CONFIRM%%-*}" +%H:%M:%S) UTC"
fi
if [ "${REHEARSE}" = "1" ]; then
    export DEPLOY_MODE=rehearse
    REHEARSE_COUNT_FILE="$(mktemp)"
    export REHEARSE_COUNT_FILE
    log "REHEARSAL: target ${SHA}; baseline (deployed): hub=${BASE_HUB} web=${BASE_WEB} worker=${BASE_WORKER}; plan ${PLAN_HASH}; every write is printed as WOULD-WRITE and NOT executed"
fi
if [ "${DRY_RUN}" = "1" ]; then
    DRY_BASE_HUB="${BASE_HUB}"
    DRY_BASE_WEB="${BASE_WEB}"
    DRY_BASE_WORKER="${BASE_WORKER}"
    log "dry-run mode: TARGET ${SHA}; baseline (deployed): hub=${DRY_BASE_HUB} web=${DRY_BASE_WEB} worker=${DRY_BASE_WORKER}; printing the plan, no writes"
fi

# Чек-лист владельца: проверки со входом остаются за ним (решение владельца),
# кроме check-proxy: его положительная проверка входит сама по одноразовой ссылке.
# Печатается в конце каждого режима.
owner_checklist() {
    cat <<EOF
=== owner checklist: authenticated checks (the toolkit does not do these) ===
  [ ] scripts/check-proxy.sh ${SITE_ORIGIN} --positive (operator, with EXPECTED_LOGIN set): hub 401 without
      a session, forged Remote-User/X-Canterlot-* stripped, sign-in by a one-off recovery link into a temp jar,
      a real client IP in the sessions list, logout; the owner gets one sign-in push from it
  [ ] owner signs in to the PWA with the password or a passkey (own sign-in, no Caddy prompt);
      the update banner shows release ${SHA}
  [ ] GET ${SITE_ORIGIN}/api/v1/config in the signed-in PWA reports the new version
  [ ] open a live session: timeline renders, send a message, a reply arrives
  [ ] the release's own new features (CHANGES.rst for this version)
  [ ] paste the result of each line into the deploy log section ("Left for the owner")
EOF
}
# Токен печатается только при PASS: провалившийся прогон не даёт права на боевой.
print_token() {
    local now token until
    now="$(date -u +%s)"
    token="$(confirm_token "${SHA}" "${PLAN_HASH}" "${now}")"
    until="$(date -u -d "@$((now + CONFIRM_TTL))" '+%Y-%m-%d %H:%M UTC')"
    RESULT_TOKEN="${token}"
    RESULT_TOKEN_UNTIL="$(date -u -d "@$((now + CONFIRM_TTL))" +%Y-%m-%dT%H:%M:%SZ)"
    printf '  CONFIRM=%s   # plan %s, valid until %s\n' "${token}" "${PLAN_HASH}" "${until}"
    printf '  real run:  scripts/deploy/cutover.sh --execute %s --confirm %s --source %s --dist %s\n' \
        "${SHA}" "${token}" "${SOURCE_DIR}" "${DIST_BASE:-${REPO_ROOT}/dist}"
}

# Провал осмотра БД — громкая смерть, а не пустой результат.
hub_psql() { ssh_hub "runuser -u canterlot -- psql -h '${HUB_PG_SOCKET}' -d '${HUB_PG_DB}' -tAc \"$1\"" || die "hub DB query failed"; }
# Вариант без die: для отчёта, где провал запроса — тоже данные.
hub_psql_soft() { ssh_hub "runuser -u canterlot -- psql -h '${HUB_PG_SOCKET}' -d '${HUB_PG_DB}' -tAc \"$1\"" 2>/dev/null || echo UNKNOWN; }

# --- сугубо читаемые проверки, общие для dry-run и боевого прогона ---
read_gates() {
    log "read gates for target ${SHA}"
    [ "$(systemctl --user show "${WORKER_UNIT}" -p KillMode --value 2>/dev/null)" = "process" ] \
        || { soft "worker unit KillMode is not process"; return 1; }
    log "gate: worker KillMode=process"
    LIVE_N="$(hub_psql "select count(*) from executions where state='running'")"
    log "gate: ${LIVE_N} live executions"
    if [ "${LIVE_N}" != "0" ]; then
        CONV_LIST="$(hub_psql "select distinct conversation_id::text from executions where state='running'" | sed "s/^/'/" | sed "s/$/'/" | paste -sd, -)"
        OPEN_SI="$(hub_psql "select count(*) from commands where conversation_id in (${CONV_LIST}) and kind in ('interrupt','stop_execution') and delivery_state != 'completed'")"
        [ "${OPEN_SI}" = "0" ] || { soft "open stop/interrupt: ${OPEN_SI}"; return 1; }
        log "gate: no open stop/interrupt"
    fi
    COMPACTIONS_TABLE=""
    if ! COMPACTIONS_TABLE="$(hub_psql "select count(*) from information_schema.tables where table_name='compactions'")"; then
        soft "cannot query compactions table"
        return 1
    fi
    if [ "${COMPACTIONS_TABLE}" = "1" ]; then
        OPEN_COMP="$(hub_psql "select count(*) from compactions where finished_at is null")"
        [ "${OPEN_COMP}" = "0" ] || { soft "open compactions: ${OPEN_COMP}"; return 1; }
        log "gate: no open compaction"
    fi
    # Присваивание отдельно: провал deployed_sha внутри [ "$(...)" ] читался бы
    # как «не совпало» и ворота молча проходили. В dry-run то же ворота: боевой
    # прогон на уже развёрнутую цель отказал бы, и dry-run обязан это показать.
    GATE_HUB_NOW="$(deployed_sha hub)" || { soft "cannot read the deployed hub release"; return 1; }
    GATE_WEB_NOW="$(deployed_sha web)" || { soft "cannot read the deployed web release"; return 1; }
    GATE_WORKER_NOW="$(deployed_sha worker)" || { soft "cannot read the deployed worker release"; return 1; }
    if [ "${ALLOW_SAME_TARGET}" = "1" ]; then
        log "shadow dry-run: deployed hub=${GATE_HUB_NOW} web=${GATE_WEB_NOW} worker=${GATE_WORKER_NOW} (equality with the target allowed)"
    else
        [ "${GATE_HUB_NOW}" != "${SHA}" ] || { soft "hub already on ${SHA}"; return 1; }
        [ "${GATE_WEB_NOW}" != "${SHA}" ] || { soft "web already on ${SHA}"; return 1; }
        [ "${GATE_WORKER_NOW}" != "${SHA}" ] || { soft "worker already on ${SHA}"; return 1; }
    fi
    if assert_no_remote_proc "${PROXMOX_SSH}" "vzdump" "vzdump"; then
        log "gate: no vzdump running"
    else
        if [ "$?" -eq 2 ]; then soft "cannot inspect for vzdump (ssh failed)"; else soft "vzdump running"; fi
        return 1
    fi
    # Провал осмотра — жёсткое «нельзя продолжать», а не молчаливое «всё чисто».
    # pvesh запускается отдельно: иначе упавший pvesh дал бы пустой grep и был бы
    # прочитан как «блокировки нет».
    CT_STATUS=""
    if ! CT_NODE_OUT="$(check_ct_node 2>&1)"; then
        soft "${CT_NODE_OUT}"
        return 1
    fi
    log "gate: ${CT_NODE_OUT}"
    if ! CT_STATUS="$(ssh_pmx "pvesh get /nodes/${PROXMOX_NODE}/lxc/${HUB_CTID}/status/current --output-format json")"; then
        soft "cannot query CT lock state (pvesh/ssh failed)"
        return 1
    fi
    CT_LOCK="$(printf '%s' "${CT_STATUS}" | grep -o 'lock[^,]*' | sed -n 1p || true)"
    if [ -n "${CT_LOCK}" ]; then soft "CT locked: ${CT_LOCK}"; return 1; fi
    log "gate: no vzdump, no CT lock"
    # readyz проверяется внутри CT: dev VM -> 8080 режет nftables по дизайну.
    if [ "$(ssh_hub "curl -fsS -m 5 'http://${HUB_IP}:8080/readyz'" 2>/dev/null)" = '{"status":"ok"}' ]; then
        log "gate: hub readyz ok"
    else
        soft "hub readyz not ok from the CT"; return 1
    fi
    UTC_HOUR="$(date -u +%H:%M)"
    if [[ "${UTC_HOUR}" > "02:45" && "${UTC_HOUR}" < "03:45" ]]; then
        soft "inside the 03:30 backup window"
        return 1
    fi
    log "gate: outside the 03:30 backup window"
    return 0
}

pwa_read_gates() {
    log "pwa read gates"
    ssh_web "cd '${WEB_COMPOSE_DIR}' && docker compose config --quiet" || { soft "compose config invalid"; return 1; }
    [ "$(ssh_web "docker exec caddy printenv GODEBUG")" = "tlsmlkem=0" ] || { soft "GODEBUG not tlsmlkem=0"; return 1; }
    WEB_SHA_LIVE="$(deployed_sha web)" || { soft "cannot read the deployed web release"; return 1; }
    if ! sha_match "$(container_pwa_sha)" "${WEB_SHA_LIVE}"; then
        soft "container version.json ($(container_pwa_sha)) is not release ${WEB_SHA_LIVE}"; return 1
    fi
    log "gate: container version.json matches deployed web release"
    # Проверка в запущенном контейнере, без создания: тот же Caddyfile, те же маунты.
    ssh_web "docker exec caddy caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile" \
        || { soft "caddy validate failed"; return 1; }
    log "gate: caddy validate ok"
    return 0
}
# --- конец читаемых проверок ---

# Превью слияния старых чанков: сколько файлов из каждого источника недостаёт
# цели (чтение). Используется dry-run и репетицией.
merge_preview() {
    local target="$1" sources counts line
    if ! sources="$(web_merge_sources "${target}")"; then
        soft "cannot list web releases for the chunk merge preview"
        return
    fi
    if [ -z "${sources}" ]; then
        log "chunk merge preview: no older web release to merge into ${target}"
        return
    fi
    # shellcheck disable=SC2086 # sources — список имён релизов через пробел
    if ! counts="$(web_merge_counts "${target}" ${sources})"; then
        soft "cannot count old chunks for the merge preview"
        return
    fi
    while IFS= read -r line; do
        [ -n "${line}" ] || continue
        # shellcheck disable=SC2086 # строка «релиз файлов пропущено»
        set -- ${line}
        log "chunk merge preview: from $1: $2 immutable files, $3 missing in ${target} (would be copied, no clobber)"
    done <<<"${counts}"
}

if [ "${DRY_RUN}" = "1" ]; then
    # Доказательно только чтение, но читаемых проверок должно быть столько же,
    # сколько в боевом прогоне: preflight (конфиги, ownership, журналы) и verify
    # (два чтения, тики, CSP, сайты) выполняются целиком. Ничего не пишется:
    # state-файл не передаём, spec_compat только печатается.
    PHASE="dry-run"
    export DEPLOY_SOFT_KIND=dry-run
    SOFT_BEFORE="${SOFT_FAILURES}"
    HARD_BEFORE="${HARD_FAILURES}"
    NOT_RUN=0
    if ! "${SCRIPT_DIR}/preflight.sh" "${SHA}" --dry-run --source "${SOURCE_DIR}" \
            --dist "${DIST_BASE:-${REPO_ROOT}/dist}"; then
        hard "preflight --dry-run failed"
        NOT_RUN=1
    fi
    # Цель: dist, залитые каталоги на каждом хосте, версии, миграции, CSP.
    TARGET_EXTRA=()
    [ "${ALLOW_SKIPPED}" = "0" ] || TARGET_EXTRA=(--allow-skipped)
    if ! "${SCRIPT_DIR}/target-check.sh" "${SHA}" --source "${SOURCE_DIR}" \
            --dist "${DIST_BASE:-${REPO_ROOT}/dist}" "${TARGET_EXTRA[@]}"; then
        hard "target-check failed for ${SHA}"
        NOT_RUN=1
    fi
    read_gates || log "dry-run: a read gate above reported a problem (see SOFT-FAIL)"
    pwa_read_gates || log "dry-run: a pwa gate above reported a problem (see SOFT-FAIL)"
    log "dry-run: baseline checks below run against the DEPLOYED release (hub=${DRY_BASE_HUB}); the target was checked above"
    log "dry-run: session PID/tick snapshot (read-only):"
    DRY_LIVE_IDS=""
    if ! DRY_LIVE_IDS="$(hub_psql "select id::text from executions where state='running' order by 1")"; then
        soft "cannot list live executions for the dry-run snapshot"
    else
        for exec_id in ${DRY_LIVE_IDS:-}; do
            log "exec ${exec_id}: $(proc_ticks "${exec_id}" | grep supervise | sed -n 1p)"
        done
    fi
    maintenance_check "${DRY_LIVE_IDS}" "${DRY_BASE_WEB}"
    merge_preview "${SHA}"
    GEOIP_RC=0
    # Путь спрашивается у выкатываемого релиза: у развёрнутого команды geoip path может не быть.
    GEOIP_WHY="$(geoip_due "${HUB_RELEASES}/${SHA}/bin/canterlot-hub")" || GEOIP_RC=$?
    case "${GEOIP_RC}" in
        0) log "dry-run: geoip: ${GEOIP_WHY}; the real run would run canterlot-hub geoip update (soft)" ;;
        1) log "dry-run: geoip: ${GEOIP_WHY}; no update needed" ;;
        *) log "dry-run: geoip: ${GEOIP_WHY}; the real run would record a soft failure" ;;
    esac
    # Диктовка: только чтение и план; её состояние dry-run не проваливает.
    speech_plan "${SHA}"
    log "dry-run: speech: ${SPEECH_UNIT}: ${SPEECH_STATE}"
    case "${SPEECH_ACTION}" in
        restart) log "dry-run: speech: ${SPEECH_REASON}; the real run restarts ${SPEECH_UNIT} after the worker is live (bounded to ${SPEECH_RESTART_TIMEOUT} s) and asks health (soft)" ;;
        disable) log "dry-run: speech: ${SPEECH_REASON}; the real run stops and disables ${SPEECH_UNIT} (soft)" ;;
        skip) log "dry-run: speech: ${SPEECH_REASON}; the real run leaves ${SPEECH_UNIT} alone" ;;
        *) log "dry-run: speech: ${SPEECH_REASON}; the real run would record a speech soft failure (never blocks the release)" ;;
    esac
    VERIFY_EXTRA=()
    [ "${ALLOW_SKIPPED}" = "0" ] || VERIFY_EXTRA=(--allow-skipped)
    if ! "${SCRIPT_DIR}/verify.sh" --dry-run --dist "${DIST_BASE:-${REPO_ROOT}/dist}" "${VERIFY_EXTRA[@]}"; then
        hard "verify --dry-run failed"
        NOT_RUN=1
    fi
    cat <<EOF
dry-run plan for ${SHA} (write steps are NOT RUN by design):
  baseline (deployed): hub=${DRY_BASE_HUB} web=${DRY_BASE_WEB} worker=${DRY_BASE_WORKER}
  target checked read-only: dist (SHA256SUMS, source=, versions, glibc, openapi, EXPECT), staged dirs on every host, migrations, CSP
  NOT RUN (write steps, never executed by a dry-run; --rehearse prints each one as WOULD-WRITE):
   - preflight snapshot/state file
   - fresh vzdump (notes pre-${SHA}-<ts>) while hub and worker run; then read gates and the session inventory again
   - worker stop (systemctl --user stop ${WORKER_UNIT}); session processes keep running
   - quiesced pg_dump canterlot-pre-${SHA}-<ts>.dump, verified
   - hub flip: cd ${HUB_BASE} && ln -s releases/${SHA} + mv -T; restart ${HUB_UNIT} once
   - worker flip + start, state=live wait (worker down time ends here)
   - PWA after the worker is live (soft): canterlot.env.${SHA}, flip build, 'docker compose up -d ${WEB_CONTAINER}'
   - old _app/immutable chunk merge; verify.sh twice (the read-only verify gates above did run)
   - precompressed Content-Encoding probe (would start a one-off loopback Caddy inside the container)
   - geoip: canterlot-hub geoip update on the hub when [auth.geoip].path (canterlot-hub geoip path) is missing or older than ${GEOIP_MAX_AGE_DAYS} days (soft)
   - speech: restart ${SPEECH_UNIT} (bounded to ${SPEECH_RESTART_TIMEOUT} s) + health, or stop and disable it when ${SHA} has no canterlot-speech (soft, never blocks)
   - retention (retain.sh --apply --keep ${SHA} ...)
  WOULD RUN (only if the gates pass and the token matches this plan):
EOF
    plan_text "${SHA}" "${DIST}" "${BASE_HUB}" "${BASE_WEB}" "${BASE_WORKER}" | sed 's/^/   | /'
    echo "  rollback points would be: this run's vzdump + pg_dump (manual restore only, see report on failure)"
    if [ "${NOT_RUN}" != "0" ] && [ "${ALLOW_SKIPPED}" = "0" ]; then
        log "dry-run FAIL: a read-only preflight/verify gate did not run (use --allow-skipped only to acknowledge a known gap)"
    fi
    if [ "${NOT_RUN}" != "0" ] && [ "${ALLOW_SKIPPED}" = "1" ]; then
        log "dry-run: read-only gate gaps explicitly allowed (--allow-skipped)"
    fi
    if [ "${SOFT_FAILURES}" != "${SOFT_BEFORE}" ] || [ "${HARD_FAILURES}" != "${HARD_BEFORE}" ] || [ "${NOT_RUN}" != "0" ]; then
        log "dry-run FAIL: ${HARD_FAILURES} hard, ${SOFT_FAILURES} soft failures, not-run=${NOT_RUN}; no confirm token"
        write_result fail 1
        exit 1
    fi
    owner_checklist
    print_token
    log "dry-run PASS: every read-only preflight/verify gate ran, nothing written"
    write_result pass 0
    exit 0
fi


# --- боевой прогон и репетиция ниже: один и тот же код. ---
# В репетиции каждая запись проходит через run_write и печатается; чтения после
# неё видят живое (неизменённое) состояние, поэтому ожидаемые значения после
# записи — «как сейчас»: *_EXPECT ниже.
# Общая предпроверка репетиции и боевого прогона: preflight и target-check цели
# (dist, залитые каталоги на каждом хосте, бинари, openapi, EXPECT, база не впереди).
# Репетиция поэтому строгое надмножество dry-run; пропуски (NOT RUN) не допускаются.
PHASE="preflight"
export DEPLOY_SOFT_KIND=preflight
log "common preflight: preflight.sh and target-check.sh for ${SHA}"
"${SCRIPT_DIR}/preflight.sh" "${SHA}" --source "${SOURCE_DIR}" --dist "${DIST_BASE:-${REPO_ROOT}/dist}" --state "${STATE_FILE}" \
    || die "preflight failed for ${SHA}: refusing before any write"
"${SCRIPT_DIR}/target-check.sh" "${SHA}" --source "${SOURCE_DIR}" --dist "${DIST_BASE:-${REPO_ROOT}/dist}" \
    || die "target check failed for ${SHA}: refusing before any write"

# CSP, который уйдёт в canterlot.env.<sha>, обязан быть выведен из финального PWA-билда
# этого же dist: проверка до любого изменения (урок 1.5.0).
if CSP_BUILD_OUT="$(check_csp_matches_build)"; then
    log "CSP vs final build: ${CSP_BUILD_OUT}"
else
    die "CSP is not derived from the final PWA build, refusing before any flip: ${CSP_BUILD_OUT}"
fi

# Состояние ДО первого изменения: предыдущие релизы каждого компонента.
PREV_HUB_SHA="$(deployed_sha hub)"
PREV_WEB_SHA="$(deployed_sha web)"
PREV_WORKER_SHA="$(deployed_sha worker)"
if is_rehearse; then
    HUB_EXPECT="${PREV_HUB_SHA}"
    WEB_EXPECT="${PREV_WEB_SHA}"
    WORKER_EXPECT="${PREV_WORKER_SHA}"
else
    HUB_EXPECT="${SHA}"
    WEB_EXPECT="${SHA}"
    WORKER_EXPECT="${SHA}"
fi
# Резерв целевого и предыдущего релизов на время операции: retain их не удалит
# между проверкой и flip (и после flip, пока оператор может вернуться назад).
RESERVED=0
release_reservations() {
    [ "${RESERVED}" = "1" ] || return 0
    RESERVED=0
    run_write "release hub reservations" unreserve_releases hub "${SHA}" "${PREV_HUB_SHA}" || log "could not release hub reservations: .reserved.* remain in ${HUB_BASE}"
    run_write "release web reservations" unreserve_releases web "${SHA}" "${PREV_WEB_SHA}" || log "could not release web reservations: .reserved.* remain in ${WEB_SITES_BASE}"
    run_write "release worker reservations" unreserve_releases worker "${SHA}" "${PREV_WORKER_SHA}" || log "could not release worker reservations: .reserved.* remain in ${WORKER_BASE}"
}
RESERVED=1
if ! { run_write "reserve hub releases" reserve_releases hub "${SHA}" "${PREV_HUB_SHA}" \
        && run_write "reserve web releases" reserve_releases web "${SHA}" "${PREV_WEB_SHA}" \
        && run_write "reserve worker releases" reserve_releases worker "${SHA}" "${PREV_WORKER_SHA}"; }; then
    die "cannot reserve releases for the run"
fi

# Stagedirs обязаны существовать до первого изменения (урок: flip в несуществующий релиз).
[ -f "${DIST}/RELEASE" ] || die "no RELEASE in dist: ${DIST}"
ssh_hub "test -d '${HUB_RELEASES}/${SHA}/bin'" || die "hub not staged: ${HUB_RELEASES}/${SHA}"
ssh_web "test -d '${WEB_RELEASES}/${SHA}/_app'" || die "web not staged: ${WEB_RELEASES}/${SHA}"
[ -x "${WORKER_RELEASES}/${SHA}/bin/canterlot-worker" ] || die "worker not staged"
# Предпосылки flip проверяются ДО остановки воркера: иначе leftover current.new
# или неполный релиз оставляли бы хаб нетронутым, а воркер — остановленным.
ssh_hub "test -x '${HUB_RELEASES}/${SHA}/bin/canterlot-hub' \
    && test -f '${HUB_RELEASES}/${SHA}/RELEASE' \
    && test -f '${HUB_RELEASES}/${SHA}/SHA256SUMS'" \
    || die "hub target release ${SHA} is incomplete"
if ssh_hub "test -e '${HUB_BASE}/current.new'"; then
    die "leftover ${HUB_BASE}/current.new: clean it up before cutover"
fi
EXPECTED_MIGR="$(grep '^migrations_last=' "${DIST}/RELEASE" | cut -d= -f2 | grep -oE '^[0-9]+' | sed 's/^0*//' || true)"
[ -n "${EXPECTED_MIGR}" ] || die "bad migrations_last in ${DIST}/RELEASE"
read_gates || die "read gates failed before any write"

# Миграции ДО первого изменения.
PREV_MIGR_MAX="$(hub_psql "select coalesce(max(version),0) from _sqlx_migrations where success")"
PREV_MIGR_FAILED="$(hub_psql "select count(*) from _sqlx_migrations where not success")"
log "previous releases: hub=${PREV_HUB_SHA} web=${PREV_WEB_SHA} worker=${PREV_WORKER_SHA}; migrations max=${PREV_MIGR_MAX} failed=${PREV_MIGR_FAILED}"
if is_rehearse; then MIGR_EXPECT="${PREV_MIGR_MAX}"; else MIGR_EXPECT="${EXPECTED_MIGR}"; fi

# Единственная ловушка на весь прогон: ставится один раз, снимается только после
# прошедшей проверки. Внутри обработчика сигналы глушатся, повторный вход невозможен.
# Решение принимается по записанным фактам, а не по имени фазы:
# - flip хаба не начинался: восстановление до flip (рестарт воркера на прежнем
#   релизе, если его останавливали; иначе восстанавливать нечего);
# - flip начат, ссылка не прочитана назад: чтение под host lock решает;
# - хаб переключён, новый воркер не доказан живым: остановить воркер и доложить;
# - новый воркер доказан живым: доложить, ничего не гасить.
PHASE="early"
WORKER_WAS_ACTIVE=0
WORKER_STOP_ISSUED=0
HUB_FLIP_STARTED=0
WORKER_DOWNTIME=""
HUB_INTERRUPTION=""
VZ_ARCHIVE=""
DONE=0
ROLLING_BACK=0
RUN_TS="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DUMP="canterlot-pre-${SHA}-${RUN_TS}.dump"
if systemctl --user is-active --quiet "${WORKER_UNIT}"; then WORKER_WAS_ACTIVE=1; fi

facts_line() {
    printf 'vzdump done=%s, worker stopped (verified)=%s, dump verified=%s, hub flip started=%s, hub link read back=%s, new worker live=%s' \
        "$(yn "${VZDUMP_DONE}")" "$(yn "${WORKER_STOPPED}")" "$(yn "${DUMP_VERIFIED}")" \
        "$(yn "${HUB_FLIP_STARTED}")" "$(yn "${HUB_FLIPPED}")" "$(yn "${WORKER_PROVEN}")"
}

# Управляющий воркер до стопа: ровно процесс MainPID юнита и ничего больше
# (find_control_worker сам по себе не ослабляется). Юнит не активен — никого.
control_worker_exact() {
    local label="$1" main_pid found=""
    main_pid="$(unit_main_pid)" || main_pid=""
    if found="$(find_control_worker)"; then
        case "${found}" in
            *"; worker process "*)
                log "control-worker scan ${label} matched more than the unit MainPID ${main_pid:-none}: ${found}"
                return 1 ;;
        esac
        if [ -n "${main_pid}" ] && [ "${main_pid}" != "0" ] && [[ "${found}" == "worker process ${main_pid} "* ]]; then
            log "control-worker scan ${label}: exactly the unit process ${main_pid}"
            return 0
        fi
        log "control-worker scan ${label} matched something other than the unit MainPID ${main_pid:-none}: ${found}"
        return 1
    fi
    if [ -n "${main_pid}" ] && [ "${main_pid}" != "0" ]; then
        log "control-worker scan ${label} did not see the unit process ${main_pid}"
        return 1
    fi
    log "control-worker scan ${label}: no control worker runs (unit not active)"
    return 0
}

# Инвентарь сессий после vzdump: каждый записанный процесс жив с тем же тиком, и
# ни одна сессия не меняет состояние (строка created/starting/stopping/owner_lost
# при живом супервизоре после ожидания). Новый супервизор без строки — к сведению,
# как и перед стопом раньше.
inventory_recheck() {
    local label="$1" extra_id extra_state bad=0
    if ! snapshot_drift "${STATE_FILE}" "${label}"; then
        log "a recorded session process disappeared or changed its start tick ${label}"
        return 1
    fi
    log "recorded session PIDs and ticks intact ${label} (new children allowed)"
    settle_supervisor_rows "${WORKER_DATA}/executions" || { log "cannot reconcile supervisors with the hub ${label}"; return 1; }
    while read -r extra_id extra_state; do
        [ -n "${extra_id:-}" ] || continue
        if is_transient_exec_state "${extra_state}"; then
            log "session ${extra_id} is ${extra_state} ${label}: a session is changing state"
            bad=1
        else
            log "supervisor ${extra_id} (hub row: ${extra_state}) appeared after the preflight: not in the tick snapshot (allowed)"
        fi
    done <<<"${SETTLED_EXTRA}"
    return "${bad}"
}

# Блок доклада печатается и кладётся в файл рядом с результатом (путь — в результате).
report_and_stop() {
    result_path
    RESULT_STOP_BLOCK="${RESULT_FILE%.result}.stop.txt"
    report_block | tee "${RESULT_STOP_BLOCK}" || RESULT_STOP_BLOCK=""
}
report_block() {
    # Точное состояние на момент остановки + ручные команды (таблица Task 12).
    # Все запросы с запасным значением: обработчик не должен умирать сам.
    local hub_link hub_readyz migr_max migr_failed worker_state worker_link web_link live_n
    hub_link="$(ssh_hub "readlink '${HUB_CURRENT}'" 2>/dev/null || echo UNKNOWN)"
    hub_readyz="$(ssh_hub "curl -fsS -m 5 'http://${HUB_IP}:8080/readyz'" 2>/dev/null || echo FAIL)"
    migr_max="$(hub_psql_soft "select coalesce(max(version),0) from _sqlx_migrations where success")"
    migr_failed="$(hub_psql_soft "select count(*) from _sqlx_migrations where not success")"
    if systemctl --user is-active --quiet "${WORKER_UNIT}" 2>/dev/null; then worker_state="active"; else worker_state="STOPPED"; fi
    worker_link="$(readlink "${WORKER_CURRENT}" 2>/dev/null || echo UNKNOWN)"
    web_link="$(ssh_web "readlink '${WEB_BUILD_LINK}'" 2>/dev/null || echo UNKNOWN)"
    live_n="$(hub_psql_soft "select count(*) from executions where state = 'running'")"
    local speech_now speech_back_cmd
    speech_now="$(speech_state "${HUB_CURRENT}/bin/canterlot-speech" 2>/dev/null)" || speech_now="UNKNOWN"
    speech_back_cmd="$(ssh_cmd_text "${HUB_SSH}" "$(script_cmd_text \
        'systemctl cat "$3" >/dev/null 2>&1 || exit 0; if test -x "$1/releases/$2/bin/canterlot-speech"; then timeout "$4" systemctl restart "$3"; else systemctl stop "$3"; systemctl disable "$3"; fi' \
        "${HUB_BASE}" "${PREV_HUB_SHA}" "${SPEECH_UNIT}" "${SPEECH_RESTART_TIMEOUT}")")"
    local hub_back_cmd web_back_cmd worker_back_cmd hub_unres_cmd web_unres_cmd worker_unres_cmd
    hub_back_cmd="$(ssh_cmd_text "${HUB_SSH}" "$(locked_cmd_text "${HUB_BASE}/.deploy.lock" \
        'rm -f "$1/current.new" && '"${HUB_FLIP_SCRIPT}"' && systemctl restart "$4"' \
        "${HUB_BASE}" "${PREV_HUB_SHA}" "${HUB_CURRENT}" "${HUB_UNIT}")")"
    web_back_cmd="$(ssh_cmd_text "${WEB_SSH}" "$(locked_cmd_text "${WEB_SITES_BASE}/.deploy.lock" \
        'cd "$1" && test -d "releases/$2/_app" && rm -f build.new && ln -s "releases/$2" build.new && mv -T build.new build && cp "$3" "$4" && cd "$5" && docker compose up -d "$6"' \
        "${WEB_SITES_BASE}" "${PREV_WEB_SHA}" "${WEB_ENV}.${PREV_WEB_SHA}" "${WEB_ENV}" "${WEB_COMPOSE_DIR}" "${WEB_CONTAINER}")")"
    worker_back_cmd="$(locked_cmd_text "${WORKER_BASE}/.deploy.lock" \
        'cd "$1" && test -x "releases/$2/bin/canterlot-worker" && rm -f current.new && ln -s "releases/$2" current.new && mv -T current.new current' \
        "${WORKER_BASE}" "${PREV_WORKER_SHA}")"
    hub_unres_cmd="$(ssh_cmd_text "${HUB_SSH}" "$(locked_cmd_text "${HUB_BASE}/.deploy.lock" "${UNRESERVE_SCRIPT}" "${HUB_BASE}" "${SHA}" "${PREV_HUB_SHA}")")"
    web_unres_cmd="$(ssh_cmd_text "${WEB_SSH}" "$(locked_cmd_text "${WEB_SITES_BASE}/.deploy.lock" "${UNRESERVE_SCRIPT}" "${WEB_SITES_BASE}" "${SHA}" "${PREV_WEB_SHA}")")"
    worker_unres_cmd="$(locked_cmd_text "${WORKER_BASE}/.deploy.lock" "${UNRESERVE_SCRIPT}" "${WORKER_BASE}" "${SHA}" "${PREV_WORKER_SHA}")"
    cat <<EOF
STOP-AND-REPORT (phase ${PHASE}): automation halted, nothing else will be touched.
  facts: $(facts_line)
  live state:
    hub current        = ${hub_link} (was ${PREV_HUB_SHA} before this run)
    hub readyz         = ${hub_readyz}
    hub migrations     = max ${migr_max} failed ${migr_failed} (was max ${PREV_MIGR_MAX} failed ${PREV_MIGR_FAILED})
    worker             = ${worker_state}, current ${worker_link} (was ${PREV_WORKER_SHA})
    web build          = ${web_link} (was releases/${PREV_WEB_SHA})
    live executions    = ${live_n} (sessions keep running in their supervisors and buffer journals)
    speech unit        = ${SPEECH_UNIT}: ${speech_now} (dictation only, never blocks recovery)
    this run's dump   = ${HUB_BACKUP_DIR}/${RUN_DUMP} (quiesced after the worker stop, verified readable before the hub flip: $(yn "${DUMP_VERIFIED}"))
    this run's vzdump = notes pre-${SHA}-${RUN_TS}${VZ_ARCHIVE:+, archive ${VZ_ARCHIVE}} (whole CT, taken earlier, while the worker still ran)
  manual recovery (Task 12 rollback table — run by hand, in this order).
  Every flip below takes the same host lock as cutover and retention, and re-checks
  the target release inside it:
    0. before flipping the hub back to a release without own sign-in (1.29 and older), in this order:
       a. restore an auth block (basic_auth or forward_auth) in Caddy first: the old hub trusts Remote-User
          from the proxy and Caddy no longer asks for a login. Keep stripping Remote-*; run check-proxy.sh afterwards.
       b. remove [auth] (with [auth.geoip]) and session_check_interval under [http.ws] from ${HUB_CONFIG}:
          the old hub refuses unknown keys (deny_unknown_fields) and would not start.
       c. if the old hub will run on this database (no restore from the dump), cancel the pending sign-in
          pushes first: the old hub cannot read a push row without a conversation and its whole delivery
          pass fails until they go stale:
            runuser -u canterlot -- psql -h '${HUB_PG_SOCKET}' -d '${HUB_PG_DB}' -c "update owner_pushes set state='cancelled',updated_at=clock_timestamp() where state='pending' and kind in ('sign_in','sign_in_failures','sign_in_lock','session_country')"
       d. remove [dictation] from ${HUB_CONFIG} before starting a hub older than the dictation release
          (deny_unknown_fields, like [provider_status]).
    1. hub broken, no new migration (flip back + one restart):
         ${hub_back_cmd}
       after any hub flip back (rows 1 and 2): restart ${SPEECH_UNIT} if the release has bin/canterlot-speech,
       otherwise stop and disable it so it is not left failing (no-op when the unit is not installed):
         ${speech_back_cmd}
    2. hub broken after a new migration (e.g. 0088_provider_status, 0091_dictation_terms): full DB restore
       from this run's verified quiesced dump (manual only; rollback-db.sh also brings ${SPEECH_UNIT} in line with the release).
       The vzdump above is a separate, earlier whole-CT restore point: restoring it loses what the worker
       wrote between the vzdump and its stop, so it is the fallback when the dump itself is unusable:
         scripts/deploy/rollback-db.sh '${HUB_BACKUP_DIR}/${RUN_DUMP}' --to ${PREV_HUB_SHA}
         # it refuses while the worker is active, stops the hub, runs dropdb + createdb -O canterlot
         # + pg_restore --exit-on-error, starts the old hub and checks readyz loudly; on a failed
         # restore it starts nothing. Never start the worker on a restored DB by hand without review.
    3. PWA broken (the PWA step runs after the new worker is live; the worker is never stopped for it):
       flip build back, restore env, one recreate:
         ${web_back_cmd}
    4. worker broken or stopped: flip ${WORKER_CURRENT} back, then start (never below a live supervisor release);
       a worker release before the delegate MCP allowlist refuses [execution].delegate_mcp_servers in
       ${WORKER_CONFIG} (deny_unknown_fields): remove that key before starting it:
         ${worker_back_cmd}
         systemctl --user start ${WORKER_UNIT}
    5. when everything is resolved, release this run's reservations (retention keeps these releases until then):
         ${hub_unres_cmd}
         ${web_unres_cmd}
         ${worker_unres_cmd}
EOF
}

recover_before_hub_flip() {
    # Хаб и база не тронуты. Воркер не останавливали — восстанавливать нечего;
    # останавливали — поднимаем его на прежнем релизе.
    if [ "${WORKER_STOP_ISSUED}" = "0" ]; then
        log "STOP before hub flip (phase ${PHASE}): nothing was stopped or flipped (vzdump done: $(yn "${VZDUMP_DONE}")); worker ${WORKER_UNIT}, hub and DB untouched"
        release_reservations
        RESULT_RECOVERY="nothing_to_recover"
        if [ "${VZDUMP_STARTED}" = "0" ]; then write_result refused 1; else write_result failed_before_flip 1; fi
        exit 1
    fi
    log "STOP before hub flip (phase ${PHASE}): hub and DB untouched, restarting worker"
    PREV_LINK="$(readlink "${WORKER_CURRENT}" 2>/dev/null | sed 's|.*/||')"
    if [ "${PREV_LINK}" != "${PREV_WORKER_SHA}" ]; then
        log "worker current is ${PREV_LINK}, expected previous release ${PREV_WORKER_SHA}: NOT starting it"
        release_reservations
        RESULT_RECOVERY="needs_owner"
        write_result failed_before_flip 1
        exit 1
    fi
    if [ "${WORKER_WAS_ACTIVE}" = "1" ]; then
        if ! systemctl --user is-active --quiet "${WORKER_UNIT}" 2>/dev/null; then
            run_write "restart worker on its previous release" worker_locked 'systemctl --user start "$1"' "${WORKER_UNIT}" || log "worker start command failed: needs the owner loudly"
        fi
        if systemctl --user is-active --quiet "${WORKER_UNIT}" 2>/dev/null; then
            if wait_worker_exe "${START_WAIT_POLLS:-60}"; then PREV_EXE="${WAIT_EXE}"; else PREV_EXE="unknown"; fi
            case "${PREV_EXE}" in
                "${WORKER_RELEASES}/${PREV_WORKER_SHA}/"*)
                    RESULT_RECOVERY="worker_restarted"
                    log "worker restarted on its previous release ${PREV_WORKER_SHA} (hub ${PREV_HUB_SHA} untouched)" ;;
                *)
                    RESULT_RECOVERY="needs_owner"
                    log "worker is active but its exe is ${PREV_EXE}, not previous release ${PREV_WORKER_SHA}: needs the owner loudly" ;;
            esac
        else
            RESULT_RECOVERY="needs_owner"
            log "worker could NOT be restarted on ${PREV_WORKER_SHA}: needs the owner loudly"
        fi
    else
        RESULT_RECOVERY="worker_restarted"
        log "worker was not active before the run; left stopped"
    fi
    release_reservations
    write_result failed_before_flip 1
    exit 1
}

post_flip_stop() {
    # После flip хаба любая неудача/сигнал: остановить воркер (сессии живут
    # в сопровождениях) и только доложить. Ни базы, ни хаба не трогаем —
    # ручной откат базы описан в отчёте и выполняется только человеком.
    # stop идемпотентен и обязателен даже для activating/auto-restart:
    # is-active в этих состояниях ненулевой, но юнит сам поднимется позже.
    log "failure after the hub flip: stopping worker ${WORKER_UNIT} (sessions survive in supervisors)"
    run_write "stop worker after a post-flip failure" systemctl --user stop "${WORKER_UNIT}" || log "worker stop command failed: needs the owner loudly"
    STOPPED_STATE="$(systemctl --user show "${WORKER_UNIT}" -p ActiveState --value 2>/dev/null || echo unknown)"
    STOPPED_PID="$(systemctl --user show "${WORKER_UNIT}" -p MainPID --value 2>/dev/null || echo unknown)"
    if [ "${STOPPED_STATE}" = "inactive" ] && [ "${STOPPED_PID}" = "0" ]; then
        log "worker verified stopped (inactive, no MainPID)"
    else
        log "worker did NOT stop cleanly (ActiveState=${STOPPED_STATE}, MainPID=${STOPPED_PID}): needs the owner loudly"
    fi
    report_and_stop
    write_result stop_and_report 1
    exit 1
}

on_end() {
    # Первым делом глушим сигналы: окно между взведением флага и trap ''
    # позволяло второму сигналу войти в обработчик и оборвать восстановление.
    # HUP здесь обязателен: terminal/ssh hangup не должен убивать восстановление.
    trap '' INT TERM HUP EXIT
    if [ "${ROLLING_BACK}" = "1" ]; then
        # Повторный вход возвращается в уже работающий обработчик, а не выходит.
        return 0
    fi
    ROLLING_BACK=1
    set +e
    if [ "${DONE}" = "1" ]; then exit 0; fi
    if is_rehearse; then
        # Репетиция ничего не меняла: никакого восстановления, только доклад.
        log "REHEARSAL FAIL at phase ${PHASE}: nothing was written (every write above was printed only); fix the cause before a real run"
        release_reservations
        rm -f "${REHEARSE_COUNT_FILE}"  # lint: local-temp
        write_result fail 1
        exit 1
    fi
    log "facts at the stop: $(facts_line)"
    if [ "${HUB_FLIP_STARTED}" = "0" ]; then
        recover_before_hub_flip
    elif [ "${HUB_FLIPPED}" = "0" ]; then
        # Проверяем фактическое состояние current, а не флаг: mv мог успеть или
        # не успеть. Точно прежний -> безопасное допеределочное восстановление;
        # изменился или неизвестен -> STOP-and-report.
        # Под тем же host lock, что и flip: если ssh оборвался, а удалённый flip ещё держит
        # lock, чтение дождётся его конца и увидит настоящий current. Не удалось
        # прочитать (транспорт, таймаут lock) — состояние неизвестно, то есть post-flip.
        HUB_NOW_ABS="$(hub_locked 'readlink -m "$1"' "${HUB_CURRENT}" 2>/dev/null)" || HUB_NOW_ABS="UNKNOWN" # lint: read-only (lock-wait readback)
        if [ -n "${PREV_HUB_SHA}" ] && [ "${HUB_NOW_ABS}" = "${HUB_RELEASES}/${PREV_HUB_SHA}" ]; then
            log "hub flip failed but current is provably unchanged (${HUB_NOW_ABS}): pre-flip recovery"
            run_write "remove leftover hub current.new" hub_locked 'rm -f "$1"' "${HUB_BASE}/current.new" || log "could not remove leftover current.new: needs the owner loudly"
            HUB_FLIP_STARTED=0
            recover_before_hub_flip
        else
            log "hub flip failed with current ${HUB_NOW_ABS} (was releases/${PREV_HUB_SHA}): treating as post-flip"
            HUB_FLIPPED=1
            post_flip_stop
        fi
    elif [ "${WORKER_PROVEN}" = "1" ]; then
        # Воркер уже доказанно жив на новом релизе (state="live", exe из
        # релиза, NRestarts=0, инвариант сессий): гасить его нельзя — на этой
        # же машине живут оркестратор и отчёты, остановка глушит их все
        # (инцидент после 1.19.1: отчёт некому было доставить 25 минут).
        # Хаб, воркер и PWA остаются как есть, retention пропускается; сбой или
        # сигнал в шаге PWA тоже сюда: PWA идёт после доказанного воркера.
        if [ "${PHASE}" = "pwa" ]; then
            log "stopped during the PWA step after the worker was verified live on ${SHA}: hub and worker keep running; the PWA state is below (row 3 restores the previous build); retention skipped"
        else
            log "failure after the worker was verified live on ${SHA}: leaving hub, PWA and worker running (retention skipped)"
        fi
        report_and_stop
        write_result stop_and_report 1
        exit 1
    else
        post_flip_stop
    fi
}
trap on_end EXIT
trap 'RESULT_SIGNAL=INT; on_end' INT
trap 'RESULT_SIGNAL=TERM; on_end' TERM
trap 'RESULT_SIGNAL=HUP; on_end' HUP
# shellcheck disable=SC2317 # вызывается из функций выше и ниже
cutover_hard() { log "HARD-FAIL: $*"; exit 1; }

# Репетиция ожидания старта воркера против НАСТОЯЩЕГО юнита: тот же
# wait_worker_exe, но первые опросы подменены состояниями, которые роняли
# выкаты 1.18.0/1.18.1 (пустой MainPID, MainPID=0, ENOENT у /proc/<pid>/exe на
# переходе exec). Потом — реальные переходы exec собственных коротких детей.
_wait_and_print() {
    if wait_worker_exe "$1"; then
        printf '%s %s' "${WAIT_PID}" "${WAIT_EXE}"
        return 0
    fi
    return 1
}
rehearse_start_wait_scenarios() {
    local spec part dir out rc want_pid="${WAIT_PID}" want_exe="${WAIT_EXE}" calls misses child seen i
    log "rehearse: start-wait scenario A (live unit, no faults): pid ${want_pid} exe ${want_exe}"
    for spec in "pid-empty:3" "pid-zero:2" "exe-enoent:3" "pid-empty:2,exe-enoent:2"; do
        dir="$(mktemp -d)"
        IFS=, read -r -a parts <<<"${spec}"
        for part in "${parts[@]}"; do printf '%s\n' "${part#*:}" >"${dir}/${part%%:*}"; done
        rc=0
        out="$(START_WAIT_FAULT_DIR="${dir}" _wait_and_print "${START_WAIT_POLLS:-60}")" || rc=$?
        calls="$(cat "${dir}"/calls-* 2>/dev/null | wc -l)"
        rm -rf "${dir}"  # lint: local-temp
        if [ "${rc}" != "0" ] || [ "${out}" != "${want_pid} ${want_exe}" ]; then
            cutover_hard "rehearse: start-wait scenario ${spec} did not recover to the live process (rc ${rc}, got '${out}')"
        fi
        log "rehearse: start-wait scenario ${spec}: ${calls} faulty polls absorbed, then pid ${want_pid} from the live unit"
    done
    # PID так и не появляется: цикл обязан кончиться чистым кодом 1, не обрывом set -e.
    dir="$(mktemp -d)"
    printf '1000\n' >"${dir}/pid-zero"
    rc=0
    out="$(START_WAIT_FAULT_DIR="${dir}" _wait_and_print 4)" || rc=$?
    rm -rf "${dir}"  # lint: local-temp
    [ "${rc}" = "1" ] || cutover_hard "rehearse: start-wait with a PID that never appears returned rc ${rc} ('${out}'), expected a clean 1"
    log "rehearse: start-wait scenario never-appears: loop ended with a clean rc 1 after 4 polls (the caller's guard fires, no set -e abort)"
    # Настоящие переходы exec: readlink /proc/<pid>/exe у только что
    # запущенного ребёнка бывает пустым. proc_exe обязан это переживать.
    misses=0
    seen=0
    for i in $(seq 1 "${REHEARSE_EXEC_CHILDREN:-50}"); do
        bash -c 'exec sleep 0.01' &
        child=$!
        out="$(proc_exe "${child}")"
        if [ -z "${out}" ]; then misses=$((misses + 1)); else seen=$((seen + 1)); fi
        wait "${child}" 2>/dev/null || true
    done
    log "rehearse: real exec transitions: ${i} own short-lived children, exe unreadable ${misses} times, readable ${seen} times, no abort"
}

# shellcheck disable=SC2317 # вызывается из функций выше и ниже
cutover_hard() { log "HARD-FAIL: $*"; exit 1; }

log "deployed hub release before cutover: ${PREV_HUB_SHA}"
log "order: vzdump (everything runs) -> read gates + session inventory again -> worker stop -> quiesced pg_dump + verify -> hub flip + restart -> worker flip + start + live -> PWA flip + Caddy recreate -> speech, chunk merge, maintenance, verify x2, geoip, retention"

# 1. vzdump снимается, пока всё работает: снимок CT атомарен и не требует тишины
# писателей. Несущая точка восстановления — проверенный pg_dump после стопа воркера.
control_worker_exact "before vzdump" || cutover_hard "control-worker scan before vzdump: refusing to start the cutover"
log "fresh vzdump of CT ${HUB_CTID} while hub and worker run (notes pre-${SHA}-${RUN_TS})"
PHASE="backing-up"
VZDUMP_STARTED=1
VZ_OUT="$(run_write "vzdump" ssh_pmx "vzdump ${HUB_CTID} --mode snapshot --storage ${PBS_STORAGE} --notes-template 'pre-${SHA}-${RUN_TS}' 2>&1")" \
    || { printf '%s\n' "${VZ_OUT}" | tail -5 >&2; cutover_hard "vzdump failed"; }
if is_rehearse; then
    log "rehearse: vzdump not run"
else
    case "${VZ_OUT}" in
        *"Backup job finished successfully"*) ;;
        *) printf '%s\n' "${VZ_OUT}" | tail -5 >&2; cutover_hard "vzdump did not report 'Backup job finished successfully'" ;;
    esac
    # Имя архива: формулировка PBS менялась (урок 1.16.1-1.18.1), поэтому запасной
    # разбор по форме id ct/<ctid>/<время>Z.
    VZ_ARCHIVE="$(printf '%s\n' "${VZ_OUT}" | grep -oE "creating (vzdump archive|Proxmox Backup Server archive) '[^']+'" | sed "s/.*'\(.*\)'/\1/" | tail -1 || true)"
    [ -n "${VZ_ARCHIVE}" ] || VZ_ARCHIVE="$(printf '%s\n' "${VZ_OUT}" | grep -oE "ct/${HUB_CTID}/[0-9T:.-]+Z" | tail -1 || true)"
    log "vzdump done: archive ${VZ_ARCHIVE:-unparsed (see the vzdump output)}, notes pre-${SHA}-${RUN_TS}"
fi
VZDUMP_DONE=1

# 2. vzdump идёт до минуты: ворота чтения и инвентарь сессий заново, прямо перед стопом.
PHASE="regating"
log "re-checking read gates and the session inventory after vzdump"
read_gates || cutover_hard "read gates failed after vzdump"
[ "$(ssh_hub "readlink '${HUB_CURRENT}'" | sed 's|.*/||')" = "${PREV_HUB_SHA}" ] || cutover_hard "hub current moved unexpectedly"
inventory_recheck "after vzdump" || cutover_hard "the session inventory changed after vzdump: refusing before the worker stop"
control_worker_exact "before the worker stop" || cutover_hard "control-worker scan before the worker stop"

# 3. Стоп воркера: писатели базы затихают для pg_dump.
log "stopping worker ${WORKER_UNIT} (sessions survive in supervisors)"
LIVE_MAIN_PID="$(unit_main_pid)"
WORKER_STOP_EPOCH="$(date -u +%s)"
WORKER_STOP_ISO="$(date -u '+%Y-%m-%d %H:%M:%S')"
PHASE="worker-stopping"
WORKER_STOP_ISSUED=1
run_write "stop worker" systemctl --user stop "${WORKER_UNIT}"
PHASE="worker-stopped"
# Остановка подтверждается чтением (а не паузой): юнит inactive и MainPID 0.
# Каждое чтение защищено: пустой ответ в окне перехода не роняет прогон.
STOP_STATE=""
for _ in $(seq 1 "${STOP_WAIT_POLLS:-20}"); do
    STOP_STATE="$(systemctl --user show "${WORKER_UNIT}" -p ActiveState --value 2>/dev/null)" || STOP_STATE=""
    if [ "${STOP_STATE}" = "inactive" ] || [ "${STOP_STATE}" = "failed" ]; then break; fi
    if is_rehearse; then break; fi
    sleep "${START_POLL}"
done
if is_rehearse; then
    log "rehearse: worker left running (ActiveState=${STOP_STATE:-unreadable}, MainPID=${LIVE_MAIN_PID:-none}); the stop wait loop ran once against the live unit"
elif [ "${STOP_STATE}" != "inactive" ] && [ "${STOP_STATE}" != "failed" ]; then
    cutover_hard "worker unit did not stop (ActiveState=${STOP_STATE:-unreadable})"
fi
# Остановка юнита не доказывает отсутствие воркера: управляющий `canterlot-worker run`,
# запущенный вне systemd, пережил бы бэкапы и миграцию хаба. Тот же охранник, что в
# rollback-db: супервизоры, релеи и клиенты аренды пропускаются, тестовые воркеры вне
# корня установки и не на прод-конфиге только печатаются, любой прод-воркер — отказ.
# В репетиции воркер жив: охранник обязан найти ровно его (и ничего чужого).
if FOREIGN_WORKER="$(find_control_worker)"; then
    if is_rehearse; then
        case "${FOREIGN_WORKER}" in
            *"; worker process "*) cutover_hard "rehearse: the control-worker scan matched more than the unit MainPID ${LIVE_MAIN_PID:-none}: ${FOREIGN_WORKER}" ;;
            "worker process ${LIVE_MAIN_PID} "*) log "rehearse: the control-worker scan finds exactly the live unit process (${FOREIGN_WORKER}); supervise/mcp-relay skipped" ;;
            *) cutover_hard "rehearse: the control-worker scan matched something other than the unit MainPID ${LIVE_MAIN_PID:-none}: ${FOREIGN_WORKER}" ;;
        esac
    else
        cutover_hard "${FOREIGN_WORKER} after the unit stop: refusing to back up and migrate under a live worker"
    fi
else
    if is_rehearse && [ -n "${LIVE_MAIN_PID}" ] && [ "${LIVE_MAIN_PID}" != "0" ]; then
        cutover_hard "rehearse: the control-worker scan did not see the live unit process ${LIVE_MAIN_PID}"
    fi
    log "no control worker process left after the unit stop"
fi
# Инвариант сессий без воркера: каждый записанный процесс жив с тем же тиком.
# Новые транзиентные дети допустимы (урок 1.17.0) и только печатаются.
snapshot_drift "${STATE_FILE}" "after worker stop" \
    || cutover_hard "a recorded session process disappeared or changed its start tick after the worker stop"
log "recorded session PIDs and ticks unchanged after worker stop (new transient children allowed)"
WORKER_STOPPED=1


# 4. Дамп при остановленном воркере и его полная проверка до flip хаба.
PHASE="dumping"
run_write "pg_dump" ssh_hub "install -d -m 0700 -o postgres '${HUB_BACKUP_DIR}' && runuser -u postgres -- pg_dump -Fc -Z zstd:3 -f '${HUB_BACKUP_DIR}/${RUN_DUMP}' '${HUB_PG_DB}'" \
    || cutover_hard "pg_dump failed"
# Проверка дампа ДО flip хаба: TOC читается и каждый блок данных распаковывается.
# Без проверенного дампа дальше только рестарт воркера (см. on_end, фазы до flip).
# Сам восстановитель живёт в lib.sh: в этом файле не должно быть исполняемых
# команд работы с базой — только напечатанная ручная процедура.
PHASE="dump-verifying"
DUMP_TO_CHECK="${RUN_DUMP}"
if is_rehearse; then
    # Репетиция проверяет чтением самый свежий существующий дамп: тот же разбор,
    # та же полная распаковка в /dev/null.
    DUMP_LIST="$(ssh_hub "ls -1t '${HUB_BACKUP_DIR}'" 2>/dev/null || true)"
    DUMP_TO_CHECK="$(printf '%s\n' "${DUMP_LIST}" | grep -E '^canterlot-pre-.*\.dump$' | sed -n 1p || true)"
fi
if [ -z "${DUMP_TO_CHECK}" ]; then
    log "rehearse: no existing dump to exercise the verification on"
elif verify_dump "${HUB_BACKUP_DIR}/${DUMP_TO_CHECK}"; then
    DUMP_BYTES="$(ssh_hub "stat -c %s '${HUB_BACKUP_DIR}/${DUMP_TO_CHECK}'" 2>/dev/null || echo 0)"
    case "${DUMP_BYTES}" in '' | *[!0-9]*) DUMP_BYTES=0 ;; esac
    # zstd:3 даёт дамп примерно на 13% меньше gzip (133 МБ против 153 МБ на проде),
    # поэтому порог снижен со 100 до 64 МБ; запас на рост базы остаётся.
    if [ "${DUMP_BYTES}" -lt "${DUMP_MIN_BYTES:-64000000}" ]; then
        cutover_hard "dump ${DUMP_TO_CHECK} is suspiciously small: ${DUMP_BYTES} bytes (DUMP_MIN_BYTES=${DUMP_MIN_BYTES:-64000000})"
    fi
    log "dump fully readable: ${DUMP_TO_CHECK} (${DUMP_BYTES} bytes, TOC lists _sqlx_migrations)"
else
    cutover_hard "dump verification failed for ${DUMP_TO_CHECK}"
fi
PHASE="backed-up"
DUMP_VERIFIED=1
log "restore points: vzdump notes pre-${SHA}-${RUN_TS} (taken before the worker stop), pg_dump ${RUN_DUMP} (quiesced, verified)"

# 5. Хаб.
# Хаб: flip с абсолютным cd (урок: flip в /root), проверка ссылки, один рестарт.
RESTART_TS="$(ssh_hub "date -u '+%Y-%m-%d %H:%M:%S'")"
# Значение пришло с хоста и попадёт в команду journalctl: только формат даты.
[[ "${RESTART_TS}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}\ [0-9]{2}:[0-9]{2}:[0-9]{2}$ ]] || cutover_hard "unexpected hub timestamp: '${RESTART_TS}'"
PHASE="hub-flipping"
HUB_FLIP_STARTED=1
HUB_LINK_AFTER="$(run_write "hub flip" hub_locked "${HUB_FLIP_SCRIPT}" "${HUB_BASE}" "${SHA}" "${HUB_CURRENT}")"
# Отдельное чтение ссылки после flip: ровно releases/<sha> и абсолютная сверка.
HUB_LINK_NOW="$(ssh_hub "readlink '${HUB_CURRENT}'")" || cutover_hard "cannot read hub current after the flip"
[ "${HUB_LINK_NOW}" = "releases/${HUB_EXPECT}" ] || cutover_hard "hub current did not move to ${HUB_EXPECT} (now ${HUB_LINK_NOW}, flip printed ${HUB_LINK_AFTER:-nothing})"
[ "$(ssh_hub "readlink -m '${HUB_CURRENT}'")" = "${HUB_RELEASES}/${HUB_EXPECT}" ] \
    || cutover_hard "hub current resolves elsewhere"
HUB_FLIPPED=1
PHASE="hub-flipped"
PHASE="hub-restarting"
HUB_RESTART_EPOCH="$(date -u +%s)"
run_write "hub restart" hub_locked 'systemctl restart "$1"' "${HUB_UNIT}" || cutover_hard "hub restart failed"
log "hub restarted once; waiting for readyz"
READYZ_OK=0
for _ in $(seq 1 "${READYZ_POLLS:-30}"); do
    READYZ_NOW="$(ssh_hub "curl -fsS -m 2 'http://${HUB_IP}:8080/readyz'" 2>/dev/null)" || READYZ_NOW=""
    if [ "${READYZ_NOW}" = '{"status":"ok"}' ]; then
        READYZ_OK=1
        break
    fi
    sleep 1
done
[ "${READYZ_OK}" = "1" ] || cutover_hard "hub readyz failed after restart"
HUB_INTERRUPTION=$(( $(date -u +%s) - HUB_RESTART_EPOCH ))
if is_rehearse; then
    log "rehearse: readyz ok without a restart; no hub interruption"
    HUB_INTERRUPTION=0
else
    log "hub interruption: ${HUB_INTERRUPTION} s (restart -> readyz ok)"
fi
MIGR_MAX="$(hub_psql "select coalesce(max(version),0) from _sqlx_migrations where success")"
[ "${MIGR_MAX}" = "${MIGR_EXPECT}" ] || cutover_hard "migrations max ${MIGR_MAX}, expected ${MIGR_EXPECT}"
[ "$(hub_psql "select count(*) from _sqlx_migrations where not success")" = "0" ] || cutover_hard "failed migrations present"
if ! HUB_ERRLOG="$(ssh_hub "journalctl -u '${HUB_UNIT}' --since '${RESTART_TS}' --no-pager")"; then
    cutover_hard "cannot read hub journal after restart"
fi
MIGR_ERR="$(printf '%s\n' "${HUB_ERRLOG}" | grep -c migration_error || true)"
[ "${MIGR_ERR}" = "0" ] || cutover_hard "migration_error in hub journal"
case "${HUB_ERRLOG}" in
    *migrations_applied*) log "hub journal since the restart: migrations_applied present, 0 migration_error" ;;
    *) if is_rehearse; then
           log "rehearse: no hub restart, so no migrations_applied line since ${RESTART_TS} (expected)"
       else
           cutover_hard "hub journal since the restart has no migrations_applied line"
       fi ;;
esac
# PID и exe хаба: пустой или нулевой PID не превращается в /proc//exe (урок 1.19.0).
HUB_PID="$(ssh_hub "systemctl show '${HUB_UNIT}' -p MainPID --value" 2>/dev/null)" || HUB_PID=""
case "${HUB_PID}" in '' | 0 | *[!0-9]*) cutover_hard "hub has no MainPID after the restart (got '${HUB_PID}')" ;; esac
HUB_EXE="$(ssh_hub "readlink '/proc/${HUB_PID}/exe'" 2>/dev/null)" || HUB_EXE=""
[ -n "${HUB_EXE}" ] || cutover_hard "hub process ${HUB_PID} exe did not resolve"
[ "${HUB_EXE}" = "${HUB_RELEASES}/${HUB_EXPECT}/bin/canterlot-hub" ] \
    || cutover_hard "hub process not from release ${HUB_EXPECT}: ${HUB_EXE}"
HUB_NRESTARTS="$(ssh_hub "systemctl show '${HUB_UNIT}' -p NRestarts --value" 2>/dev/null)" || HUB_NRESTARTS=""
[ "${HUB_NRESTARTS}" = "0" ] || cutover_hard "hub NRestarts is '${HUB_NRESTARTS}', expected 0 after one manual restart"
PHASE="hub-verified"
log "hub on ${HUB_EXPECT}: readyz ok, migrations ${MIGR_MAX}, process ${HUB_PID} from the release, NRestarts=0"

# 6. Воркер.
# Воркер: ворота перед стартом жёсткие — новый воркер стартует только
# против живого, полностью смигрированного хаба (пара совместима по построению).
worker_start_gates() {
    [ "$(ssh_hub "curl -fsS -m 5 'http://${HUB_IP}:8080/readyz'" 2>/dev/null)" = '{"status":"ok"}' ] \
        || cutover_hard "hub readyz not ok before worker start"
    [ "$(hub_psql "select coalesce(max(version),0) from _sqlx_migrations where success")" = "${MIGR_EXPECT}" ] \
        || cutover_hard "hub migrations not at ${MIGR_EXPECT} before worker start"
    [ "$(hub_psql "select count(*) from _sqlx_migrations where not success")" = "0" ] \
        || cutover_hard "failed hub migrations before worker start"
}
worker_start_gates
PHASE="worker-flipping"
run_write "worker flip" worker_locked 'cd "$1" && ln -s "releases/$2" current.new && mv -T current.new current' \
    "${WORKER_BASE}" "${SHA}" || cutover_hard "worker current did not move"
link_points_to "${WORKER_CURRENT}" "${WORKER_BASE}" "releases/${WORKER_EXPECT}" \
    || cutover_hard "worker current did not move"
PHASE="worker-starting"
run_write "worker start" worker_locked 'systemctl --user start "$1"' "${WORKER_UNIT}" || cutover_hard "worker start failed"
# Процесс воркера: защищённое ожидание MainPID и читаемого exe (урок 1.18.1:
# пустой MainPID, MainPID=0 и ENOENT на переходе exec роняли set -e молча).
if ! wait_worker_exe "${START_WAIT_POLLS:-60}"; then
    cutover_hard "worker did not get a MainPID with a readable exe after start (last MainPID '${WAIT_PID:-none}')"
fi
case "${WAIT_EXE}" in
    "${WORKER_RELEASES}/${WORKER_EXPECT}/"*) log "worker process ${WAIT_PID} from release ${WORKER_EXPECT}" ;;
    *) cutover_hard "worker process not from ${WORKER_EXPECT}: ${WAIT_EXE}" ;;
esac
if is_rehearse; then
    # То же ожидание против настоящего юнита, с подмешанными состояниями старта.
    rehearse_start_wait_scenarios
fi
# Ждём именно state="live" из журнала юнита, как это делает дежурный лог.
# Голое state="live" (метка времени — префикс journald), без time="…" после него.
# В репетиции окно — с момента, когда живой юнит последний раз стал активным.
if is_rehearse; then
    LIVE_SINCE_TS="$(systemctl --user show "${WORKER_UNIT}" -p ActiveEnterTimestamp --value 2>/dev/null)" || LIVE_SINCE_TS=""
    LIVE_SINCE_EPOCH="$(date -u -d "${LIVE_SINCE_TS}" +%s 2>/dev/null)" || LIVE_SINCE_EPOCH="${WORKER_STOP_EPOCH}"
else
    LIVE_SINCE_EPOCH="${WORKER_STOP_EPOCH}"
fi
if ! LAST_STATE="$(live_from_journal "@$((LIVE_SINCE_EPOCH - 1))" "${WORKER_LIVE_WAIT:-90}")"; then
    cutover_hard "worker did not reach state=\"live\" after start (last state: ${LAST_STATE:-none})"
fi
WORKER_LIVE_EPOCH="$(date -u +%s)"
WORKER_DOWNTIME=$((WORKER_LIVE_EPOCH - WORKER_STOP_EPOCH))
if is_rehearse; then
    log "rehearse: worker journal parse found state=\"live\" of the running unit (since $(date -u -d "@${LIVE_SINCE_EPOCH}" '+%H:%M:%S') UTC)"
    log "rehearse: no down time (nothing stopped or restarted)"
else
    log "worker state=\"live\" after start; measured down time: ${WORKER_DOWNTIME} s (stop ${WORKER_STOP_ISO} UTC)"
    if [ "${WORKER_DOWNTIME}" -gt 180 ]; then
        soft "worker down time ${WORKER_DOWNTIME} s exceeds the 3-minute outage grace; machine_outages will gain a row"
    fi
    log "downtime: worker ${WORKER_DOWNTIME} s (stop -> state=live), hub ${HUB_INTERRUPTION} s (restart -> readyz ok)"
fi
systemctl --user is-active --quiet "${WORKER_UNIT}" || cutover_hard "worker not active after start"
WORKER_NRESTARTS="$(systemctl --user show "${WORKER_UNIT}" -p NRestarts --value 2>/dev/null)" || WORKER_NRESTARTS=""
[ "${WORKER_NRESTARTS}" = "0" ] || cutover_hard "worker NRestarts is '${WORKER_NRESTARTS}', expected 0"
PHASE="worker-started"
# Записанные процессы сессий живы с теми же тиками; новые дети допустимы.
snapshot_drift "${STATE_FILE}" "after worker start" \
    || cutover_hard "a recorded session process disappeared or changed its start tick after the worker start"
WORKER_PROVEN=1
log "recorded session PIDs and ticks unchanged after worker start (new transient children allowed)"

# 7. PWA — после доказанно живого воркера: он не ждёт flip билда и recreate Caddy.
# Всё мягкое (урок: PWA никогда не откатывает хаб и не трогает воркер). Ловушка
# остаётся взведённой: мягкие ошибки вызывают только soft(), а не выход.
PHASE="pwa"
log "PWA step (all failures soft)"
PWA_FAIL=""
WEB_SHA_EFF="${WEB_EXPECT}"
DIST_CSP=""
CSP_EXPECT=""
pwa_read_gates || PWA_FAIL="pwa pre-gates imperfect"
if [ -z "${PWA_FAIL}" ]; then
    # Читаем csp.txt с явным статусом: printf с упавшим cat иначе записал бы
    # CANTERLOT_CSP="" и объявил это успехом.
    if ! DIST_CSP="$(cat "${DIST}/csp.txt" 2>/dev/null)" || [ -z "${DIST_CSP}" ]; then
        PWA_FAIL="dist csp.txt is missing or empty: ${DIST}/csp.txt"
    fi
fi
if [ -z "${PWA_FAIL}" ]; then
    printf 'CANTERLOT_CSP="%s"\n' "${DIST_CSP}" | run_write_pipe "write canterlot.env.${SHA}" \
        recv_to "${WEB_SSH}" "cat > '${WEB_ENV}.${SHA}' && chmod 0600 '${WEB_ENV}.${SHA}'" \
        || PWA_FAIL="canterlot.env.${SHA} write failed"
fi
if [ -z "${PWA_FAIL}" ]; then
    # Чтение назад. В репетиции файла цели нет: читается живой env, его значение —
    # ожидаемый CSP контейнера.
    if is_rehearse; then ENV_READ_PATH="${WEB_ENV}"; else ENV_READ_PATH="${WEB_ENV}.${SHA}"; fi
    if ! ENV_WRITTEN="$(ssh_web "cat '${ENV_READ_PATH}'")"; then
        PWA_FAIL="cannot read back ${ENV_READ_PATH##*/}"
    else
        ENV_VALUE="$(printf '%s' "${ENV_WRITTEN}" | sed 's/^CANTERLOT_CSP="//; s/"$//')"
        if is_rehearse; then
            CSP_EXPECT="${ENV_VALUE}"
            log "rehearse: env read-back parsed the live ${WEB_ENV##*/} (${#ENV_VALUE} bytes)"
        elif [ "${ENV_VALUE}" != "${DIST_CSP}" ]; then
            PWA_FAIL="canterlot.env.${SHA} content differs"
        else
            CSP_EXPECT="${DIST_CSP}"
        fi
    fi
fi
if [ -z "${PWA_FAIL}" ]; then
    run_write "swap canterlot.env" ssh_web "cp '${WEB_ENV}.${SHA}' '${WEB_ENV}'" || PWA_FAIL="canterlot.env swap failed"
fi
if [ -z "${PWA_FAIL}" ]; then
    ssh_web "test -d '${WEB_RELEASES}/${SHA}'" || PWA_FAIL="web staged dir missing before build flip"
fi
CADDY_ID_BEFORE="$(ssh_web "docker inspect -f '{{.Id}}' $(shq "${WEB_CONTAINER}")" 2>/dev/null)" || CADDY_ID_BEFORE=""
if [ -z "${PWA_FAIL}" ]; then
    run_write "web build flip" web_locked 'cd "$1" && ln -s "releases/$2" build.new && mv -T build.new build && readlink "$3"' \
        "${WEB_SITES_BASE}" "${SHA}" "${WEB_BUILD_LINK}" >/dev/null || true
    WEB_LINK_AFTER="$(ssh_web "readlink '${WEB_BUILD_LINK}'" 2>/dev/null)" || WEB_LINK_AFTER=""
    [ "${WEB_LINK_AFTER}" = "releases/${WEB_EXPECT}" ] || PWA_FAIL="web build flip failed"
fi
if [ -z "${PWA_FAIL}" ]; then
    run_write "caddy recreate" web_locked 'cd "$1" && docker compose up -d "$2"' "${WEB_COMPOSE_DIR}" "${WEB_CONTAINER}" \
        || PWA_FAIL="caddy recreate failed"
fi
if [ -z "${PWA_FAIL}" ]; then
    CADDY_ID_AFTER="$(ssh_web "docker inspect -f '{{.Id}}' $(shq "${WEB_CONTAINER}")" 2>/dev/null)" || CADDY_ID_AFTER=""
    if [ -n "${CADDY_ID_BEFORE}" ] && [ "${CADDY_ID_BEFORE}" != "${CADDY_ID_AFTER}" ]; then
        log "caddy recreated once: ${CADDY_ID_BEFORE:0:12} -> ${CADDY_ID_AFTER:0:12}"
    else
        log "caddy container id unchanged (${CADDY_ID_AFTER:0:12})$(is_rehearse && printf ' (rehearse: no recreate)')"
    fi
    GOT_VERSION="$(container_pwa_sha)" || GOT_VERSION=""
    if ! sha_match "${GOT_VERSION:-}" "${WEB_EXPECT}"; then
        PWA_FAIL="pwa-version: container version.json (${GOT_VERSION:-unreadable}) is not ${WEB_EXPECT}"
    fi
fi
if [ -z "${PWA_FAIL}" ]; then
    if ! GOT_CSP="$(ssh_web "docker exec caddy printenv CANTERLOT_CSP")"; then
        PWA_FAIL="cannot read container CSP"
    elif [ "${GOT_CSP}" != "${CSP_EXPECT}" ]; then
        PWA_FAIL="container CSP differs from $(is_rehearse && printf 'the live env' || printf 'dist')"
    fi
fi
if [ -n "${PWA_FAIL}" ]; then
    soft "PWA failure (soft, hub is not rolled back): ${PWA_FAIL}; restoring previous build ${PREV_WEB_SHA}"
    WEB_SHA_EFF="${PREV_WEB_SHA}"
    if run_write "web build flip back" web_locked 'cd "$1" && ln -s "releases/$2" build.new && mv -T build.new build' "${WEB_SITES_BASE}" "${PREV_WEB_SHA}"; then
        log "PWA build flipped back to ${PREV_WEB_SHA}"
    else
        soft "PWA flip-back failed"
    fi
    if ssh_web "test -f '${WEB_ENV}.${PREV_WEB_SHA}'"; then
        run_write "restore env + caddy recreate" web_locked 'cp "$1" "$2" && cd "$3" && docker compose up -d "$4"' \
            "${WEB_ENV}.${PREV_WEB_SHA}" "${WEB_ENV}" "${WEB_COMPOSE_DIR}" "${WEB_CONTAINER}" \
            || soft "PWA env restore failed"
    else
        run_write "caddy recreate after flip-back" web_locked 'cd "$1" && docker compose up -d "$2"' "${WEB_COMPOSE_DIR}" "${WEB_CONTAINER}" \
            || soft "PWA recreate after flip-back failed"
    fi
else
    log "PWA on ${WEB_EXPECT}: build flipped, one Caddy recreate, version.json and CSP ok"
fi
PHASE="pwa-done"

# Диктовка: служба живёт из current хаба, поэтому после flip её перезапускают.
# Шаг стоит после доказанного старта воркера, чтобы ожидание рестарта и health не
# удлиняло простой. Всё мягкое: speech_sync не выходит и не трогает soft().
PHASE="speech"
speech_sync "${SHA}"

# Старые чанки PWA: после старта воркера и ДО любой чистки (открытые вкладки
# продолжают догружать свои чанки). Ошибки мягкие, как всё в PWA.
PHASE="merging-chunks"
merge_old_chunks() {
    local sources counts line bad=0
    if [ "${WEB_SHA_EFF}" != "${SHA}" ] && ! is_rehearse; then
        log "chunk merge skipped: PWA stayed on ${WEB_SHA_EFF}"
        return 0
    fi
    if ! sources="$(web_merge_sources "${SHA}")"; then
        soft "chunk merge: cannot list web releases"
        return 0
    fi
    if [ -z "${sources}" ]; then
        log "chunk merge: no older web release to merge"
        return 0
    fi
    log "chunk merge: copying _app/immutable of ${sources} into ${SHA} (no clobber)"
    # shellcheck disable=SC2086 # sources — список имён релизов через пробел
    run_write "merge old immutable chunks" web_locked "${WEB_MERGE_SCRIPT}" "${WEB_RELEASES}" "${SHA}" ${sources} \
        || { soft "chunk merge command failed"; return 0; }
    # shellcheck disable=SC2086
    if ! counts="$(web_merge_counts "${SHA}" ${sources})"; then
        soft "chunk merge: cannot count merged files"
        return 0
    fi
    while IFS= read -r line; do
        [ -n "${line}" ] || continue
        # shellcheck disable=SC2086
        set -- ${line}
        if is_rehearse; then
            log "rehearse: chunk merge from $1 would copy $3 of $2 files"
        elif [ "$3" = "0" ]; then
            log "chunk merge: every one of $2 immutable files of $1 is present in ${SHA}"
        else
            soft "chunk merge: $3 of $2 immutable files of $1 are missing in ${SHA}"
            bad=1
        fi
    done <<<"${counts}"
    if web_dist_files_intact "${SHA}"; then
        log "chunk merge: every dist web file of ${SHA} is byte-identical in the served release (sha256sum -c)"
    elif is_rehearse; then
        log "rehearse: dist web files of ${SHA} differ in the staged release (would be a soft failure after a real merge)"
        soft "dist web files of ${SHA} differ from the staged release"
    else
        soft "chunk merge: dist web files of ${SHA} changed in the served release"
        bad=1
    fi
    [ "${bad}" = "0" ] || log "chunk merge finished with problems (soft, see above)"
}
merge_old_chunks
PHASE="maintenance"
MAINT_LIVE_IDS="$(hub_psql "select id::text from executions where state='running' order by 1")"
maintenance_check "${MAINT_LIVE_IDS}" "${WEB_SHA_EFF}"

# Проверка под защитой ловушки: её провал — тоже STOP-and-report, не откат.
PHASE="verifying"
export DEPLOY_SOFT_KIND=verify
log "running verification (two reads)"
if is_rehearse; then
    # Репетиция проверяет живое состояние (развёрнутые релизы) тем же verify.sh;
    # его единственная запись (loopback-проба) печатается через run_write.
    "${SCRIPT_DIR}/verify.sh" --dist "${DIST_BASE:-${REPO_ROOT}/dist}" --state "${STATE_FILE}"
else
    "${SCRIPT_DIR}/verify.sh" "${SHA}" --dist "${DIST_BASE:-${REPO_ROOT}/dist}" --state "${STATE_FILE}" --web-sha "${WEB_SHA_EFF}"
fi
# База GeoIP для мест входа: после проверки, ошибки мягкие (место входа просто пусто).
PHASE="geoip"
geoip_refresh "${HUB_RELEASES}/${SHA}/bin/canterlot-hub"
# Чистка — после проверки и под своими /proc-guard'ами; цель и предыдущие релизы
# передаются как --keep и не могут быть удалены даже если выпали из трёх свежих.
# В репетиции retain.sh печатает план без --apply (ничего не удаляет).
PHASE="retention"
export DEPLOY_SOFT_KIND=retention
RETAIN_ARGS=(--dist "${DIST_BASE:-${REPO_ROOT}/dist}" --keep "${SHA}" --keep "${PREV_HUB_SHA}" --keep "${PREV_WEB_SHA}" --keep "${PREV_WORKER_SHA}")
if is_rehearse; then
    "${SCRIPT_DIR}/retain.sh" "${RETAIN_ARGS[@]}" || soft "retention plan failed"
elif ! "${SCRIPT_DIR}/retain.sh" --apply "${RETAIN_ARGS[@]}"; then
    soft "retention failed; artifacts stay, rerun retain.sh --apply after review"
fi
release_reservations
trap - INT TERM HUP
trap result_exit EXIT
if is_rehearse; then
    WRITES_PRINTED="$(wc -l <"${REHEARSE_COUNT_FILE}" 2>/dev/null || echo '?')"
    rm -f "${REHEARSE_COUNT_FILE}"  # lint: local-temp
    plan_text "${SHA}" "${DIST}" "${BASE_HUB}" "${BASE_WEB}" "${BASE_WORKER}" | sed 's/^/   | /'
    log "rehearse: speech: ${SPEECH_SUMMARY}; speech soft issues ${SPEECH_ISSUES} (never block the token)"
    if [ "${HARD_FAILURES}" != "0" ] || [ "${SOFT_FAILURES}" != "0" ]; then
        log "rehearsal FAIL: ${HARD_FAILURES} hard, ${SOFT_FAILURES} soft failures; ${WRITES_PRINTED} writes printed, none executed; no confirm token"
        write_result fail 1
        exit 1
    fi
    owner_checklist
    print_token
    log "rehearsal PASS: the whole write path ran against the live system; ${WRITES_PRINTED} writes printed as WOULD-WRITE, none executed"
    write_result pass 0
    exit 0
fi
log "cutover done: hub=${SHA}, worker=${SHA}, web=${WEB_SHA_EFF}, down ${WORKER_DOWNTIME}s (worker stop -> live), hub interruption ${HUB_INTERRUPTION}s, soft failures ${SOFT_FAILURES}, speech: ${SPEECH_SUMMARY} (speech soft issues ${SPEECH_ISSUES})"
owner_checklist
cat <<EOF
update note: the worker has connected to the new hub, so only forward fixes remain;
an old hub stops on VersionMissing and a DB restore now causes state_divergence.
hub back to a release without own sign-in (1.29 and older): restore the Caddy auth block first, remove [auth]
and [http.ws].session_check_interval from hub.toml, cancel pending sign-in pushes (row 0 of the rollback table).
worker back below this release: remove [execution].delegate_mcp_servers from worker.toml first.
hub back below the dictation release: remove [dictation] from hub.toml first; after any hub flip back restart
${SPEECH_UNIT}, or stop and disable it when that release has no bin/canterlot-speech (rollback-db.sh does it).
restore points: ${RUN_DUMP} in ${HUB_BACKUP_DIR} (quiesced, verified: the one for a DB restore), vzdump ${VZ_ARCHIVE:-archive} notes pre-${SHA}-${RUN_TS} (earlier, taken while the worker ran).
PWA alone: build to releases/${PREV_WEB_SHA}, ${WEB_ENV}.${PREV_WEB_SHA}, one recreate.
worker alone: flip current to ${PREV_WORKER_SHA}, restart (never below a live supervisor release).

=== section template (paste into .superpowers/deploy/pilot-deploy-log.md; the script does not commit) ===
## Update to X.Y.Z (\`${SHA}\`), <N> live executions, DD.MM, HH:MM-HH:MM UTC

Release: \`${SHA}\` (\`<full sha>\`, tag \`vX.Y.Z\`), built by \`scripts/build-release.sh\`; dist \`${DIST}\`. Procedure: \`scripts/deploy/cutover.sh\` (rehearsal/dry-run token, then --execute).

**Result: hub, PWA and worker on \`${SHA}\` (web ${WEB_SHA_EFF}), migrations ${MIGR_MAX} (0 failed), worker \`live\` after ${WORKER_DOWNTIME} s down (stop ${WORKER_STOP_ISO} UTC), hub interruption ${HUB_INTERRUPTION} s; recorded session PIDs/ticks intact; no automatic DB restore; capabilities and harnesses as declared by the release EXPECT; CSP equal, precompressed encodings observed, old chunks merged, maintenance clean, soft failures ${SOFT_FAILURES}; speech: ${SPEECH_SUMMARY}.**
### Steps
1. Dist: \`sha256sum -c\` OK; \`RELEASE source=\`; EXPECT; versions; glibc; openapi.
2. Rehearsal and/or dry-run: plan ${PLAN_HASH}, token issued <time>, logs <paths>.
3. Cutover: vzdump ${VZ_ARCHIVE:-<archive>} -> gates + inventory again -> stop -> pg_dump ${RUN_DUMP} + full read -> hub flip/restart (${HUB_INTERRUPTION} s) -> worker flip/start/live (down ${WORKER_DOWNTIME} s) -> PWA flip + one recreate.
4. Chunk merge, maintenance, verification (two reads), retention: <paste the log lines>.
### Deviations
- <none or describe>
### Left for the owner
- Authenticated checks (checklist above); <other>
EOF
write_result "done" 0
