#!/usr/bin/env bash
# При sourcing ничего не исполнять.
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Решение о быстром пути выпуска без миграций. Читает развёрнутые релизы хаба, веба и
# воркера, проверяет, что ссылки ведут в существующие релизы, работающие процессы
# запущены из них, а раздаваемый PWA — релиз веба; берёт полные хэши коммитов из
# RELEASE развёрнутого релиза и из RELEASE dist цели и классифицирует git diff между
# ними. Ничего не пишет. Использование:
#   scripts/deploy/fastpath.sh SHA --source DIR [--dist DIR]
# Печатает ключ=значение: live, target, path, rehearsal, vzdump, reasons, vzdump_reason
# (см. fastpath_eval в lib.sh). Любая неясность — path=full с причиной; код выхода
# всегда 0, кроме неверных аргументов. Окончательное решение о vzdump принимает
# cutover.sh по манифесту миграций хаба и живой базе, а не этот скрипт.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

SHA=""
SOURCE_DIR=""
DIST_BASE=""
while (($#)); do
    case "$1" in
        --source) SOURCE_DIR="$2"; shift 2 ;;
        --dist) DIST_BASE="$2"; shift 2 ;;
        -*) die "unknown flag: $1" ;;
        *) SHA="$(short_sha "$1")"; shift ;;
    esac
done
[ -n "${SHA}" ] || die "usage: fastpath.sh SHA --source DIR [--dist DIR]"
require_release_id "${SHA}"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SOURCE_DIR="${SOURCE_DIR:-${REPO_ROOT}}"
DIST="${DIST_BASE:-${REPO_ROOT}/dist}/${SHA}"

# deployed_sha при сбое умирает громко (die → exit 1): читаем в подоболочках и
# превращаем сбой в пустое значение, которое fastpath_eval читает как «не определён».
read_live() { (deployed_sha "$1") 2>/dev/null || printf ''; }
print_full() {
    printf 'path=full\nrehearsal=run\nvzdump=run\nreasons=%s\nvzdump_reason=\n' "$(printf '%s' "$1" | tr '\n\r=' '  :')"
}

LIVE_HUB="$(read_live hub)"
LIVE_WEB="$(read_live web)"
LIVE_WORKER="$(read_live worker)"
printf 'live=%s\ntarget=%s\n' "${LIVE_HUB}" "${SHA}"

LIVE_FULL=""
TARGET_FULL=""
if [[ "${LIVE_HUB}" =~ ^[0-9a-f]{12}$ ]] && [ "${LIVE_HUB}" = "${LIVE_WEB}" ] && [ "${LIVE_HUB}" = "${LIVE_WORKER}" ]; then
    if ! BASELINE_WHY="$(baseline_verify "${LIVE_HUB}" "${LIVE_WEB}" "${LIVE_WORKER}" 2>&1)"; then
        print_full "the live baseline is inconsistent: ${BASELINE_WHY}"
        exit 0
    fi
    LIVE_FULL="$(deployed_source_full "${LIVE_HUB}")" || LIVE_FULL=""
    if [ -z "${LIVE_FULL}" ]; then
        print_full "the live release ${LIVE_HUB} has no usable source= (a full commit id matching its name) in its RELEASE"
        exit 0
    fi
fi
if [ -f "${DIST}/RELEASE" ]; then
    TARGET_FULL="$(sed -n 's/^source=\([0-9a-f]\{40\}\)$/\1/p' "${DIST}/RELEASE" | sed -n 1p)" || TARGET_FULL=""
fi
if [ -n "${LIVE_FULL}" ] && { [ -z "${TARGET_FULL}" ] || [ "${TARGET_FULL:0:12}" != "${SHA}" ]; }; then
    print_full "the target dist RELEASE has no full source= commit id matching ${SHA}"
    exit 0
fi
fastpath_eval "${SOURCE_DIR}" "${TARGET_FULL}" "${LIVE_FULL}" "${LIVE_HUB}" "${LIVE_WEB}" "${LIVE_WORKER}"
