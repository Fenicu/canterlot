#!/usr/bin/env bash
# shellcheck disable=SC2016 # remote scripts expand their own positional args
# При sourcing ничего не исполнять: скрипт запускает боевой выкат только как
# программа (урок 1.16.1: source cutover-скрипта остановил воркер).
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Выкат релиза одним прогоном: ворота, стоп воркера, свежие vzdump и pg_dump
# (проверенный чтением каждого байта), flip хаба, рестарт хаба, flip PWA и один
# recreate Caddy, flip и старт воркера, затем проверка.
#
# Политика отката (решение владельца: никакой автоматики по базе):
# - До flip хаба: любая неудача или сигнал — только рестарт воркера на его
#   собственном предыдущем релизе (хаб не тронут, пара заведомо совместимая).
# - С flip хаба и дальше: СТОП и доклад. Никаких действий с базой, никакого
#   flip назад; воркер останавливаем и оставляем остановленным (его сессии живут
#   в сопровождениях). Печатаем точное состояние и ручные команды восстановления
#   (таблица Task 12), включая восстановление дампа этого прогона.
#   Ручной откат базы — только scripts/deploy/rollback-db.sh, никогда отсюда.
# - PWA-ошибки мягкие всегда: записать, вернуть предыдущий build, продолжить.
# Использование: scripts/deploy/cutover.sh SHA --source DIR [--dist DIR] [--state FILE] [--dry-run] [--allow-skipped]
# --dry-run: SHA обязателен (цель никогда не подменяется развёрнутым релизом). Читаемые
#   ворота: preflight и target-check против ЦЕЛИ (dist, залитые каталоги, версии,
#   миграции, CSP), verify против развёрнутого релиза как базы; печать плана, без единой
#   записи; write-шаги печатаются как NOT RUN.
#   Ненулевой выход, если хоть одни ворота не прошли или не были выполнены
#   (--allow-skipped разрешает незапущенные читаемые проверки явно).
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
ALLOW_SKIPPED=0
SHADOW=0
ALLOW_SAME_TARGET=0
while (($#)); do
    case "$1" in
        --source) SOURCE_DIR="$2"; shift 2 ;;
        --dist) DIST_BASE="$2"; shift 2 ;;
        --state) STATE_FILE="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --allow-skipped) ALLOW_SKIPPED=1; shift ;;
        --shadow) SHADOW=1; shift ;;
        --execute) EXECUTE_ID="$2"; shift 2 ;;
        --confirm) CONFIRM="$2"; shift 2 ;;
        -*) die "unknown flag: $1" ;;
        *) SHA="$(short_sha "$1")"; shift ;;
    esac
done
XDG_RUNTIME_DIR="/run/user/$(id -u)"
export XDG_RUNTIME_DIR

# Целевой релиз НИКОГДА не подменяется развёрнутым: dry-run проверяет именно цель
# (dist, залитые каталоги, версии, миграции, CSP), а развёрнутый релиз — это база.
if [ "${DRY_RUN}" = "1" ]; then
    [ -z "${EXECUTE_ID}" ] || SHA="$(short_sha "${EXECUTE_ID}")"
    [ -n "${SHA}" ] || die "dry-run needs the TARGET sha: cutover.sh SHA --dry-run (the deployed release is only the baseline, never a substitute)"
else
    # Боевой прогон — только явным --execute и только с токеном свежего dry-run.
    # Позиционный SHA в боевом режиме не принимается: опечатка в аргументе не
    # должна останавливать прод.
    [ -n "${EXECUTE_ID}" ] || die "real cutover requires --execute <release-id>; run --dry-run first and pass its CONFIRM token with --confirm"
    [ -z "${SHA}" ] || [ "$(short_sha "${SHA}")" = "$(short_sha "${EXECUTE_ID}")" ] \
        || die "--execute ${EXECUTE_ID} contradicts positional sha ${SHA}"
    SHA="$(short_sha "${EXECUTE_ID}")"
    require_release_id "${SHA}"
    [ -n "${CONFIRM}" ] || die "real cutover requires --confirm <token> printed by --dry-run on the same UTC day"
    EXPECTED_CONFIRM="$(confirm_token "${SHA}")"
    [ "${CONFIRM}" = "${EXPECTED_CONFIRM}" ] \
        || die "refusing: wrong --confirm (expected the token printed by the current dry-run; tokens are valid for the UTC day)"
    [ "${SHADOW}" = "0" ] || die "--shadow is a dry-run flag and cannot be used for a real cutover"
fi
require_release_id "${SHA}"
if [ "${DRY_RUN}" = "1" ] && [ "${SHADOW}" = "1" ]; then
    ALLOW_SAME_TARGET=1
    log "shadow dry-run: the target ${SHA} may already be deployed; equality is allowed and noted"
fi
if [ "${DRY_RUN}" = "1" ]; then
    DRY_BASE_HUB="$(deployed_sha hub)"
    DRY_BASE_WEB="$(deployed_sha web)"
    DRY_BASE_WORKER="$(deployed_sha worker)"
    log "dry-run mode: TARGET ${SHA}; baseline (deployed): hub=${DRY_BASE_HUB} web=${DRY_BASE_WEB} worker=${DRY_BASE_WORKER}; printing the plan, no writes"
fi
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SOURCE_DIR="${SOURCE_DIR:-${REPO_ROOT}}"
DIST="${DIST_BASE:-${REPO_ROOT}/dist}/${SHA}"
STATE_FILE="${STATE_FILE:-/tmp/canterlot-deploy-${SHA}.state}"

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
    CT_LOCK="$(printf '%s' "${CT_STATUS}" | grep -o 'lock[^,]*' | head -1 || true)"
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

if [ "${DRY_RUN}" = "1" ]; then
    # Доказательно только чтение, но читаемых проверок должно быть столько же,
    # сколько в боевом прогоне: preflight (конфиги, ownership, журналы) и verify
    # (два чтения, тики, CSP, сайты) выполняются целиком. Ничего не пишется:
    # state-файл не передаём, spec_compat только печатается.
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
    if ! DRY_LIVE_IDS="$(hub_psql "select id::text from executions where state='running' order by 1")"; then
        soft "cannot list live executions for the dry-run snapshot"
    else
        for exec_id in ${DRY_LIVE_IDS:-}; do
            log "exec ${exec_id}: $(proc_ticks "${exec_id}" | grep supervise | head -1)"
        done
    fi
    VERIFY_EXTRA=()
    [ "${ALLOW_SKIPPED}" = "0" ] || VERIFY_EXTRA=(--allow-skipped)
    if ! "${SCRIPT_DIR}/verify.sh" --dry-run --dist "${DIST_BASE:-${REPO_ROOT}/dist}" "${VERIFY_EXTRA[@]}"; then
        hard "verify --dry-run failed"
        NOT_RUN=1
    fi
    cat <<EOF
dry-run plan for ${SHA} (write steps are NOT RUN by design):
  baseline (deployed): hub=${DRY_BASE_HUB} web=${DRY_BASE_WEB} worker=${DRY_BASE_WORKER}
  target checked read-only: dist (SHA256SUMS, source=, versions, glibc, openapi), staged dirs on every host, migrations, CSP
  NOT RUN (write steps, never executed by a dry-run):
   - preflight snapshot/state file
   - worker stop (systemctl --user stop ${WORKER_UNIT}); session processes keep running
   - fresh vzdump (notes pre-${SHA}) + fresh pg_dump canterlot-pre-${SHA}-<ts>.dump
   - hub flip: cd ${HUB_BASE} && ln -s releases/${SHA} + mv -T; restart ${HUB_UNIT} once
   - PWA: canterlot.env.${SHA}, flip build, 'docker compose up -d ${WEB_CONTAINER}'
   - worker flip + start; verify.sh twice (the read-only verify gates above did run)
   - precompressed Content-Encoding probe (would start a one-off loopback Caddy inside the container)
   - retention (retain.sh --apply --keep ${SHA} ...)
  WOULD RUN (only if the gates pass):
   - preflight.sh ${SHA} --source DIR --state FILE
   - the write steps above, in this order
  rollback points would be: this run's vzdump + pg_dump (manual restore only, see report on failure)
  CONFIRM=$(confirm_token "${SHA}")   # pass with: cutover.sh --execute ${SHA} --confirm $(confirm_token "${SHA}") on $(date -u +%F)
EOF
    if [ "${NOT_RUN}" != "0" ] && [ "${ALLOW_SKIPPED}" = "0" ]; then
        log "dry-run FAIL: a read-only preflight/verify gate did not run (use --allow-skipped only to acknowledge a known gap)"
    fi
    if [ "${NOT_RUN}" != "0" ] && [ "${ALLOW_SKIPPED}" = "1" ]; then
        log "dry-run: read-only gate gaps explicitly allowed (--allow-skipped)"
    fi
    if [ "${SOFT_FAILURES}" != "${SOFT_BEFORE}" ] || [ "${HARD_FAILURES}" != "${HARD_BEFORE}" ] || [ "${NOT_RUN}" != "0" ]; then
        log "dry-run FAIL: ${HARD_FAILURES} hard, ${SOFT_FAILURES} soft failures, not-run=${NOT_RUN}"
        exit 1
    fi
    log "dry-run PASS: every read-only preflight/verify gate ran, nothing written"
    exit 0
fi

# --- боевой прогон ниже. Сначала preflight с полным набором, он же пишет снимок. ---
"${SCRIPT_DIR}/preflight.sh" "${SHA}" --source "${SOURCE_DIR}" --dist "${DIST_BASE:-${REPO_ROOT}/dist}" --state "${STATE_FILE}"

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
# Резерв целевого и предыдущего релизов на время операции: retain их не удалит
# между проверкой и flip (и после flip, пока оператор может вернуться назад).
RESERVED=0
release_reservations() {
    [ "${RESERVED}" = "1" ] || return 0
    RESERVED=0
    unreserve_releases hub "${SHA}" "${PREV_HUB_SHA}" || log "could not release hub reservations: .reserved.* remain in ${HUB_BASE}"
    unreserve_releases web "${SHA}" "${PREV_WEB_SHA}" || log "could not release web reservations: .reserved.* remain in ${WEB_SITES_BASE}"
    unreserve_releases worker "${SHA}" "${PREV_WORKER_SHA}" || log "could not release worker reservations: .reserved.* remain in ${WORKER_BASE}"
}
RESERVED=1
if ! { reserve_releases hub "${SHA}" "${PREV_HUB_SHA}" && reserve_releases web "${SHA}" "${PREV_WEB_SHA}" \
        && reserve_releases worker "${SHA}" "${PREV_WORKER_SHA}"; }; then
    die "cannot reserve releases for the run"
fi
trap 'release_reservations' EXIT

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
read_gates || die "read gates failed before worker stop"

# Миграции ДО первого изменения.
PREV_MIGR_MAX="$(hub_psql "select coalesce(max(version),0) from _sqlx_migrations where success")"
PREV_MIGR_FAILED="$(hub_psql "select count(*) from _sqlx_migrations where not success")"
log "previous releases: hub=${PREV_HUB_SHA} web=${PREV_WEB_SHA} worker=${PREV_WORKER_SHA}; migrations max=${PREV_MIGR_MAX} failed=${PREV_MIGR_FAILED}"

# Единственная ловушка на весь прогон: ставится один раз, снимается только после
# прошедшей проверки. Внутри обработчика сигналы глушатся, повторный вход невозможен.
# До flip хаба: только рестарт воркера (хаб и база нетронуты — пара заведомо совместимая).
# С flip хаба: остановить воркер и только доложить (см. report_and_stop).
PHASE="early"
WORKER_WAS_ACTIVE=0
DONE=0
ROLLING_BACK=0
RUN_TS="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DUMP="canterlot-pre-${SHA}-${RUN_TS}.dump"
if systemctl --user is-active --quiet "${WORKER_UNIT}"; then WORKER_WAS_ACTIVE=1; fi

report_and_stop() {
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
  live state:
    hub current        = ${hub_link} (was ${PREV_HUB_SHA} before this run)
    hub readyz         = ${hub_readyz}
    hub migrations     = max ${migr_max} failed ${migr_failed} (was max ${PREV_MIGR_MAX} failed ${PREV_MIGR_FAILED})
    worker             = ${worker_state}, current ${worker_link} (was ${PREV_WORKER_SHA})
    web build          = ${web_link} (was releases/${PREV_WEB_SHA})
    live executions    = ${live_n} (sessions keep running in their supervisors and buffer journals)
    this run's dump   = ${HUB_BACKUP_DIR}/${RUN_DUMP} (verified readable before the hub flip: yes)
  manual recovery (Task 12 rollback table — run by hand, in this order).
  Every flip below takes the same host lock as cutover and retention, and re-checks
  the target release inside it:
    1. hub broken, no new migration (flip back + one restart):
         ${hub_back_cmd}
    2. hub broken after a new migration: full DB restore from this run's dump (manual only):
         scripts/deploy/rollback-db.sh '${HUB_BACKUP_DIR}/${RUN_DUMP}' --to ${PREV_HUB_SHA}
         # it refuses while the worker is active, stops the hub, runs dropdb + createdb -O canterlot
         # + pg_restore --exit-on-error, starts the old hub and checks readyz loudly; on a failed
         # restore it starts nothing. Never start the worker on a restored DB by hand without review.
    3. PWA broken: flip build back, restore env, one recreate:
         ${web_back_cmd}
    4. worker broken or stopped: flip ${WORKER_CURRENT} back, then start (never below a live supervisor release):
         ${worker_back_cmd}
         systemctl --user start ${WORKER_UNIT}
    5. when everything is resolved, release this run's reservations (retention keeps these releases until then):
         ${hub_unres_cmd}
         ${web_unres_cmd}
         ${worker_unres_cmd}
EOF
}

recover_before_hub_flip() {
    # Хаб и база не тронуты: поднимаем воркер на его прежнем релизе.
    log "STOP before hub flip (phase ${PHASE}): hub and DB untouched, restarting worker"
    PREV_LINK="$(readlink "${WORKER_CURRENT}" 2>/dev/null | sed 's|.*/||')"
    if [ "${PREV_LINK}" != "${PREV_WORKER_SHA}" ]; then
        log "worker current is ${PREV_LINK}, expected previous release ${PREV_WORKER_SHA}: NOT starting it"
        release_reservations
        exit 1
    fi
    if [ "${WORKER_WAS_ACTIVE}" = "1" ]; then
        if ! systemctl --user is-active --quiet "${WORKER_UNIT}" 2>/dev/null; then
            worker_locked 'systemctl --user start "$1"' "${WORKER_UNIT}" || log "worker start command failed: needs the owner loudly"
        fi
        if systemctl --user is-active --quiet "${WORKER_UNIT}" 2>/dev/null; then
            PREV_EXE="$(readlink "/proc/$(systemctl --user show "${WORKER_UNIT}" -p MainPID --value 2>/dev/null)/exe" 2>/dev/null || echo unknown)"
            case "${PREV_EXE}" in
                "${WORKER_RELEASES}/${PREV_WORKER_SHA}/"*)
                    log "worker restarted on its previous release ${PREV_WORKER_SHA} (hub ${PREV_HUB_SHA} untouched)" ;;
                *)
                    log "worker is active but its exe is ${PREV_EXE}, not previous release ${PREV_WORKER_SHA}: needs the owner loudly" ;;
            esac
        else
            log "worker could NOT be restarted on ${PREV_WORKER_SHA}: needs the owner loudly"
        fi
    else
        log "worker was not active before the run; left stopped"
    fi
    release_reservations
    exit 1
}

post_flip_stop() {
    # После flip хаба любая неудача/сигнал: остановить воркер (сессии живут
    # в сопровождениях) и только доложить. Ни базы, ни хаба не трогаем —
    # ручной откат базы описан в отчёте и выполняется только человеком.
    # stop идемпотентен и обязателен даже для activating/auto-restart:
    # is-active в этих состояниях ненулевой, но юнит сам поднимется позже.
    log "failure after the hub flip: stopping worker ${WORKER_UNIT} (sessions survive in supervisors)"
    systemctl --user stop "${WORKER_UNIT}" || log "worker stop command failed: needs the owner loudly"
    STOPPED_STATE="$(systemctl --user show "${WORKER_UNIT}" -p ActiveState --value 2>/dev/null || echo unknown)"
    STOPPED_PID="$(systemctl --user show "${WORKER_UNIT}" -p MainPID --value 2>/dev/null || echo unknown)"
    if [ "${STOPPED_STATE}" = "inactive" ] && [ "${STOPPED_PID}" = "0" ]; then
        log "worker verified stopped (inactive, no MainPID)"
    else
        log "worker did NOT stop cleanly (ActiveState=${STOPPED_STATE}, MainPID=${STOPPED_PID}): needs the owner loudly"
    fi
    report_and_stop
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
    case "${PHASE}" in
        early|worker-stopping|worker-stopped|backing-up|dumping|dump-verifying|backed-up)
            recover_before_hub_flip
            ;;
        hub-flipping)
            # Проверяем фактическое состояние current, а не флаг фазы: mv мог
            # успеть/не успеть. Точно прежний -> безопасное допеределочное
            # восстановление; изменился или неизвестен -> STOP-and-report.
            # Под тем же host lock, что и flip: если ssh оборвался, а удалённый flip ещё держит
            # lock, чтение дождётся его конца и увидит настоящий current. Не удалось
            # прочитать (транспорт, таймаут lock) — состояние неизвестно, то есть post-flip.
            HUB_NOW_ABS="$(hub_locked 'readlink -m "$1"' "${HUB_CURRENT}" 2>/dev/null)" || HUB_NOW_ABS="UNKNOWN"
            if [ -n "${PREV_HUB_SHA}" ] && [ "${HUB_NOW_ABS}" = "${HUB_RELEASES}/${PREV_HUB_SHA}" ]; then
                log "hub flip failed but current is provably unchanged (${HUB_NOW_ABS}): pre-flip recovery"
                hub_locked 'rm -f "$1"' "${HUB_BASE}/current.new" || log "could not remove leftover current.new: needs the owner loudly"
                recover_before_hub_flip
            else
                log "hub flip failed with current ${HUB_NOW_ABS} (was releases/${PREV_HUB_SHA}): treating as post-flip"
                post_flip_stop
            fi
            ;;
        *)
            post_flip_stop
            ;;
    esac
}
trap on_end EXIT INT TERM HUP
cutover_hard() { log "HARD-FAIL: $*"; exit 1; }

log "deployed hub release before cutover: ${PREV_HUB_SHA}"

# Ворота непосредственно перед стопом воркера.
read_gates || cutover_hard "read gates failed"
[ "$(ssh_hub "readlink '${HUB_CURRENT}'" | sed 's|.*/||')" = "${PREV_HUB_SHA}" ] || cutover_hard "hub current moved unexpectedly"
# Инвентарь сопровождения заново, прямо перед стопом: каждый записанный процесс
# обязан быть жив с тем же тиком; новые процессы (в том числе новые супервизоры
# параллельных сессий) допустимы и печатаются, но не сравниваются как множество.
if [ -f "${STATE_FILE}" ]; then
    snapshot_drift "${STATE_FILE}" "before worker stop" \
        || cutover_hard "a recorded session process disappeared or changed its start tick before the stop"
    log "recorded session PIDs and ticks intact before the stop (new children allowed)"
fi

log "stopping worker ${WORKER_UNIT} (sessions survive in supervisors)"
WORKER_STOP_EPOCH="$(date -u +%s)"
WORKER_STOP_ISO="$(date -u '+%Y-%m-%d %H:%M:%S')"
PHASE="worker-stopping"
systemctl --user stop "${WORKER_UNIT}"
PHASE="worker-stopped"
sleep 2
# Остановка юнита не доказывает отсутствие воркера: управляющий `canterlot-worker run`,
# запущенный вне systemd, пережил бы бэкапы и миграцию хаба. Тот же охранник, что в
# rollback-db: супервизоры и релеи сессий пропускаются, любой другой воркер — отказ.
if FOREIGN_WORKER="$(find_control_worker)"; then
    cutover_hard "${FOREIGN_WORKER} after the unit stop: refusing to back up and migrate under a live worker"
fi
log "no control worker process left after the unit stop"
# Инвариант сессий без воркера: каждый записанный процесс жив с тем же тиком.
# Новые транзиентные дети допустимы (урок 1.17.0) и только печатаются.
snapshot_drift "${STATE_FILE}" "after worker stop" \
    || cutover_hard "a recorded session process disappeared or changed its start tick after the worker stop"
log "recorded session PIDs and ticks unchanged after worker stop (new transient children allowed)"

log "fresh vzdump and pg_dump under a unique name"
PHASE="backing-up"
ssh_pmx "vzdump ${HUB_CTID} --mode snapshot --storage ${PBS_STORAGE} --notes-template 'pre-${SHA}-${RUN_TS}'" \
    || cutover_hard "vzdump failed"
PHASE="dumping"
ssh_hub "install -d -m 0700 -o postgres '${HUB_BACKUP_DIR}' && runuser -u postgres -- pg_dump -Fc -f '${HUB_BACKUP_DIR}/${RUN_DUMP}' '${HUB_PG_DB}'" \
    || cutover_hard "pg_dump failed"
# Проверка дампа ДО flip хаба: TOC читается и каждый блок данных распаковывается.
# Без проверенного дампа дальше только рестарт воркера (см. on_end, фазы до flip).
# Сам восстановитель живёт в lib.sh: в этом файле не должно быть исполняемых
# команд работы с базой — только напечатанная ручная процедура.
PHASE="dump-verifying"
if verify_dump "${HUB_BACKUP_DIR}/${RUN_DUMP}"; then
    log "dump fully readable: ${RUN_DUMP} (TOC lists _sqlx_migrations)"
else
    cutover_hard "dump verification failed for ${RUN_DUMP}"
fi
PHASE="backed-up"
log "restore points: vzdump notes pre-${SHA}-${RUN_TS}, pg_dump ${RUN_DUMP} (verified)"

# Хаб: flip с абсолютным cd (урок: flip в /root), проверка ссылки, один рестарт.
RESTART_TS="$(ssh_hub "date -u '+%Y-%m-%d %H:%M:%S'")"
# Значение пришло с хоста и попадёт в команду journalctl: только формат даты.
[[ "${RESTART_TS}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}\ [0-9]{2}:[0-9]{2}:[0-9]{2}$ ]] || cutover_hard "unexpected hub timestamp: '${RESTART_TS}'"
PHASE="hub-flipping"
HUB_LINK_AFTER="$(hub_locked "${HUB_FLIP_SCRIPT}" "${HUB_BASE}" "${SHA}" "${HUB_CURRENT}")"
# Цель обязана быть ровно releases/<sha>; плюс абсолютная сверка разрешения ссылки.
[ "${HUB_LINK_AFTER}" = "releases/${SHA}" ] || cutover_hard "hub current did not move to ${SHA} (now ${HUB_LINK_AFTER})"
[ "$(ssh_hub "readlink -m '${HUB_CURRENT}'")" = "${HUB_RELEASES}/${SHA}" ] \
    || cutover_hard "hub current resolves elsewhere"
PHASE="hub-flipped"
PHASE="hub-restarting"
hub_locked 'systemctl restart "$1"' "${HUB_UNIT}" || cutover_hard "hub restart failed"
log "hub restarted once; waiting for readyz"
READYZ_OK=0
for _ in $(seq 1 30); do
    if [ "$(ssh_hub "curl -fsS -m 2 'http://${HUB_IP}:8080/readyz'" 2>/dev/null)" = '{"status":"ok"}' ]; then READYZ_OK=1; break; fi
    sleep 1
done
[ "${READYZ_OK}" = "1" ] || cutover_hard "hub readyz failed after restart"
MIGR_MAX="$(hub_psql "select coalesce(max(version),0) from _sqlx_migrations where success")"
[ "${MIGR_MAX}" = "${EXPECTED_MIGR}" ] || cutover_hard "migrations max ${MIGR_MAX}, expected ${EXPECTED_MIGR}"
[ "$(hub_psql "select count(*) from _sqlx_migrations where not success")" = "0" ] || cutover_hard "failed migrations present"
if ! HUB_ERRLOG="$(ssh_hub "journalctl -u '${HUB_UNIT}' --since '${RESTART_TS}' --no-pager")"; then
    cutover_hard "cannot read hub journal after restart"
fi
MIGR_ERR="$(printf '%s' "${HUB_ERRLOG}" | grep -c migration_error || true)"
[ "${MIGR_ERR}" = "0" ] || cutover_hard "migration_error in hub journal"
HUB_PID="$(ssh_hub "systemctl show '${HUB_UNIT}' -p MainPID --value")"
[ "$(ssh_hub "readlink '/proc/${HUB_PID}/exe'")" = "${HUB_RELEASES}/${SHA}/bin/canterlot-hub" ] \
    || cutover_hard "hub process not from release ${SHA}"
PHASE="hub-verified"
log "hub on ${SHA}: readyz ok, migrations ${MIGR_MAX}, process from the new release"

# PWA: всё мягкое (урок: PWA никогда не откатывает хаб). Ловушка остаётся взведённой:
# мягкие ошибки вызывают только soft(), а не выход, так что on_end не срабатывает.
PHASE="pwa"
log "PWA step (all failures soft)"
PWA_FAIL=""
WEB_SHA_EFF="${SHA}"
DIST_CSP=""
pwa_read_gates || PWA_FAIL="pwa pre-gates imperfect"
if [ -z "${PWA_FAIL}" ]; then
    # Читаем csp.txt с явным статусом: printf с упавшим cat иначе записал бы
    # CANTERLOT_CSP="" и объявил это успехом.
    if ! DIST_CSP="$(cat "${DIST}/csp.txt" 2>/dev/null)" || [ -z "${DIST_CSP}" ]; then
        PWA_FAIL="dist csp.txt is missing or empty: ${DIST}/csp.txt"
    fi
fi
if [ -z "${PWA_FAIL}" ]; then
    printf 'CANTERLOT_CSP="%s"\n' "${DIST_CSP}" | recv_to "${WEB_SSH}" "cat > '${WEB_ENV}.${SHA}' && chmod 0600 '${WEB_ENV}.${SHA}'" \
        || PWA_FAIL="canterlot.env.${SHA} write failed"
fi
if [ -z "${PWA_FAIL}" ]; then
    if ! ENV_WRITTEN="$(ssh_web "cat '${WEB_ENV}.${SHA}'")"; then
        PWA_FAIL="cannot read back canterlot.env.${SHA}"
    elif [ "$(printf '%s' "${ENV_WRITTEN}" | sed 's/^CANTERLOT_CSP="//; s/"$//')" != "${DIST_CSP}" ]; then
        PWA_FAIL="canterlot.env.${SHA} content differs"
    fi
fi
if [ -z "${PWA_FAIL}" ]; then
    ssh_web "cp '${WEB_ENV}.${SHA}' '${WEB_ENV}'" || PWA_FAIL="canterlot.env swap failed"
fi
if [ -z "${PWA_FAIL}" ]; then
    ssh_web "test -d '${WEB_RELEASES}/${SHA}'" || PWA_FAIL="web staged dir missing before build flip"
fi
if [ -z "${PWA_FAIL}" ]; then
    WEB_LINK_AFTER="$(web_locked 'cd "$1" && ln -s "releases/$2" build.new && mv -T build.new build && readlink "$3"' \
        "${WEB_SITES_BASE}" "${SHA}" "${WEB_BUILD_LINK}" || true)"
    [ "${WEB_LINK_AFTER}" = "releases/${SHA}" ] || PWA_FAIL="web build flip failed"
fi
if [ -z "${PWA_FAIL}" ]; then
    web_locked 'cd "$1" && docker compose up -d "$2"' "${WEB_COMPOSE_DIR}" "${WEB_CONTAINER}" \
        || PWA_FAIL="caddy recreate failed"
fi
if [ -z "${PWA_FAIL}" ]; then
    GOT_VERSION="$(container_pwa_sha)"
    if ! sha_match "${GOT_VERSION:-}" "${SHA}"; then
        PWA_FAIL="pwa-version: container version.json (${GOT_VERSION:-unreadable}) is not ${SHA}"
    fi
fi
if [ -z "${PWA_FAIL}" ]; then
    if ! GOT_CSP="$(ssh_web "docker exec caddy printenv CANTERLOT_CSP")"; then
        PWA_FAIL="cannot read container CSP"
    elif [ "${GOT_CSP}" != "${DIST_CSP}" ]; then
        PWA_FAIL="container CSP differs from dist"
    fi
fi
if [ -n "${PWA_FAIL}" ]; then
    soft "PWA failure (soft, hub is not rolled back): ${PWA_FAIL}; restoring previous build ${PREV_WEB_SHA}"
    WEB_SHA_EFF="${PREV_WEB_SHA}"
    if web_locked 'cd "$1" && ln -s "releases/$2" build.new && mv -T build.new build' "${WEB_SITES_BASE}" "${PREV_WEB_SHA}"; then
        log "PWA build flipped back to ${PREV_WEB_SHA}"
    else
        soft "PWA flip-back failed"
    fi
    if ssh_web "test -f '${WEB_ENV}.${PREV_WEB_SHA}'"; then
        web_locked 'cp "$1" "$2" && cd "$3" && docker compose up -d "$4"' \
            "${WEB_ENV}.${PREV_WEB_SHA}" "${WEB_ENV}" "${WEB_COMPOSE_DIR}" "${WEB_CONTAINER}" \
            || soft "PWA env restore failed"
    else
        web_locked 'cd "$1" && docker compose up -d "$2"' "${WEB_COMPOSE_DIR}" "${WEB_CONTAINER}" \
            || soft "PWA recreate after flip-back failed"
    fi
else
    log "PWA on ${SHA}: build flipped, one Caddy recreate, version.json and CSP ok"
fi
PHASE="pwa-done"

# Воркер: ворота перед стартом жёсткие — новый воркер стартует только
# против живого, полностью смигрированного хаба (пара совместима по построению).
worker_start_gates() {
    [ "$(ssh_hub "curl -fsS -m 5 'http://${HUB_IP}:8080/readyz'" 2>/dev/null)" = '{"status":"ok"}' ] \
        || cutover_hard "hub readyz not ok before worker start"
    [ "$(hub_psql "select coalesce(max(version),0) from _sqlx_migrations where success")" = "${EXPECTED_MIGR}" ] \
        || cutover_hard "hub migrations not at ${EXPECTED_MIGR} before worker start"
    [ "$(hub_psql "select count(*) from _sqlx_migrations where not success")" = "0" ] \
        || cutover_hard "failed hub migrations before worker start"
}
worker_start_gates
PHASE="worker-flipping"
worker_locked 'cd "$1" && ln -s "releases/$2" current.new && mv -T current.new current' \
    "${WORKER_BASE}" "${SHA}" || cutover_hard "worker current did not move"
link_points_to "${WORKER_CURRENT}" "${WORKER_BASE}" "releases/${SHA}" \
    || cutover_hard "worker current did not move"
PHASE="worker-starting"
worker_locked 'systemctl --user start "$1"' "${WORKER_UNIT}" || cutover_hard "worker start failed"
# Ждём именно state="live" из журнала юнита, как это делает дежурный лог.
# Голое state="live" (метка времени — префикс journald), без time="…" после него.
# Никаких `[ … ] && break`: под set -e первый неудачный poll не должен убивать
# ожидание (урок 1.17.0).
if ! LAST_STATE="$(live_from_journal "@$((WORKER_STOP_EPOCH - 1))" "${WORKER_LIVE_WAIT:-90}")"; then
    cutover_hard "worker did not reach state=\"live\" after start (last state: ${LAST_STATE:-none})"
fi
WORKER_LIVE_EPOCH="$(date -u +%s)"
WORKER_DOWNTIME=$((WORKER_LIVE_EPOCH - WORKER_STOP_EPOCH))
log "worker state=\"live\" after start; measured down time: ${WORKER_DOWNTIME} s (stop ${WORKER_STOP_ISO} UTC)"
if [ "${WORKER_DOWNTIME}" -gt 180 ]; then
    soft "worker down time ${WORKER_DOWNTIME} s exceeds the 3-minute outage grace; machine_outages will gain a row"
fi
systemctl --user is-active --quiet "${WORKER_UNIT}" || cutover_hard "worker not active after start"
WEXE="$(readlink "/proc/$(systemctl --user show "${WORKER_UNIT}" -p MainPID --value)/exe")"
case "${WEXE}" in
    "${WORKER_RELEASES}/${SHA}/"*) log "worker process from the new release" ;;
    *) cutover_hard "worker process not from ${SHA}: ${WEXE}" ;;
esac
PHASE="worker-started"
# Записанные процессы сессий живы с теми же тиками; новые дети допустимы.
snapshot_drift "${STATE_FILE}" "after worker start" \
    || cutover_hard "a recorded session process disappeared or changed its start tick after the worker start"
log "recorded session PIDs and ticks unchanged after worker start (new transient children allowed)"

# Проверка под защитой ловушки: её провал — тоже STOP-and-report, не откат.
PHASE="verifying"
log "running verification (two reads)"
"${SCRIPT_DIR}/verify.sh" "${SHA}" --dist "${DIST_BASE:-${REPO_ROOT}/dist}" --state "${STATE_FILE}" --web-sha "${WEB_SHA_EFF}"
# Чистка — после проверки и под своими /proc-guard'ами; цель и предыдущие релизы
# передаются как --keep и не могут быть удалены даже если выпали из трёх свежих.
if ! "${SCRIPT_DIR}/retain.sh" --apply --dist "${DIST_BASE:-${REPO_ROOT}/dist}" \
        --keep "${SHA}" "${PREV_HUB_SHA}" "${PREV_WEB_SHA}" "${PREV_WORKER_SHA}"; then
    soft "retention failed; artifacts stay, rerun retain.sh --apply after review"
fi
release_reservations
trap - EXIT INT TERM HUP
log "cutover done: hub=${SHA}, worker=${SHA}, web=${WEB_SHA_EFF}, down ${WORKER_DOWNTIME}s, soft failures ${SOFT_FAILURES}"
cat <<EOF
update note: the worker has connected to the new hub, so only forward fixes remain;
an old hub stops on VersionMissing and a DB restore now causes state_divergence.
restore points: ${RUN_DUMP} in ${HUB_BACKUP_DIR}, vzdump notes pre-${SHA}-${RUN_TS}.
PWA alone: build to releases/${PREV_WEB_SHA}, ${WEB_ENV}.${PREV_WEB_SHA}, one recreate.
worker alone: flip current to ${PREV_WORKER_SHA}, restart (never below a live supervisor release).

=== section template (paste into .superpowers/deploy/pilot-deploy-log.md; the script does not commit) ===
## Update to X.Y.Z (\`${SHA}\`), <N> live executions, DD.MM, HH:MM-HH:MM UTC

Release: \`${SHA}\` (\`<full sha>\`, branch \`<branch>\`, tag \`vX.Y.Z\`), built by \`scripts/build-release.sh\` in <build host>; dist \`.superpowers/deploy/dist/${SHA}\` verified read-only. Procedure: \`scripts/deploy\` (one scripted block).

**Result: hub, PWA and worker on \`${SHA}\`, migrations <max> (0 failed), worker \`live\` after ${WORKER_DOWNTIME} s down (stop ${WORKER_STOP_ISO} UTC); recorded session PIDs/ticks intact; <N> executions running; no automatic DB restore. Capabilities: \`claude_histories_v1\`, \`path_probe_v1\`; harnesses claude_code/codex/opencode. CSP equal, precompressed encodings observed, journal error counts 0.**
### Steps
1. Dist: \`sha256sum -c\` OK; \`RELEASE source=\` matches; \`--version\`; no \`libssl\`; max GLIBC ≤ 2.41; openapi equal.
2. Preflight: <live executions and supervisor releases>; specs/spec_compat; configs by key names; migrations <old> -> <new>.
3. Stage: exact trees on hub/web/worker, current/build untouched.
4. Dry-run: CONFIRM token, no writes.
5. Cutover: stop -> vzdump -> pg_dump + TOC/full read -> hub flip/restart -> PWA flip + one recreate -> worker flip/start; down ${WORKER_DOWNTIME} s.
6. Verification: two reads; capabilities; CSP; precompressed; journal counts; PIDs/ticks.
### Deviations
- <none or describe>
### Left for the owner
- Authenticated checks; <other>
EOF
