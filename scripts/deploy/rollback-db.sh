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
# После старта старого хаба служба диктовки идёт за его релизом (мягко): рестарт,
# а без bin/canterlot-speech в релизе — стоп и выключение юнита.
# Дамп — проверенный pg_dump прогона (canterlot-pre-<sha>-<ts>.dump): его снимают
# после стопа воркера, при затихших писателях, и это несущая точка восстановления.
# vzdump того же прогона (notes pre-<sha>-<ts>), если он был, снят раньше, пока воркер
# работал: это отдельная точка восстановления всего CT, и её восстановление теряет записи
# между vzdump и стопом воркера. Выпуск без миграций (cutover.sh --fast, --skip-ct-backup)
# vzdump не снимает: тогда проверенный дамп — единственная точка восстановления прогона.
# Дамп заливается в отдельную базу под уникальным именем восстановления; прежняя база
# снимается, только когда заливка прошла целиком, и после этого база восстановления
# переименовывается в штатное имя. Повтор той же команды продолжает с записанной фазы.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

DUMP=""
TO_RELEASE=""
EXPECT_SYS=""
EXPECT_OID=""
while (($#)); do
    case "$1" in
        --to) TO_RELEASE="$(short_sha "$2")"; shift 2 ;;
        --expect-system-id) EXPECT_SYS="$2"; shift 2 ;;
        --expect-db-oid) EXPECT_OID="$2"; shift 2 ;;
        -*) die "unknown flag: $1" ;;
        *) DUMP="$1"; shift ;;
    esac
done
[ -n "${DUMP}" ] || die "usage: rollback-db.sh <dump> [--to <release>]"
[ -z "${TO_RELEASE}" ] || require_release_id "${TO_RELEASE}"
FLIP_REQUESTED=0
[ -z "${TO_RELEASE}" ] || FLIP_REQUESTED=1
# Имена баз подставляются в SQL без кавычек: только строчные латинские буквы, цифры и подчёркивание.
[[ "${HUB_PG_DB}" =~ ^[a-z_][a-z0-9_]*$ ]] || die "HUB_PG_DB '${HUB_PG_DB}' is not a plain lowercase identifier: refusing"
# Тот же замок, что у cutover: откат не идёт параллельно выкату и не разбирает его состояние на лету.
take_deploy_lock "this rollback"
XDG_RUNTIME_DIR="/run/user/$(id -u)"
export XDG_RUNTIME_DIR
# Правило отката (ADR-0005): хаб без собственного входа (1.29 и раньше) верит
# Remote-User от прокси, поэтому блок входа в Caddy возвращается раньше отката; его
# конфиг deny_unknown_fields, поэтому новые ключи hub.toml убираются до старта.
# Отменять пуши входа в восстановленной базе не нужно: дамп снят до релиза.
if [ -n "${TO_RELEASE}" ]; then
    log "reminder: if ${TO_RELEASE} is a release without own sign-in (1.29 and older), BEFORE starting it: restore the Caddy auth block (basic_auth or forward_auth), and remove [auth] and [http.ws].session_check_interval from ${HUB_CONFIG}"
    log "reminder: if ${TO_RELEASE} is older than the dictation release, remove [dictation] from ${HUB_CONFIG} BEFORE starting it (deny_unknown_fields, like [provider_status]); ${SPEECH_UNIT} follows the release after the restore (restart, or stop and disable without bin/canterlot-speech)"
fi

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

# Чтение записи рядом с дампом: код 0 — файл есть (содержимое в stdout), 3 — файла нет, любой
# другой — чтение не удалось. Оборванное чтение не равно «файла нет»: прежде сбой ssh на этом
# шаге молча отключал всю сверку идентичности, включая записанный oid фазы.
remote_record() {
    ssh_hub "if test -e '$1'; then cat '$1'; else exit 3; fi"
}
# Запрос к кластеру через обслуживающую базу postgres: он работает в любой фазе, в том числе
# когда базы под штатным именем нет.
pg_query() {
    ssh_hub "runuser -u postgres -- psql -h '${HUB_PG_SOCKET}' -d postgres -tAc \"$1\""
}
# oid базы по имени; пустая строка — кластер ответил, что такой базы нет. Сбой запроса — не ноль.
db_oid() {
    local out
    out="$(pg_query "select oid::text from pg_database where datname = '$1'")" || return 1
    [[ -z "${out}" || "${out}" =~ ^[0-9]+$ ]] || return 1
    printf '%s' "${out}"
}

# Кластер и база те же, что у дампа: сверка ДО остановки хаба и любого dropdb. Ожидаемые значения
# печатает cutover в команде отката; без них — запись рядом с дампом (.identity); нет и её — громкое
# предупреждение (дамп старого прогона).
if [ -z "${EXPECT_SYS}${EXPECT_OID}" ]; then
    ID_RC=0
    ID_FILE="$(remote_record "${DUMP}.identity" 2>/dev/null)" || ID_RC=$?
    case "${ID_RC}" in
        0)
            EXPECT_SYS="$(printf '%s\n' "${ID_FILE}" | sed -n 's/^system_identifier=//p' | sed -n 1p)"
            EXPECT_OID="$(printf '%s\n' "${ID_FILE}" | sed -n 's/^database_oid=//p' | sed -n 1p)" ;;
        3) ;;
        *) die "cannot read ${DUMP}.identity (exit ${ID_RC}): a failed read is not a missing record; refusing before the hub stops" ;;
    esac
fi
# Фаза восстановления лежит рядом с дампом (<дамп>.restore-phase), одной строкой
# «<фаза> <имя восстановления> <oid прежней базы> <oid базы восстановления или ->»:
#   verified  прежняя база сверена, выбрано уникальное имя восстановления;
#   created   база восстановления создана, её oid записан;
#   loaded    pg_restore в неё прошёл целиком;
#   dropped   прежняя база снесена по записанному oid;
#   restored  база восстановления переименована в штатное имя. Это последняя фаза данных: повтор
#             только поднимает хаб и проверяет его, без stop, flip, dropdb и pg_restore (иначе
#             он сносил бы записи владельца, принятые после восстановления).
# Удаляется только база, чей oid записан в фазе. Чужая база под любым из двух имён — STOP до
# остановки хаба, без изменений.
PHASE_FILE="${DUMP}.restore-phase"
RESTORE_PHASE=""
REC_NAME=""
REC_OLD_OID=""
REC_OID=""
PH_RC=0
PH_RAW="$(remote_record "${PHASE_FILE}" 2>/dev/null)" || PH_RC=$?
case "${PH_RC}" in
    0)
        read -r RESTORE_PHASE REC_NAME REC_OLD_OID REC_OID _ <<<"$(printf '%s\n' "${PH_RAW}" | sed -n 1p)" || true
        case "${RESTORE_PHASE}" in
            verified) [ "${REC_OID}" = "-" ] || RESTORE_PHASE="bad" ;;
            created | loaded | dropped | restored) [[ "${REC_OID}" =~ ^[0-9]+$ ]] || RESTORE_PHASE="bad" ;;
            *) RESTORE_PHASE="bad" ;;
        esac
        [[ "${REC_NAME}" =~ ^${HUB_PG_DB}_rb_[0-9a-f]{8}$ ]] || RESTORE_PHASE="bad"
        [[ "${REC_OLD_OID}" =~ ^[0-9]+$ ]] || RESTORE_PHASE="bad"
        [ "${RESTORE_PHASE}" != "bad" ] || die "unreadable restore phase '$(printf '%s\n' "${PH_RAW}" | sed -n 1p)' in ${PHASE_FILE}: refusing before the hub stops"
        log "an earlier restore of this dump left the phase '${RESTORE_PHASE} ${REC_NAME} ${REC_OLD_OID} ${REC_OID}': continuing from it" ;;
    3) ;;
    *) die "cannot read ${PHASE_FILE} (exit ${PH_RC}): a failed read is not a missing phase; refusing before the hub stops" ;;
esac
write_phase() {
    RESTORE_PHASE="$1"
    run_write "restore phase $1" ssh_hub "printf '%s\n' '$1 ${REC_NAME} ${REC_OLD_OID} ${REC_OID}' >'${PHASE_FILE}'" \
        || die "cannot record the restore phase '$1 ${REC_NAME} ${REC_OLD_OID} ${REC_OID}' in ${PHASE_FILE}"
}

# STOP: дальше без владельца нельзя. Блок называет фазу и фактическую ссылку хаба; хаб остаётся
# в том состоянии, которое названо во втором аргументе.
rollback_stop() {
    local link
    link="$(ssh_hub "readlink '${HUB_CURRENT}'" 2>/dev/null)" || link="UNKNOWN (the read failed)"
    {
        printf '\n=== STOP: rollback-db needs the owner ===\n'
        printf 'reason:   %s\n' "$1"
        printf 'phase:    %s (%s)\n' "${RESTORE_PHASE:+${RESTORE_PHASE} ${REC_NAME} ${REC_OLD_OID} ${REC_OID}}" "${PHASE_FILE}"
        printf 'hub link: %s -> %s\n' "${HUB_CURRENT}" "${link}"
        printf 'hub:      %s\n' "$2"
        printf 'do not start the hub or the worker on this database before checking it by hand\n'
        printf '=== END STOP ===\n'
    } >&2
    die "$1"
}

# Сверка до остановки хаба. Кластер — по system identifier; базы под штатным именем и под именем
# восстановления — по oid из ответа кластера (через postgres, так что отсутствие базы видно как
# пустой ответ, а сбой запроса — как отказ).
CUR_SYS="$(pg_query "select system_identifier::text from pg_control_system()")" \
    || die "cannot read the cluster identity through the postgres database: refusing to touch the database"
if [ -n "${EXPECT_SYS}" ]; then
    [ "${CUR_SYS}" = "${EXPECT_SYS}" ] || die "the cluster at ${HUB_PG_SOCKET} has system identifier ${CUR_SYS}, the dump was taken from ${EXPECT_SYS}: refusing before the hub stops (wrong cluster)"
    log "cluster identity verified: system identifier ${EXPECT_SYS}"
else
    log "WARNING: no recorded cluster identity for ${DUMP} (no --expect-system-id, no ${DUMP}.identity): the cluster is NOT verified; read the target before going on"
fi
MAIN_OID="$(db_oid "${HUB_PG_DB}")" || die "cannot ask the cluster for the oid of ${HUB_PG_DB}: refusing before the hub stops"
REC_NOW=""
if [ -n "${REC_NAME}" ]; then
    REC_NOW="$(db_oid "${REC_NAME}")" || die "cannot ask the cluster for the oid of ${REC_NAME}: refusing before the hub stops"
fi
UNTOUCHED="not stopped by this run"
if [ -n "${RESTORE_PHASE}" ] && [ -n "${EXPECT_OID}" ] && [ "${REC_OLD_OID}" != "${EXPECT_OID}" ]; then
    die "${PHASE_FILE} records the old database oid ${REC_OLD_OID}, the dump was taken from oid ${EXPECT_OID}: refusing before the hub stops"
fi
# NEXT — первый шаг этого запуска: create, adopt, redo, load, drop_old, rename, mark, finish.
NEXT=""
case "${RESTORE_PHASE}" in
    "")
        [ -n "${MAIN_OID}" ] || die "${HUB_PG_DB} does not exist on ${HUB_PG_SOCKET} and no restore phase is recorded for this dump: refusing before the hub stops"
        if [ -n "${EXPECT_OID}" ] && [ "${MAIN_OID}" != "${EXPECT_OID}" ]; then
            die "the database ${HUB_PG_DB} has database oid ${MAIN_OID}, the dump was taken from oid ${EXPECT_OID}: refusing before the hub stops"
        fi
        [ -z "${EXPECT_OID}" ] || log "database identity verified: ${HUB_PG_DB} oid ${EXPECT_OID}"
        REC_OLD_OID="${MAIN_OID}"
        NEXT=create ;;
    verified | created)
        [ "${MAIN_OID}" = "${REC_OLD_OID}" ] \
            || rollback_stop "${HUB_PG_DB} has oid ${MAIN_OID:-none}, the phase records the untouched old database ${REC_OLD_OID}: it is not ours to drop" "${UNTOUCHED}"
        if [ -z "${REC_NOW}" ]; then
            NEXT=create
        elif [ "${RESTORE_PHASE}" = "verified" ]; then
            # Имя записано до createdb и уникально для этого восстановления: база под ним — наш
            # createdb, чей oid не успели записать. Она пустая и принимается, а не сносится.
            NEXT=adopt
        elif [ "${REC_NOW}" = "${REC_OID}" ]; then
            NEXT=redo
        else
            rollback_stop "${REC_NAME} has oid ${REC_NOW}, the phase records ${REC_OID}: it is not ours to drop" "${UNTOUCHED}"
        fi ;;
    loaded)
        [ "${REC_NOW}" = "${REC_OID}" ] \
            || rollback_stop "the loaded recovery database ${REC_NAME} has oid ${REC_NOW:-none}, the phase records ${REC_OID}" "${UNTOUCHED}"
        if [ "${MAIN_OID}" = "${REC_OLD_OID}" ]; then
            NEXT=drop_old
        elif [ -z "${MAIN_OID}" ]; then
            log "${HUB_PG_DB} is missing: the earlier restore dropped it after the load and stopped before recording the phase dropped"
            NEXT=rename
        else
            rollback_stop "${HUB_PG_DB} has oid ${MAIN_OID}, neither the old database ${REC_OLD_OID} nor the restored ${REC_OID}: it is not ours to drop" "${UNTOUCHED}"
        fi ;;
    dropped)
        if [ -z "${MAIN_OID}" ] && [ "${REC_NOW}" = "${REC_OID}" ]; then
            NEXT=rename
        elif [ "${MAIN_OID}" = "${REC_OID}" ] && [ -z "${REC_NOW}" ]; then
            log "${HUB_PG_DB} already has the restored oid ${REC_OID}: the earlier restore renamed it and stopped before recording the phase restored"
            NEXT=mark
        else
            rollback_stop "the phase is dropped, but ${HUB_PG_DB} has oid ${MAIN_OID:-none} and ${REC_NAME} has oid ${REC_NOW:-none} (expected none and ${REC_OID}): a database this restore did not create holds the name; nothing is dropped" "${UNTOUCHED}"
        fi ;;
    restored)
        [ "${MAIN_OID}" = "${REC_OID}" ] \
            || rollback_stop "the phase is restored, but ${HUB_PG_DB} has oid ${MAIN_OID:-none}, not the restored ${REC_OID}" "${UNTOUCHED}"
        rollback_stop "the restore already finished (phase restored): nothing is restored again" "${UNTOUCHED}" ;;
esac

# Старт хаба после восстановления и проверка: общая для обычного прогона и для повтора после
# restored. Повтор безопасен: systemctl start на работающем хабе ничего не делает.
finish_hub() {
    log "starting hub"
    run_write "start hub" hub_locked 'systemctl start "$1"' "${HUB_UNIT}" || die "hub failed to start after restore (the database IS restored, phase restored): running the same command again only starts and checks the hub. Needs the owner loudly"
    # Опрос, как в cutover.sh после рестарта хаба: старый хаб на только что залитой базе отвечает не
    # сразу. Одна проба через 3 с объявляла удавшийся откат проваленным.
    READYZ="FAIL"
    READYZ_TRIES=0
    for _ in $(seq 1 "${READYZ_POLLS:-30}"); do
        READYZ_TRIES=$((READYZ_TRIES + 1))
        READYZ="$(ssh_hub "curl -fsS -m 2 'http://${HUB_IP}:8080/readyz'" 2>/dev/null)" || READYZ="FAIL"
        if [ "${READYZ}" = '{"status":"ok"}' ]; then break; fi
        sleep 1
    done
    HUB_NOW="$(ssh_hub "readlink '${HUB_CURRENT}'" | sed 's|.*/||')" || HUB_NOW="UNKNOWN"
    if [ "${READYZ}" = '{"status":"ok"}' ]; then
        log "old hub readyz ok (probe ${READYZ_TRIES})"
    else
        log "speech unit ${SPEECH_UNIT}: $(speech_state "${HUB_CURRENT}/bin/canterlot-speech" 2>/dev/null || echo UNKNOWN) (not touched: the hub is not ready)"
        die "old hub readyz FAILED after restore (${READYZ_TRIES} probes, last: ${READYZ:-nothing}): the database IS restored (phase restored); running the same command again only starts and checks the hub, it does not restore again. Needs the owner loudly"
    fi
    # Диктовка следует за релизом хаба: рестарт или, без бинаря в релизе, стоп и
    # выключение юнита. Всё мягкое: откат базы от неё не зависит.
    if is_release_id "${HUB_NOW}"; then
        speech_sync "${HUB_NOW}"
    else
        speech_soft "cannot read the hub current release (${HUB_NOW}): ${SPEECH_UNIT} not touched"
        SPEECH_SUMMARY="not touched (hub release unknown)"
    fi
    log "rollback-db done: hub on ${HUB_NOW}, worker stays stopped (start it by hand); speech: ${SPEECH_SUMMARY} (speech soft issues ${SPEECH_ISSUES})"
}

log "rollback-db: refusing unless the worker is provably stopped"
assert_worker_stopped "before any change"

# Все предпосылки проверяются ДО остановки хаба: иначе битый дамп или
# несуществующий релиз оставляли бы хаб остановленным без плана восстановления.
log "restoring the run's quiesced pg_dump (taken after the worker stop); a vzdump of the run, if it took one (none on a run without migrations), is an earlier whole-CT point taken while the worker ran"
ssh_hub "test -s '${DUMP}'" || die "dump not found or empty: ${DUMP}"
verify_dump "${DUMP}" || die "dump verification failed before stopping the hub: ${DUMP}"
# Без --to хаб поднимется на том релизе, на который сейчас указывает current:
# он проверяется так же, как явная цель (а flip в этом случае не делается).
HUB_LINK_BEFORE="$(ssh_hub "readlink '${HUB_CURRENT}'")" || die "cannot read the hub current release"
if [ "${FLIP_REQUESTED}" = "0" ]; then
    TO_RELEASE="${HUB_LINK_BEFORE##*/}"
    require_release_id "${TO_RELEASE}"
    log "no --to given: the hub restarts on its current release ${TO_RELEASE} (no flip)"
fi
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

# Последняя сверка ровно перед первым изменением базы: между ранней проверкой и этой точкой
# воркер мог появиться (auto-restart, ручной запуск, осиротевший процесс).
assert_worker_stopped "immediately before dropdb"
restore_fail() {
    log "$1: refusing to start anything on a partial database. The phase is '$(ssh_hub "cat '${PHASE_FILE}'" 2>/dev/null || echo unknown)' in ${PHASE_FILE}: run the same command again to continue. Needs the owner loudly"
    exit 1
}
# Снос только базы, чей oid записан в фазе; oid перечитывается ровно перед dropdb.
drop_proven() {
    local now
    now="$(db_oid "$1")" || restore_fail "cannot read the oid of $1 before dropping it"
    [ "${now}" = "$2" ] || restore_fail "$1 has oid ${now:-none}, the phase records $2: refusing to drop it"
    run_write "dropdb $1" ssh_hub "runuser -u postgres -- dropdb -h '${HUB_PG_SOCKET}' '$1'" || restore_fail "dropdb $1 FAILED"
}
if [ -z "${REC_NAME}" ]; then
    REC_NAME="${HUB_PG_DB}_rb_$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
    [[ "${REC_NAME}" =~ ^${HUB_PG_DB}_rb_[0-9a-f]{8}$ ]] || die "cannot pick a recovery database name (${REC_NAME})"
fi
log "restoring ${DUMP} into ${REC_NAME} (createdb + full pg_restore --exit-on-error), then replacing ${HUB_PG_DB} with it"
if [ "${NEXT}" = "redo" ]; then
    # База восстановления создана этим восстановлением (oid записан), но залита частично:
    # --exit-on-error не восстанавливает поверх частичных данных.
    drop_proven "${REC_NAME}" "${REC_OID}"
    NEXT=create
fi
if [ "${NEXT}" = "create" ]; then
    REC_OID="-"
    write_phase verified
    run_write "createdb" ssh_hub "runuser -u postgres -- createdb -h '${HUB_PG_SOCKET}' -O '${HUB_PG_USER}' '${REC_NAME}'" || restore_fail "createdb ${REC_NAME} FAILED"
    REC_OID="$(db_oid "${REC_NAME}")" || restore_fail "cannot read the oid of the new database ${REC_NAME}"
    [ -n "${REC_OID}" ] || restore_fail "the new database ${REC_NAME} is missing after createdb"
    write_phase created
    NEXT=load
fi
if [ "${NEXT}" = "adopt" ]; then
    REC_OID="${REC_NOW}"
    log "adopting ${REC_NAME} (oid ${REC_OID}): the earlier createdb made it before its oid was recorded"
    write_phase created
    NEXT=load
fi
if [ "${NEXT}" = "load" ]; then
    run_write "pg_restore" ssh_hub "runuser -u postgres -- pg_restore --exit-on-error -h '${HUB_PG_SOCKET}' -d '${REC_NAME}' '${DUMP}'" || restore_fail "pg_restore FAILED (${HUB_PG_DB} is still the database from before the restore)"
    write_phase loaded
    NEXT=drop_old
fi
if [ "${NEXT}" = "drop_old" ]; then
    assert_worker_stopped "immediately before dropping ${HUB_PG_DB}"
    drop_proven "${HUB_PG_DB}" "${REC_OLD_OID}"
    write_phase dropped
    NEXT=rename
fi
if [ "${NEXT}" = "rename" ]; then
    run_write "rename ${REC_NAME}" pg_query "alter database ${REC_NAME} rename to ${HUB_PG_DB}" >/dev/null || restore_fail "renaming ${REC_NAME} to ${HUB_PG_DB} FAILED"
    NEXT=mark
fi
if [ "${NEXT}" = "mark" ]; then
    MAIN_OID="$(db_oid "${HUB_PG_DB}")" || restore_fail "cannot read the oid of ${HUB_PG_DB} after the rename"
    [ "${MAIN_OID}" = "${REC_OID}" ] || restore_fail "${HUB_PG_DB} has oid ${MAIN_OID:-none} after the rename, expected ${REC_OID}"
    write_phase restored
fi
log "pg_restore done: ${HUB_PG_DB} is the restored database (oid ${REC_OID})"
finish_hub
