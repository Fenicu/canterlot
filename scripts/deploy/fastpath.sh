#!/usr/bin/env bash
# При sourcing ничего не исполнять.
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Решение о быстром пути выпуска без миграций (читает развёрнутые релизы хаба, веба и
# воркера и git diff между развёрнутым и целью; ничего не пишет). Использование:
#   scripts/deploy/fastpath.sh SHA --source DIR
# Печатает ключ=значение: path, rehearsal, vzdump, reasons, live, target (см. fastpath_eval
# в lib.sh). Любая неясность — path=full. Код выхода всегда 0, кроме неверных аргументов:
# даже если развёрнутый релиз не прочитался, ответ — full с причиной.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

SHA=""
SOURCE_DIR=""
while (($#)); do
    case "$1" in
        --source) SOURCE_DIR="$2"; shift 2 ;;
        -*) die "unknown flag: $1" ;;
        *) SHA="$(short_sha "$1")"; shift ;;
    esac
done
[ -n "${SHA}" ] || die "usage: fastpath.sh SHA --source DIR"
require_release_id "${SHA}"
SOURCE_DIR="${SOURCE_DIR:-$(cd "${SCRIPT_DIR}/../.." && pwd)}"

# deployed_sha при сбое умирает громко (die → exit 1): читаем в подоболочках и
# превращаем сбой в пустое значение, которое fastpath_eval читает как «не определён».
read_live() { (deployed_sha "$1") 2>/dev/null || printf ''; }
LIVE_HUB="$(read_live hub)"
LIVE_WEB="$(read_live web)"
LIVE_WORKER="$(read_live worker)"
printf 'live=%s\ntarget=%s\n' "${LIVE_HUB}" "${SHA}"
fastpath_eval "${SOURCE_DIR}" "${SHA}" "${LIVE_HUB}" "${LIVE_WEB}" "${LIVE_WORKER}"
