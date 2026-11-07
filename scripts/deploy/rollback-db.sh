#!/usr/bin/env bash
# shellcheck disable=SC2016 # remote scripts expand their own positional args
# При sourcing ничего не исполнять: скрипт запускает боевой выкат только как
# программа (урок 1.16.1: source cutover-скрипта остановил воркер).
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Ручной откат базы хаба на дамп этого прогона (Task 12, строка таблицы).
# Запускается ТОЛЬКО вручную (или оркестратором по тексту STOP-and-report),
# никогда из cutover.sh. После восстановления старого хаба на новой базе
# воркер НЕ стартует: его поднимает владелец отдельной командой из отчёта.
# Использование: scripts/deploy/rollback-db.sh <dump> [--to <release>]
# Отказывается работать, пока воркер активен (рассинхронизация!), и отказывается
# что-либо стартовать, если pg_restore упал.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

DUMP=""
TO_RELEASE=""
while (($#)); do
    case "$1" in
        --to) TO_RELEASE="$(short_sha "$2")"; shift 2 ;;
        -*) die "unknown flag: $1" ;;
        *) DUMP="$1"; shift ;;
    esac
done
[ -n "${DUMP}" ] || die "usage: rollback-db.sh <dump> [--to <release>]"
[ -z "${TO_RELEASE}" ] || require_release_id "${TO_RELEASE}"
XDG_RUNTIME_DIR="/run/user/$(id -u)"
export XDG_RUNTIME_DIR

# Охранник юнита на время восстановления: даже если кто-то (или сам systemd после
# auto-restart) попробует поднять воркер, запуск не состоится. `systemctl mask`
# для этого не годится: systemd отказывает, когда файл юнита лежит в каталоге
# пользовательских юнитов (документированная установка). Поэтому — обратимый
# drop-in: RefuseManualStart=yes блокирует ручной start, ExecCondition=/bin/false —
# запуск по зависимости. Применяется через daemon-reload и проверяется по
# свойствам загруженного юнита (без попытки запуска). Снимается на любом выходе,
# чтобы владелец мог поднять воркер вручную по отчёту.
WORKER_GUARD_DIR="${WORKER_UNIT_DIR}/${WORKER_UNIT}.service.d"
WORKER_GUARD_FILE="${WORKER_GUARD_DIR}/zz-rollback-db-guard.conf"
WORKER_GUARDED=0
RESERVED_RELEASE=""
remove_worker_guard() {
    WORKER_GUARDED=0
    run_write "remove worker guard" rm -f "${WORKER_GUARD_FILE}" || log "could not remove ${WORKER_GUARD_FILE}: needs the owner loudly"
    rmdir "${WORKER_GUARD_DIR}" 2>/dev/null || true
    run_write "daemon-reload" systemctl --user daemon-reload || log "daemon-reload failed after removing the guard: needs the owner loudly"
}
install_worker_guard() {
    local refuse cond
    WORKER_GUARDED=1
    run_write "create worker guard dir" mkdir -p "${WORKER_GUARD_DIR}" || die "cannot create ${WORKER_GUARD_DIR}"
    printf '[Unit]\nRefuseManualStart=yes\n[Service]\nExecCondition=/bin/false\n' >"${WORKER_GUARD_FILE}" \
        || die "cannot write ${WORKER_GUARD_FILE}"
    run_write "daemon-reload" systemctl --user daemon-reload || die "daemon-reload failed: worker guard not loaded"
    refuse="$(systemctl --user show "${WORKER_UNIT}" -p RefuseManualStart --value 2>/dev/null || true)"
    cond="$(systemctl --user show "${WORKER_UNIT}" -p ExecCondition --value 2>/dev/null || true)"
    [ "${refuse}" = "yes" ] || die "worker guard not loaded (RefuseManualStart=${refuse:-unknown}): refusing to touch the database"
    case "${cond}" in
        *"/bin/false"*) ;;
        *) die "worker guard not loaded (ExecCondition=${cond:-empty}): refusing to touch the database" ;;
    esac
}
cleanup_rollback() {
    local rc=$?
    if [ -n "${RESERVED_RELEASE}" ]; then
        run_write "release reservation" unreserve_releases hub "${RESERVED_RELEASE}" || log "could not release the reservation of ${RESERVED_RELEASE}: .reserved.${RESERVED_RELEASE} remains in ${HUB_BASE}"
        RESERVED_RELEASE=""
    fi
    if [ "${WORKER_GUARDED}" = "1" ]; then
        remove_worker_guard
    fi
    exit "${rc}"
}
trap cleanup_rollback EXIT

# Ни один процесс воркера не должен жить (классификация — find_control_worker в lib.sh).
assert_worker_stopped() {
    local phase="$1"
    local worker_show active_state sub_state main_pid found_worker
    if ! worker_show="$(systemctl --user show "${WORKER_UNIT}" -p ActiveState,SubState 2>/dev/null)"; then
        die "cannot query ${WORKER_UNIT} state (${phase}): refusing to touch the database"
    fi
    active_state="$(printf '%s\n' "${worker_show}" | sed -n 's/^ActiveState=//p')"
    sub_state="$(printf '%s\n' "${worker_show}" | sed -n 's/^SubState=//p')"
    if [ -z "${active_state}" ] || [ -z "${sub_state}" ]; then
        die "cannot parse ${WORKER_UNIT} state (${phase}): refusing to touch the database"
    fi
    case "${active_state}/${sub_state}" in
        inactive/dead|failed/*) ;;
        *)
            die "worker ${WORKER_UNIT} is ${active_state}/${sub_state} (${phase}): stop it first (systemctl --user stop ${WORKER_UNIT}), otherwise it diverges on the restored DB" ;;
    esac
    if ! main_pid="$(systemctl --user show "${WORKER_UNIT}" -p MainPID --value 2>/dev/null)"; then
        die "cannot query ${WORKER_UNIT} MainPID (${phase}): refusing to touch the database"
    fi
    case "${main_pid}" in
        ""|0) ;;
        *)
            # MainPID мог остаться в файле юнита после падения: он безопасен только
            # если процесса с этим PID больше нет. Живой (в том числе чужой из-за
            # переиспользования PID) — отказ.
            if kill -0 "${main_pid}" 2>/dev/null; then
                die "worker ${WORKER_UNIT} still has live MainPID ${main_pid} (${phase}): refusing to touch the database"
            fi
            log "worker MainPID ${main_pid} is not alive; treating the worker as stopped (${phase})" ;;
    esac
    if found_worker="$(find_control_worker)"; then
        die "${found_worker} (${phase}): refusing to touch the database"
    fi
    log "worker is provably stopped (${active_state}/${sub_state}, no MainPID, no worker process; ${phase})"
}

log "rollback-db: refusing unless the worker is provably stopped"
assert_worker_stopped "before any change"

# Все предпосылки проверяются ДО остановки хаба: иначе битый дамп или
# несуществующий релиз оставляли бы хаб остановленным без плана восстановления.
ssh_hub "test -s '${DUMP}'" || die "dump not found or empty: ${DUMP}"
verify_dump "${DUMP}" || die "dump verification failed before stopping the hub: ${DUMP}"
# Без --to хаб поднимется на том релизе, на который сейчас указывает current:
# он проверяется так же, как явная цель (а flip в этом случае не делается).
FLIP_REQUESTED=0
if [ -n "${TO_RELEASE}" ]; then
    FLIP_REQUESTED=1
else
    TO_RELEASE="$(ssh_hub "readlink '${HUB_CURRENT}'" | sed 's|.*/||')" || die "cannot read the hub current release"
    require_release_id "${TO_RELEASE}"
    log "no --to given: the hub restarts on its current release ${TO_RELEASE} (no flip)"
fi
if [ -n "${TO_RELEASE}" ]; then
    # Резерв под lock: retain не удалит цель между проверкой и flip.
    run_write "reserve release" reserve_releases hub "${TO_RELEASE}" || die "cannot reserve release ${TO_RELEASE}"
    RESERVED_RELEASE="${TO_RELEASE}"
    ssh_hub "test -d '${HUB_RELEASES}/${TO_RELEASE}/bin' \
        && test -f '${HUB_RELEASES}/${TO_RELEASE}/RELEASE' \
        && test -f '${HUB_RELEASES}/${TO_RELEASE}/SHA256SUMS' \
        && test -x '${HUB_RELEASES}/${TO_RELEASE}/bin/canterlot-hub'" \
        || die "target release ${TO_RELEASE} is missing or incomplete"
    if [ "${FLIP_REQUESTED}" = "1" ] && ssh_hub "test -e '${HUB_BASE}/current.new'"; then
        die "leftover ${HUB_BASE}/current.new: clean it up before rollback"
    fi
fi
# На время восстановления воркер не только остановлен, но и под охранником:
# auto-restart/systemd не поднимут его посреди dropdb/createdb/pg_restore.
log "guarding worker ${WORKER_UNIT} (drop-in) for the duration of the restore"
run_write "stop worker" systemctl --user stop "${WORKER_UNIT}" || die "cannot stop worker before the restore"
install_worker_guard
assert_worker_stopped "after stop+guard, before hub stop"
log "prerequisites verified (dump, target release), stopping hub ${HUB_UNIT}"
run_write "stop hub" hub_locked 'systemctl stop "$1"' "${HUB_UNIT}" || die "cannot stop hub"

if [ "${FLIP_REQUESTED}" = "1" ]; then
    log "flipping hub to previous release ${TO_RELEASE}"
    HUB_FLIP_FAILED=0
    if ! HUB_LINK_AFTER="$(run_write "hub flip" hub_locked "${HUB_FLIP_SCRIPT}" \
            "${HUB_BASE}" "${TO_RELEASE}" "${HUB_CURRENT}")"; then
        HUB_FLIP_FAILED=1
    else
        [ "${HUB_LINK_AFTER}" = "releases/${TO_RELEASE}" ] || HUB_FLIP_FAILED=1
        [ "$(ssh_hub "readlink -m '${HUB_CURRENT}'")" = "${HUB_RELEASES}/${TO_RELEASE}" ] || HUB_FLIP_FAILED=1
    fi
    if [ "${HUB_FLIP_FAILED}" = "1" ]; then
        HUB_NOW="$(ssh_hub "readlink '${HUB_CURRENT}'" 2>/dev/null || echo UNKNOWN)"
        log "hub flip to ${TO_RELEASE} FAILED (current is ${HUB_NOW}): starting the hub again on its current release; database is untouched"
        run_write "remove leftover current.new" hub_locked 'rm -f "$1"' "${HUB_BASE}/current.new" || log "could not remove leftover current.new: needs the owner loudly"
        run_write "start hub" hub_locked 'systemctl start "$1"' "${HUB_UNIT}" || log "could not start the hub after the failed flip: needs the owner loudly"
        HUB_NOW="$(ssh_hub "readlink '${HUB_CURRENT}'" 2>/dev/null || echo UNKNOWN)"
        log "hub restarted on ${HUB_NOW}; no database restore was attempted. Needs the owner loudly"
        exit 1
    fi
    log "hub current -> releases/${TO_RELEASE}"
fi

log "restoring ${DUMP} (dropdb + createdb + full pg_restore --exit-on-error)"
# Последняя сверка ровно перед dropdb: между ранней проверкой и этой точкой
# воркер мог появиться (auto-restart, ручной запуск, осиротевший процесс).
assert_worker_stopped "immediately before dropdb"
if ssh_hub "runuser -u postgres -- dropdb '${HUB_PG_DB}' && runuser -u postgres -- createdb -O '${HUB_PG_USER}' '${HUB_PG_DB}' && runuser -u postgres -- pg_restore --exit-on-error -d '${HUB_PG_DB}' '${DUMP}'"; then
    log "pg_restore done"
else
    log "pg_restore FAILED: refusing to start anything on a partial database. Needs the owner loudly"
    exit 1
fi

log "starting hub"
run_write "start hub" hub_locked 'systemctl start "$1"' "${HUB_UNIT}" || die "hub failed to start after restore"
sleep 3
READYZ="$(ssh_hub "curl -fsS -m 5 'http://${HUB_IP}:8080/readyz'" 2>/dev/null || echo FAIL)"
if [ "${READYZ}" = '{"status":"ok"}' ]; then
    log "old hub readyz ok"
else
    die "old hub readyz FAILED after restore (got: ${READYZ}): needs the owner loudly"
fi
log "rollback-db done: hub on $(ssh_hub "readlink '${HUB_CURRENT}'" | sed 's|.*/||'), worker stays stopped (start it by hand)"
