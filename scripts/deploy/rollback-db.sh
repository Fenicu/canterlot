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

# Кластер и база те же, что у дампа: сверка ДО остановки хаба и любого dropdb. Ожидаемые значения
# печатает cutover в команде отката; без них — запись рядом с дампом (.identity); нет и её — громкое
# предупреждение (дамп старого прогона).
if [ -z "${EXPECT_SYS}${EXPECT_OID}" ]; then
    ID_FILE="$(ssh_hub "cat '${DUMP}.identity'" 2>/dev/null)" || ID_FILE=""
    EXPECT_SYS="$(printf '%s\n' "${ID_FILE}" | sed -n 's/^system_identifier=//p' | sed -n 1p)"
    EXPECT_OID="$(printf '%s\n' "${ID_FILE}" | sed -n 's/^database_oid=//p' | sed -n 1p)"
fi
# Фаза восстановления лежит рядом с дампом (<дамп>.restore-phase): verified, dropped, created <новый oid>,
# restored <oid>. dropdb и createdb меняют oid базы, поэтому повтор той же команды после частичного
# восстановления принимает ТОЛЬКО записанный oid; чужой oid без записи — отказ.
PHASE_FILE="${DUMP}.restore-phase"
RESTORE_PHASE=""
RESTORE_OID=""
if PH_RAW="$(ssh_hub "cat '${PHASE_FILE}'" 2>/dev/null)"; then
    RESTORE_PHASE="$(printf '%s\n' "${PH_RAW}" | sed -n 1p | cut -d' ' -f1)"
    RESTORE_OID="$(printf '%s\n' "${PH_RAW}" | sed -n 1p | cut -s -d' ' -f2)"
    case "${RESTORE_PHASE}" in
        verified | dropped | created | restored) ;;
        *) die "unreadable restore phase '${RESTORE_PHASE}' in ${PHASE_FILE}: refusing before the hub stops" ;;
    esac
    log "an earlier restore of this dump left the phase '${RESTORE_PHASE}${RESTORE_OID:+ ${RESTORE_OID}}': continuing from it"
fi
# База отсутствует по ответу самого кластера (а не «не удалось спросить»): запрос прошёл и строк нет.
db_exists_raw() {
    ssh_hub "runuser -u postgres -- psql -h '${HUB_PG_SOCKET}' -d postgres -tAc \"select 1 from pg_database where datname = '${HUB_PG_DB}'\""
}
db_is_missing() {
    local out
    out="$(db_exists_raw 2>/dev/null)" || return 1
    [ -z "${out}" ]
}
if [ -z "${EXPECT_SYS}" ] || [ -z "${EXPECT_OID}" ]; then
    log "WARNING: no recorded cluster identity for ${DUMP} (no --expect-system-id/--expect-db-oid, no ${DUMP}.identity): the cluster is NOT verified; read the target before going on"
elif [ "${RESTORE_PHASE}" = "dropped" ] || { [ -n "${RESTORE_PHASE}" ] && db_is_missing; }; then
    # Прежняя база снесена этим восстановлением: соединиться можно только с обслуживающей postgres.
    # Кластер всё равно сверяется. Сюда же попадает сбой между dropdb и записью фазы dropped
    # (в файле прежняя фаза, базы нет): фаза пишется только после сверки идентичности, поэтому
    # отсутствующая база при записанной фазе — след нашего же dropdb.
    CUR_SYS="$(ssh_hub "runuser -u postgres -- psql -h '${HUB_PG_SOCKET}' -d postgres -tAc \"select system_identifier::text from pg_control_system()\"")" || die "cannot read the cluster identity through the postgres database: refusing to touch the database"
    [ "${CUR_SYS}" = "${EXPECT_SYS}" ] || die "the cluster at ${HUB_PG_SOCKET} has system identifier ${CUR_SYS}, the dump was taken from ${EXPECT_SYS}: refusing before the hub stops (wrong cluster)"
    if [ "${RESTORE_PHASE}" = "dropped" ]; then
        log "cluster identity verified through the postgres database: system identifier ${EXPECT_SYS}; the earlier restore dropped ${HUB_PG_DB} (phase dropped), whatever carries that name now is dropped again before createdb"
    else
        log "cluster identity verified through the postgres database: system identifier ${EXPECT_SYS}; ${HUB_PG_DB} is missing (the earlier restore stopped between dropdb and the phase record, phase ${RESTORE_PHASE})"
    fi
else
    CUR_ID="$(pg_identity)" || die "cannot read the cluster identity of ${HUB_PG_DB} on ${HUB_PG_SOCKET}: refusing to touch the database"
    [ "${CUR_ID% *}" = "${EXPECT_SYS}" ] || die "the cluster at ${HUB_PG_SOCKET} has system identifier ${CUR_ID% *}, the dump was taken from ${EXPECT_SYS}: refusing before the hub stops (wrong cluster)"
    if [ "${CUR_ID#* }" = "${EXPECT_OID}" ]; then
        log "cluster identity verified: system identifier ${EXPECT_SYS}, database oid ${EXPECT_OID}"
    elif [ -n "${RESTORE_OID}" ] && { [ "${RESTORE_PHASE}" = "created" ] || [ "${RESTORE_PHASE}" = "restored" ]; } && [ "${CUR_ID#* }" = "${RESTORE_OID}" ]; then
        log "cluster identity verified: system identifier ${EXPECT_SYS}; replacement database oid ${RESTORE_OID} recorded by the earlier restore (phase ${RESTORE_PHASE}) accepted"
    else
        die "the database ${HUB_PG_DB} has database oid ${CUR_ID#* }, the dump was taken from oid ${EXPECT_OID}${RESTORE_OID:+ (an earlier restore recorded ${RESTORE_OID})}: refusing before the hub stops"
    fi
fi
write_phase() {
    run_write "restore phase ${1%% *}" ssh_hub "printf '%s\n' '$1' >'${PHASE_FILE}'" || die "cannot record the restore phase '$1' in ${PHASE_FILE}"
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
# Шаги по отдельности, фаза записывается после каждого: сбой посреди восстановления не теряет, где мы.
# Фаза created (база создана, но частично залита) продолжается с dropdb: --exit-on-error не
# восстанавливает поверх частичных данных.
# Записанную фазу повтор не затирает: verified поверх dropped или created <oid> при снесённой либо
# пересозданной базе заставлял следующий запуск отказывать на ранней сверке идентичности навсегда.
[ -n "${RESTORE_PHASE}" ] || write_phase "verified"
restore_fail() {
    log "$1: refusing to start anything on a partial database. The phase is '$(ssh_hub "cat '${PHASE_FILE}'" 2>/dev/null || echo unknown)' in ${PHASE_FILE}: run the same command again to continue. Needs the owner loudly"
    exit 1
}
# Вопрос «есть ли база» задаётся в любой фазе, в том числе dropped: createdb мог пройти, а чтение
# идентичности после него — оборваться. Тогда база уже есть, и createdb на ней упал бы.
DB_EXISTS="$(db_exists_raw)" || restore_fail "cannot ask the cluster whether ${HUB_PG_DB} exists"
if [ "${DB_EXISTS}" = "1" ]; then
    run_write "dropdb" ssh_hub "runuser -u postgres -- dropdb -h '${HUB_PG_SOCKET}' '${HUB_PG_DB}'" || restore_fail "dropdb FAILED"
fi
write_phase "dropped"
run_write "createdb" ssh_hub "runuser -u postgres -- createdb -h '${HUB_PG_SOCKET}' -O '${HUB_PG_USER}' '${HUB_PG_DB}'" || restore_fail "createdb FAILED"
NEW_ID="$(pg_identity)" || restore_fail "cannot read the identity of the recreated database"
write_phase "created ${NEW_ID#* }"
run_write "pg_restore" ssh_hub "runuser -u postgres -- pg_restore --exit-on-error -h '${HUB_PG_SOCKET}' -d '${HUB_PG_DB}' '${DUMP}'" || restore_fail "pg_restore FAILED"
write_phase "restored ${NEW_ID#* }"
log "pg_restore done"

log "starting hub"
run_write "start hub" hub_locked 'systemctl start "$1"' "${HUB_UNIT}" || die "hub failed to start after restore"
# Опрос, как в cutover.sh после рестарта хаба: старый хаб на только что залитой базе отвечает не
# сразу. Одна проба через 3 с объявляла удавшийся откат проваленным, а повтор команды снова
# останавливал хаб и заново сносил и заливал базу.
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
    die "old hub readyz FAILED after restore (${READYZ_TRIES} probes, last: ${READYZ:-nothing}): the database IS restored (phase restored), check the hub before running anything again. Needs the owner loudly"
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
