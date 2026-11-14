#!/usr/bin/env bash
# При sourcing ничего не исполнять: скрипт запускает боевой выкат только как
# программа (урок 1.16.1: source cutover-скрипта остановил воркер).
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Проверка ЦЕЛЕВОГО релиза, только чтение: то, чего не видит ни одна проверка
# развёрнутого релиза. Ничего не пишет и не меняет ни на одном хосте.
#   1. dist цели: RELEASE (sha, migrations_last), SHA256SUMS сходится, csp.txt
#      (ровно один wss-origin и хотя бы один хэш скрипта), web/_app/version.json равен
#      SHA, состав корня web выведен из dist (а не из зашитого списка);
#   2. залитые каталоги релиза на каждом хосте (хаб, веб, воркер): полный пофайловый
#      состав и хэши против dist, точный состав корня, RELEASE, версии; если релиз
#      ещё не залит — "NOT RUN: ... not staged" (провал, пока не разрешено явно);
#   3. миграции: migrations_last целевого RELEASE против базы (база не должна быть
#      впереди цели; сколько миграций ждёт применения);
#   4. CSP целевого dist против CSP запущенного контейнера (отличие — не ошибка, а
#      заметка: хэш скрипта меняется между релизами).
# Использование: scripts/deploy/target-check.sh SHA --source DIR [--dist DIR] [--allow-skipped]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

SHA=""
SOURCE_DIR=""
DIST_BASE=""
ALLOW_SKIPPED=0
while (($#)); do
    case "$1" in
        --source) SOURCE_DIR="$2"; shift 2 ;;
        --dist) DIST_BASE="$2"; shift 2 ;;
        --allow-skipped) ALLOW_SKIPPED=1; shift ;;
        -*) die "unknown flag: $1" ;;
        *) SHA="$(short_sha "$1")"; shift ;;
    esac
done
[ -n "${SHA}" ] || die "usage: target-check.sh SHA --source DIR [--dist DIR] [--allow-skipped] (the target release is never guessed)"
require_release_id "${SHA}"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SOURCE_DIR="${SOURCE_DIR:-${REPO_ROOT}}"
DIST="${DIST_BASE:-${REPO_ROOT}/dist}/${SHA}"
GLIBC_MAX="${GLIBC_MAX:-2.41}"

hub_psql() { ssh_hub "runuser -u canterlot -- psql -h '${HUB_PG_SOCKET}' -d '${HUB_PG_DB}' -tAc \"$1\"" || die "hub DB query failed"; }

# Бинарь dist пришёл из контейнерной сборки scripts/build-release.sh; проверяем
# ровно то, что проверял дежурный лог: --version, отсутствие libssl, максимальную
# версию символа GLIBC (прод — Debian 13, glibc 2.41) и openapi.
is_elf() { [ "$(head -c 4 "$1" 2>/dev/null | od -An -tx1 | tr -d ' \n')" = "7f454c46" ]; }
binary_version() {
    local bin="$1" out
    out="$("${bin}" --version 2>/dev/null | sed -n 1p || true)"
    [ -n "${out}" ] || return 1
    printf '%s' "${out}"
}
max_glibc() {
    local bin="$1" out
    out="$(objdump -T "${bin}" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sed 's/^GLIBC_//' | sort -V | tail -1 || true)"
    [ -n "${out}" ] || return 1
    printf '%s' "${out}"
}
glibc_le() {
    local have="$1" want="$2"
    [ "$(printf '%s\n%s\n' "${have}" "${want}" | sort -V | tail -1)" = "${want}" ]
}

NOT_RUN_LIST=""
not_run() {
    NOT_RUN_LIST+="${1}"$'\n'
    log "NOT RUN: $1"
}

log "target check for ${SHA} (read-only; dist ${DIST})"

# --- 1. dist цели ---
DIST_OK=1
if [ ! -d "${DIST}" ]; then
    hard "no dist for the target release: ${DIST} (run scripts/build-release.sh vX.Y.Z)"
    DIST_OK=0
fi
TARGET_MIGR=""
if [ "${DIST_OK}" = "1" ]; then
    REL_SHA="$(grep '^sha=' "${DIST}/RELEASE" 2>/dev/null | cut -d= -f2 || true)"
    if [ -z "${REL_SHA}" ]; then
        hard "dist RELEASE has no sha: ${DIST}/RELEASE"
    elif ! sha_match "${REL_SHA}" "${SHA}"; then
        hard "dist RELEASE sha ${REL_SHA} is not the target ${SHA}"
    else
        log "target dist RELEASE sha=${REL_SHA}"
    fi
    TARGET_MIGR_NAME="$(grep '^migrations_last=' "${DIST}/RELEASE" 2>/dev/null | cut -d= -f2 || true)"
    TARGET_MIGR="$(printf '%s' "${TARGET_MIGR_NAME}" | grep -oE '^[0-9]+' | sed 's/^0*//' || true)"
    [ -n "${TARGET_MIGR}" ] || hard "bad migrations_last in ${DIST}/RELEASE"
fi
if [ "${DIST_OK}" = "1" ]; then
    if [ ! -s "${DIST}/SHA256SUMS" ]; then
        hard "target dist has no SHA256SUMS"
        DIST_OK=0
    elif ! ( cd "${DIST}" && sha256sum -c SHA256SUMS >/dev/null 2>&1 ); then
        hard "target dist SHA256SUMS does not verify"
        DIST_OK=0
    else
        log "target dist SHA256SUMS verifies ($(wc -l <"${DIST}/SHA256SUMS") files)"
    fi
    # source= — полный коммит сборки; он обязан начинаться с целевого sha.
    SOURCE_SHA="$(grep '^source=' "${DIST}/RELEASE" 2>/dev/null | cut -d= -f2 || true)"
    if [[ "${SOURCE_SHA}" =~ ^[0-9a-f]{40}$ ]] && [ "$(short_sha "${SOURCE_SHA}")" = "${SHA}" ]; then
        log "target dist RELEASE source=${SOURCE_SHA:0:12} matches the release id"
    else
        hard "target dist RELEASE source is '${SOURCE_SHA:-missing}', expected a 40-hex commit starting with ${SHA}"
    fi
    # EXPECT: что релиз ждёт от воркера (возможности, харнессы). Файл входит в
    # SHA256SUMS и обязан совпадать с выводом `capabilities` боевого воркера dist.
    if [ -f "${DIST}/EXPECT" ]; then
        if ! CAPS_OUT="$(timeout 10 "${DIST}/worker/bin/canterlot-worker" capabilities 2>/dev/null)"; then
            hard "target dist worker cannot print its capabilities (canterlot-worker capabilities)"
        else
            EXPECT_FROM_BIN="$(printf 'format=1\ncapabilities=%s\nharnesses=%s\n' \
                "$(printf '%s\n' "${CAPS_OUT}" | sed -n 's/^capability //p' | tr '\n' ' ' | sed 's/ $//')" \
                "$(printf '%s\n' "${CAPS_OUT}" | sed -n 's/^harness //p' | tr '\n' ' ' | sed 's/ $//')")"
            if [ "${EXPECT_FROM_BIN}" = "$(cat "${DIST}/EXPECT")" ]; then
                log "target dist EXPECT equals the worker's own capabilities: $(sed -n 's/^capabilities=//p' "${DIST}/EXPECT" | wc -w) capabilities, harnesses $(sed -n 's/^harnesses=//p' "${DIST}/EXPECT")"
            else
                hard "target dist EXPECT differs from 'canterlot-worker capabilities' of the dist worker"
            fi
        fi
    else
        log "target dist has no EXPECT (release built before it existed): verify will fall back to the built-in capability list"
    fi
    # --version, libssl, glibc, openapi — на самих бинарях dist.
    HUB_VERSION=""; WORKER_VERSION=""
    for pair in hub:canterlot-hub worker:canterlot-worker synthetic:canterlot-worker; do
        comp="${pair%%:*}"; bin_name="${pair##*:}"
        bin="${DIST}/${comp}/bin/${bin_name}"
        [ -f "${bin}" ] || { hard "target dist lacks ${comp}/bin/${bin_name}"; continue; }
        [ -x "${bin}" ] || { hard "target dist ${comp}/bin/${bin_name} is not executable"; continue; }
        if ! VER="$(binary_version "${bin}")"; then
            hard "target dist ${comp}/bin/${bin_name} --version failed or printed nothing"
        else
            log "target dist ${comp} --version: ${VER}"
            case "${comp}" in
                hub) HUB_VERSION="${VER}" ;;
                worker) WORKER_VERSION="${VER}" ;;
            esac
        fi
        # Скриптовые/тестовые артефакты (не ELF) не линкуются: для них ldd/glibc
        # неприменимы, и это отмечается, а не выдаётся за проверку.
        if is_elf "${bin}"; then
            if ldd "${bin}" 2>/dev/null | grep -c libssl >/dev/null; then
                hard "target dist ${comp}/bin/${bin_name} links libssl"
            else
                log "target dist ${comp}: no libssl in ldd"
            fi
            if ! MAXG="$(max_glibc "${bin}")"; then
                hard "target dist ${comp}/bin/${bin_name}: cannot determine the maximum GLIBC symbol"
            elif glibc_le "${MAXG}" "${GLIBC_MAX}"; then
                log "target dist ${comp}: max GLIBC ${MAXG} <= ${GLIBC_MAX}"
            else
                hard "target dist ${comp}: max GLIBC ${MAXG} > target ${GLIBC_MAX}"
            fi
        else
            log "target dist ${comp}: not an ELF binary (ldd/GLIBC not applicable)"
        fi
    done
    if [ -n "${HUB_VERSION}" ] && [ -n "${WORKER_VERSION}" ]; then
        hub_v="$(printf '%s' "${HUB_VERSION}" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | sed -n 1p || true)"
        worker_v="$(printf '%s' "${WORKER_VERSION}" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | sed -n 1p || true)"
        if [ -n "${hub_v}" ] && [ "${hub_v}" = "${worker_v}" ]; then
            log "target dist hub and worker versions agree (${hub_v})"
        else
            hard "target dist hub version '${HUB_VERSION}' and worker version '${WORKER_VERSION}' disagree"
        fi
    fi
    if [ ! -f "${SOURCE_DIR}/docs/api/openapi.json" ]; then
        hard "cannot check openapi: ${SOURCE_DIR}/docs/api/openapi.json is missing"
    elif cmp -s <("${DIST}/hub/bin/canterlot-hub" openapi 2>/dev/null) "${SOURCE_DIR}/docs/api/openapi.json"; then
        log "target dist openapi equals ${SOURCE_DIR}/docs/api/openapi.json"
    else
        hard "target dist openapi differs from ${SOURCE_DIR}/docs/api/openapi.json"
    fi
    CSP="$(cat "${DIST}/csp.txt" 2>/dev/null || true)"
    if [ -z "${CSP}" ]; then
        hard "target dist csp.txt is missing or empty"
    else
        WSS_N="$(grep -c "${WSS_ORIGIN}" "${DIST}/csp.txt" || true)"
        HASH_N="$(grep -o "'sha256-[^']*'" "${DIST}/csp.txt" | wc -l || true)"
        [ "${WSS_N}" = "1" ] || hard "target csp.txt lacks exactly one ${WSS_ORIGIN}: got ${WSS_N}"
        [ "${HASH_N}" -ge 1 ] || hard "target csp.txt has no script hash"
        log "target CSP: one ${WSS_ORIGIN}, ${HASH_N} script hash(es)"
        if CSP_BUILD_OUT="$(check_csp_matches_build)"; then
            log "target CSP vs final build: ${CSP_BUILD_OUT}"
        else
            hard "target CSP is not derived from the dist build: ${CSP_BUILD_OUT}"
        fi
    fi
    if EXPECTED_TOP="$(web_top_entries "${DIST}/web")"; then
        log "target web top entries (from the dist): ${EXPECTED_TOP}"
    else
        hard "target dist web is not a build root"
        EXPECTED_TOP=""
        DIST_OK=0
    fi
    [ "$(cat "${DIST}/web/_app/version.json" 2>/dev/null)" = "{\"version\":\"${SHA}\"}" ] \
        || { hard "target dist web/_app/version.json is not ${SHA}"; }
    log "target dist version.json is ${SHA}"
fi

# --- 2. залитые каталоги на хостах ---
if [ "${DIST_OK}" = "1" ]; then
    # Состав каталога: первое несовпадение — жёсткая ошибка, подробность — в журнал.
    check_tree() { # $1 метка, $2 host (ssh-алиас) либо local, $3 каталог, $4 ожидаемые хэши
        local label="$1" host="$2" dir="$3" expected="$4" actual
        if [ "${host}" = "local" ]; then
            actual="$(tree_sums_local "${dir}")" || { hard "cannot hash ${label} at ${dir}"; return; }
        else
            actual="$(tree_sums_remote "${host}" "${dir}")" || { hard "cannot hash ${label} at ${dir}"; return; }
        fi
        if [ "${actual}" = "${expected}" ]; then
            log "${label}: staged tree equals the dist (every file hash)"
        else
            hard "${label}: staged tree differs from the dist"
        fi
    }

    # хаб
    if ssh_hub "test -d $(shq "${HUB_RELEASES}/${SHA}")"; then
        TOP="$(ssh_hub "ls -A $(shq "${HUB_RELEASES}/${SHA}")" | LC_ALL=C sort | tr '\n' ' ')"
        [ "${TOP}" = "$(printf 'bin\nRELEASE\nSHA256SUMS\n' | LC_ALL=C sort | tr '\n' ' ')" ] \
            || hard "hub ${SHA} staged top entries are wrong: ${TOP}"
        ssh_hub "cat $(shq "${HUB_RELEASES}/${SHA}/RELEASE")" | cmp -s - "${DIST}/RELEASE" \
            || hard "hub ${SHA} staged RELEASE differs from the dist"
        ssh_hub "test -x $(shq "${HUB_RELEASES}/${SHA}/bin/canterlot-hub")" \
            || hard "hub ${SHA} staged binary is not executable"
        check_tree "hub ${SHA}" "${HUB_SSH}" "${HUB_RELEASES}/${SHA}" "$(hub_expected_sums)"
        HUB_STAGED_SHA="$(ssh_hub "grep '^sha=' $(shq "${HUB_RELEASES}/${SHA}/RELEASE")" | cut -d= -f2 || true)"
        if sha_match "${HUB_STAGED_SHA}" "${SHA}"; then
            log "hub staged RELEASE sha=${HUB_STAGED_SHA}"
        else
            hard "hub staged RELEASE sha is '${HUB_STAGED_SHA}', not ${SHA}"
        fi
    else
        not_run "hub ${SHA} is not staged on ${HUB_SSH} (${HUB_RELEASES}/${SHA}): run stage.sh ${SHA}"
    fi

    # веб: версия читается из залитого каталога на диске хоста (это stagedir, а не раздаваемый
    # build); версия РАЗДАВАЕМОГО PWA читается только внутри контейнера (container_pwa_sha).
    if ssh_web "test -d $(shq "${WEB_RELEASES}/${SHA}")"; then
        TOP="$(ssh_web "ls -A $(shq "${WEB_RELEASES}/${SHA}")" | LC_ALL=C sort | tr '\n' ' ')"
        [ "${TOP}" = "${EXPECTED_TOP}" ] || hard "web ${SHA} staged top entries differ from the dist: staged [${TOP}] dist [${EXPECTED_TOP}]"
        [ "$(ssh_web "cat $(shq "${WEB_RELEASES}/${SHA}/_app/version.json")")" = "{\"version\":\"${SHA}\"}" ] \
            || hard "web ${SHA} staged _app/version.json is not ${SHA}"
        # Залитый web-каталог до flip равен dist; после ручного слияния старых immutable-чанков
        # (cp -rn, чтобы открытые вкладки не теряли ассеты) он строго шире: каждый файл dist на
        # месте с тем же хэшем, лишние — только под _app/immutable/. Это допустимо и помечается.
        WEB_ACTUAL="$(tree_sums_remote "${WEB_SSH}" "${WEB_RELEASES}/${SHA}")" || WEB_ACTUAL=""
        WEB_EXPECTED="$(web_expected_sums)"
        if [ -z "${WEB_ACTUAL}" ]; then
            hard "cannot hash web ${SHA} at ${WEB_RELEASES}/${SHA}"
        elif [ "${WEB_ACTUAL}" = "${WEB_EXPECTED}" ]; then
            log "web ${SHA}: staged tree equals the dist (every file hash)"
        else
            WEB_MISSING="$(comm -13 <(LC_ALL=C sort <<<"${WEB_ACTUAL}") <(LC_ALL=C sort <<<"${WEB_EXPECTED}") | wc -l)"
            WEB_EXTRA_OUT="$(comm -23 <(LC_ALL=C sort <<<"${WEB_ACTUAL}") <(LC_ALL=C sort <<<"${WEB_EXPECTED}") | sed 's/^[0-9a-f]*  //')"
            WEB_EXTRA_BAD="$(printf '%s\n' "${WEB_EXTRA_OUT}" | grep -vc '^\./_app/immutable/' || true)"
            if [ "${WEB_MISSING}" = "0" ] && [ "${WEB_EXTRA_BAD}" = "0" ]; then
                log "web ${SHA}: every dist file present with the same hash, plus $(printf '%s\n' "${WEB_EXTRA_OUT}" | grep -c .) older immutable chunk(s) merged in (expected after the cp -rn merge)"
            else
                hard "web ${SHA}: staged tree differs from the dist (${WEB_MISSING} dist file(s) missing or changed, ${WEB_EXTRA_BAD} extra file(s) outside _app/immutable)"
            fi
        fi
    else
        not_run "web ${SHA} is not staged on ${WEB_SSH} (${WEB_RELEASES}/${SHA}): run stage.sh ${SHA}"
    fi

    # воркер (локально)
    if [ -d "${WORKER_RELEASES}/${SHA}" ]; then
        [ -x "${WORKER_RELEASES}/${SHA}/bin/canterlot-worker" ] || hard "worker ${SHA} staged binary is not executable"
        check_tree "worker ${SHA}" local "${WORKER_RELEASES}/${SHA}" "$(worker_expected_sums)"
        WORKER_STAGED_SHA="$(grep '^sha=' "${WORKER_RELEASES}/${SHA}/RELEASE" 2>/dev/null | cut -d= -f2 || true)"
        if sha_match "${WORKER_STAGED_SHA}" "${SHA}"; then
            log "worker staged RELEASE sha=${WORKER_STAGED_SHA}"
        else
            hard "worker staged RELEASE sha is '${WORKER_STAGED_SHA}', not ${SHA}"
        fi
    else
        not_run "worker ${SHA} is not staged (${WORKER_RELEASES}/${SHA}): run stage.sh ${SHA}"
    fi
fi

# --- 3. миграции цели против базы ---
if [ -n "${TARGET_MIGR}" ]; then
    DB_MAX="$(hub_psql "select coalesce(max(version),0) from _sqlx_migrations where success")"
    DB_FAILED="$(hub_psql "select count(*) from _sqlx_migrations where not success")"
    [ "${DB_FAILED}" = "0" ] || hard "failed migrations present in the database: ${DB_FAILED}"
    if [ "${DB_MAX}" -gt "${TARGET_MIGR}" ] 2>/dev/null; then
        hard "database is ahead of the target release: migrations max ${DB_MAX} > target migrations_last ${TARGET_MIGR}"
    elif [ "${DB_MAX}" = "${TARGET_MIGR}" ]; then
        log "migrations: database at ${DB_MAX} equals the target migrations_last (${TARGET_MIGR_NAME}): none to apply"
    else
        log "migrations: database at ${DB_MAX}, target ends at ${TARGET_MIGR} (${TARGET_MIGR_NAME}): $((TARGET_MIGR - DB_MAX)) to apply while the hub restarts"
    fi
fi

# --- 4. CSP цели против запущенного контейнера (заметка, не ошибка) ---
if [ "${DIST_OK}" = "1" ] && [ -n "${CSP:-}" ]; then
    if RUNNING_CSP="$(ssh_web "docker exec caddy printenv CANTERLOT_CSP" 2>/dev/null)"; then
        if [ "${RUNNING_CSP}" = "${CSP}" ]; then
            log "target CSP equals the running container CSP"
        else
            log "target CSP differs from the running container CSP (expected when the script hash changes): the cutover writes canterlot.env.${SHA} and recreates the container"
        fi
    else
        not_run "running container CSP is not readable, target CSP not compared with it"
    fi
fi

if [ -n "${NOT_RUN_LIST}" ]; then
    if [ "${ALLOW_SKIPPED}" = "1" ]; then
        log "target comparisons NOT RUN were explicitly allowed (--allow-skipped)"
    else
        hard "target comparisons NOT RUN (stage the release or use --allow-skipped only to acknowledge a known gap)"
    fi
fi
summary_exit "target check PASS for ${SHA}"
