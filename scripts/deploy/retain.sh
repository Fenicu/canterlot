#!/usr/bin/env bash
# shellcheck disable=SC2016 # remote scripts expand their own positional args
# При sourcing ничего не исполнять: скрипт запускает боевой выкат только как
# программа (урок 1.16.1: source cutover-скрипта остановил воркер).
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Чистка старых релизов: держит RETAIN_COUNT свежих на каждом компоненте плюс каждый
# релиз, из которого живёт процесс. Удаление только буквальными абсолютными путями,
# каждый проверен перед удалением: не цель current/build, ни один процесс не
# исполняет файл из каталога. Без --apply только печатает, ничего не удаляет.
# Использование: scripts/deploy/retain.sh [--apply]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

APPLY=0
DIST_BASE=""
KEEP_IDS=""
while (($#)); do
    case "$1" in
        --apply) APPLY=1; shift ;;
        --dist) DIST_BASE="$2"; shift 2 ;;
        --keep) KEEP_IDS+=" $(short_sha "$2")"; shift 2 ;;
        -*) die "unknown flag: $1" ;;
        *) die "unexpected argument: $1" ;;
    esac
done
XDG_RUNTIME_DIR="/run/user/$(id -u)"
export XDG_RUNTIME_DIR
REMOVE_LIST="$(mktemp)"
trap 'rm -f "${REMOVE_LIST}"' EXIT

HUB_CURRENT_TARGET="$(deployed_sha hub)"
WEB_BUILD_TARGET="$(deployed_sha web)"
WORKER_CURRENT_TARGET="$(deployed_sha worker)"
require_release_id "${HUB_CURRENT_TARGET}"
require_release_id "${WEB_BUILD_TARGET}"
require_release_id "${WORKER_CURRENT_TARGET}"
for keep in ${KEEP_IDS:-}; do require_release_id "${keep}"; done

# Релиз, который реально обслуживает запущенный Caddy: symlink build мог уже
# уйти вперёд (или назад), а контейнер держит старый mount. Читаем version.json
# из контейнера и сопоставляем с каталогом релиза; не смогли — не удаляем вслепую.
if ! CADDY_VERSION_RAW="$(ssh_web "docker exec '${WEB_CONTAINER}' cat '${WEB_CONTAINER_VERSION_JSON}'")"; then
    die "cannot read version.json from the running ${WEB_CONTAINER} container"
fi
CADDY_VERSION="$(printf '%s' "${CADDY_VERSION_RAW}" | sed -n 's/.*"version"[ ]*:[ ]*"\([0-9a-zA-Z]\+\)".*/\1/p')"
[ -n "${CADDY_VERSION}" ] || die "cannot parse version.json from the running ${WEB_CONTAINER} container"
CADDY_LIVE_RELEASE=""
for raw in $(ssh_web "ls -dt '${WEB_RELEASES}'/*/"); do
    dir="${raw%/}"
    name="${dir##*/}"
    if sha_match "${name}" "${CADDY_VERSION}"; then
        CADDY_LIVE_RELEASE="${name}"
        break
    fi
done
[ -n "${CADDY_LIVE_RELEASE}" ] || die "caddy serves ${CADDY_VERSION}, but no release directory matches it"
require_release_id "${CADDY_LIVE_RELEASE}"

# Релиз процесса хаба (живой) и релизы живых супервизоров (эта машина). Только имена.
# Не можем узнать живой релиз — громкая смерть, а не тихий пропуск (иначе снесли бы
# релиз остановленного хаба).
HUB_MAIN_PID="$(ssh_hub "systemctl show '${HUB_UNIT}' -p MainPID --value")" \
    || die "cannot query hub unit state"
if [ "${HUB_MAIN_PID}" = "0" ] || [ -z "${HUB_MAIN_PID}" ]; then
    die "hub unit not running, cannot determine live release"
fi
HUB_EXE_RELEASE="$(ssh_hub "readlink '/proc/${HUB_MAIN_PID}/exe'" | sed "s|.*releases/||; s|/bin/.*||")" \
    || die "cannot read hub process exe"
[ -n "${HUB_EXE_RELEASE}" ] || die "cannot determine hub live release"
require_release_id "${HUB_EXE_RELEASE}"
WORKER_LIVE="$(for d in /proc/[0-9]*/exe; do
    target="$(readlink "${d}" 2>/dev/null || true)"
    case "${target}" in
        "${WORKER_BASE}"/releases/*/bin/*) printf '%s\n' "${target}" | sed "s|.*releases/||; s|/bin/.*||" ;;
    esac
done | sort -u | tr '\n' ' ')"
for rel in ${WORKER_LIVE}; do require_release_id "${rel}"; done
log "live: hub exe from ${HUB_EXE_RELEASE}; worker exes from [${WORKER_LIVE}]; caddy serves ${CADDY_LIVE_RELEASE} (${CADDY_VERSION}); currents hub=${HUB_CURRENT_TARGET} web=${WEB_BUILD_TARGET} worker=${WORKER_CURRENT_TARGET}"

# Список каталогов релизов компонента, от новых к старым (по mtime).
# Печатает план; удаления складывает в REMOVE_LIST строками "host|abspath".
plan_releases() {
    local label="$1" list="$2" hostkind="$3" live_set="$4"
    local rank=0
    # shellcheck disable=SC2086
    for raw in ${list}; do
        # Слеш на конце делал имя пустым, а пустой образец совпадал со всем
        # (урок: ничего никогда не удалялось — или удалялось не то).
        dir="${raw%/}"
        rank=$((rank + 1))
        name="${dir##*/}"
        [ -n "${name}" ] || die "refusing empty release name from: ${raw}"
        is_release_id "${name}" || die "refusing odd path: ${dir}"
        case " ${live_set} " in
            *" ${name} "*) log "keep ${dir} (live or current)" ;;
            *)
                if [ "${rank}" -le "${RETAIN_COUNT}" ]; then
                    log "keep ${dir} (within newest ${RETAIN_COUNT})"
                else
                    log "REMOVE ${dir}"
                    printf '%s|%s\n' "${hostkind}" "${dir}" >>"${REMOVE_LIST}"
                fi ;;
        esac
    done
    log "${label}: ${rank} releases total"
}

plan_releases "hub" "$(ssh_hub "ls -dt '${HUB_RELEASES}'/*/")" "hub" \
    "${HUB_EXE_RELEASE} ${HUB_CURRENT_TARGET}${KEEP_IDS}"
plan_releases "web" "$(ssh_web "ls -dt '${WEB_RELEASES}'/*/")" "web" \
    "${WEB_BUILD_TARGET} ${CADDY_LIVE_RELEASE}${KEEP_IDS}"
plan_releases "worker" "$(ls -dt "${WORKER_RELEASES}"/*/)" "local" \
    "${WORKER_LIVE} ${WORKER_CURRENT_TARGET}${KEEP_IDS}"

# Локальный dist: держать 3 свежих плюс всё, что передано в --keep (цель и
# текущие релизы). Удаление — только буквальные абсолютные пути под DIST_BASE.
if [ -n "${DIST_BASE}" ] && [ -d "${DIST_BASE}" ]; then
    rank=0
    for dist_dir in $(ls -dt "${DIST_BASE}"/*/ 2>/dev/null || true); do
        dist_dir="${dist_dir%/}"
        name="${dist_dir##*/}"
        case "${name}" in *-logs) continue ;; esac
        is_release_id "${name}" || die "refusing odd dist path: ${dist_dir}"
        rank=$((rank + 1))
        case " ${KEEP_IDS} " in
            *" ${name} "*) log "keep ${dist_dir} (passed with --keep)" ;;
            *)
                if [ "${rank}" -le "${RETAIN_COUNT}" ]; then
                    log "keep ${dist_dir} (within newest ${RETAIN_COUNT})"
                else
                    log "REMOVE ${dist_dir}"
                    printf 'localdist|%s\n' "${dist_dir}" >>"${REMOVE_LIST}"
                fi ;;
        esac
    done
    log "dist: ${rank} release dirs under ${DIST_BASE}"
fi

# Дампы БД на CT: держать 3 свежих.
# Нет ни одного дампа — не ошибка (ls вернёт 2): чистить просто нечего.
DUMPS="$(ssh_hub "ls -t '${HUB_BACKUP_DIR}'/canterlot-pre-*.dump 2>/dev/null" || true)"
rank=0
# shellcheck disable=SC2086
for dump in ${DUMPS:-}; do
    rank=$((rank + 1))
    if [ "${rank}" -le 3 ]; then
        log "keep dump ${dump}"
    else
        log "REMOVE ${dump}"
        printf 'hub|%s\n' "${dump}" >>"${REMOVE_LIST}"
    fi
done

if [ ! -s "${REMOVE_LIST}" ]; then
    log "nothing to remove"
    exit 0
fi
if [ "${APPLY}" = "0" ]; then
    log "plan only; re-run with --apply to delete after review"
    exit 0
fi

# Код 6 — релиз зарезервирован идущей операцией (cutover/rollback-db): это не
# ошибка, а «не сейчас»; пропускаем его и идём дальше. Остальные коды — громкий отказ.
SKIPPED_RESERVED=0
remove_refused() {
    if [ "$1" = "6" ]; then
        log "skip ${3}: reserved by a running operation"
        SKIPPED_RESERVED=$((SKIPPED_RESERVED + 1))
        return 0
    fi
    die "guard refused $2 removal: $3"
}

log "applying removals, each guarded by literal-path checks"
while IFS='|' read -r hostkind path; do
    case "${path}" in
        "${HUB_RELEASES}"/????????????|"${WEB_RELEASES}"/????????????|"${WORKER_RELEASES}"/????????????|"${HUB_BACKUP_DIR}"/canterlot-pre-*.dump) ;;
        "${DIST_BASE:-/nonexistent}"/????????????) ;;
        *) die "refusing odd path: ${path}" ;;
    esac
    name="${path##*/}"
    [ -n "${name}" ] || die "refusing empty release name for: ${path}"
    case "${path}" in
        "${HUB_BACKUP_DIR}"/canterlot-pre-*.dump) ;;
        *) is_release_id "${name}" || die "refusing odd path: ${path}" ;;
    esac
    case "${hostkind}" in
        hub) GUARD_LIVE="${HUB_EXE_RELEASE} ${HUB_CURRENT_TARGET}" ;;
        web) GUARD_LIVE="${WEB_BUILD_TARGET} ${CADDY_LIVE_RELEASE}" ;;
        local) GUARD_LIVE="${WORKER_LIVE} ${WORKER_CURRENT_TARGET}${KEEP_IDS}" ;;
        localdist) GUARD_LIVE="${KEEP_IDS}" ;;
        *) die "refusing unknown host kind: ${hostkind}" ;;
    esac
    case " ${GUARD_LIVE} " in
        *" ${name} "*) die "guard: ${path} became live or current, aborting" ;;
    esac
    case "${hostkind}" in
        hub)
            # Финальная сверка current и удаление — одна удалённая команда под
            # одним flock: flip между ними невозможен (все flip'ы берут тот же lock).
            run_write "remove hub release" hub_locked '
                current="$1"; path="$2"; name="$3"; base="$4"
                [ ! -e "${base}/.reserved.${name}" ] || { echo "guard: ${path} is reserved by a running operation" >&2; exit 6; }
                readlink -m "${current}" >/dev/null 2>&1 || { echo "guard: cannot read hub current" >&2; exit 4; }
                cur="$(readlink "${current}")" || { echo "guard: cannot read hub current" >&2; exit 4; }
                cur="${cur##*/}"
                [ "${cur}" != "${name}" ] || { echo "guard: ${path} is hub current now" >&2; exit 5; }
                for e in /proc/[0-9]*/exe; do readlink "${e}" 2>/dev/null; done | grep -q "^${path}/" \
                    && { echo "guard: process runs from ${path}" >&2; exit 3; }
                rm -rf "${path}"
            ' "${HUB_CURRENT}" "${path}" "${name}" "${HUB_BASE}" \
                || remove_refused "$?" hub "${path}" ;;
        web)
            # Caddy-version, build и удаление — под тем же lock, что и compose up:
            # recreate между проверками не проскочит.
            run_write "remove web release" web_locked '
                container="$1"; version_json="$2"; build="$3"; path="$4"; name="$5"; base="$6"
                [ ! -e "${base}/.reserved.${name}" ] || { echo "guard: ${path} is reserved by a running operation" >&2; exit 6; }
                now="$(docker exec "${container}" cat "${version_json}")" \
                    || { echo "guard: cannot read caddy version.json" >&2; exit 4; }
                ver="$(printf "%s" "${now}" | sed -n "s/.*\"version\"[ ]*:[ ]*\"\([0-9a-zA-Z]\+\)\".*/\1/p")"
                [ -n "${ver}" ] || { echo "guard: cannot parse caddy version.json" >&2; exit 4; }
                if [ "${#name}" -lt "${#ver}" ]; then short="${name}"; long="${ver}"; else short="${ver}"; long="${name}"; fi
                if [ "${#short}" -ge 7 ]; then
                    case "${long}" in "${short}"*) echo "guard: ${path} is served by caddy now" >&2; exit 5 ;; esac
                fi
                cur="$(readlink "${build}")" || { echo "guard: cannot read web build" >&2; exit 4; }
                cur="${cur##*/}"
                [ "${cur}" != "${name}" ] || { echo "guard: ${path} is web build now" >&2; exit 2; }
                rm -rf "${path}"
            ' "${WEB_CONTAINER}" "${WEB_CONTAINER_VERSION_JSON}" "${WEB_BUILD_LINK}" "${path}" "${name}" "${WEB_SITES_BASE}" \
                || remove_refused "$?" web "${path}" ;;
        localdist)
            # Буквальный абсолютный путь под DIST_BASE, не переданный в --keep.
            case "${path}" in "${DIST_BASE}"/*) ;; *) die "guard: ${path} is outside ${DIST_BASE}" ;; esac
            [ "${path##*/}" = "${name}" ] || die "guard: path/name mismatch for ${path}"
            case " ${KEEP_IDS} " in
                *" ${name} "*) die "guard: ${path} is a kept release id" ;;
            esac
            run_write "remove local dist" rm -rf "${path}"
            ;;
        local)
            run_write "remove worker release" worker_locked '
                current="$1"; base="$2"; path="$3"; name="$4"
                [ ! -e "${base}/.reserved.${name}" ] || { echo "guard: ${path} is reserved by a running operation" >&2; exit 6; }
                for e in /proc/[0-9]*/exe; do
                    case "$(readlink "${e}" 2>/dev/null || true)" in "${path}"/*) echo "guard: process runs from ${path}" >&2; exit 3 ;; esac
                done
                target="$(readlink "${current}")" || { echo "guard: cannot read worker current" >&2; exit 4; }
                [ "${target}" != "releases/${name}" ] || { echo "guard: ${path} is worker current" >&2; exit 5; }
                [ "$(readlink -m "${base}/${target}")" != "${base}/releases/${name}" ] \
                    || { echo "guard: ${path} is worker current" >&2; exit 5; }
                rm -rf "${path}"
            ' "${WORKER_CURRENT}" "${WORKER_BASE}" "${path}" "${name}" \
                || remove_refused "$?" worker "${path}" ;;
    esac
    [ "${SKIPPED_RESERVED}" = "0" ] || [ ! -e "${path}" ] || continue
    log "removed ${path}"
done <"${REMOVE_LIST}"
log "retain done"
