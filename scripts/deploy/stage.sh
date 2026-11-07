#!/usr/bin/env bash
# При sourcing ничего не исполнять: скрипт запускает боевой выкат только как
# программа (урок 1.16.1: source cutover-скрипта остановил воркер).
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi
# Копирование релиза на хосты: только tar-конвейеры, никогда rsync (урок: план говорил rsync).
# Проверяет хэши, утверждает точный состав корней stagedir и двигает их в releases/<sha>
# атомарным mv. Никогда не трогает current/build.
# Использование: scripts/deploy/stage.sh SHA [--dist DIR]
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
[ -n "${SHA}" ] || die "usage: stage.sh SHA --source DIR [--dist DIR]"
require_release_id "${SHA}"
DIST="${DIST_BASE:-$(cd "${SCRIPT_DIR}/../.." && pwd)/dist}/${SHA}"
[ -d "${DIST}" ] || die "no dist dir: ${DIST}"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
# openapi сверяется с исходником ТОГО чекаута, из которого собран dist (--source),
# а не с тем, где лежит сам toolkit: у инструмента и релиза разные коммиты.
SOURCE_DIR="${SOURCE_DIR:-${REPO_ROOT}}"
[ -f "${SOURCE_DIR}/docs/api/openapi.json" ] \
    || die "no ${SOURCE_DIR}/docs/api/openapi.json: pass --source with the release checkout"
verify_remote_tree() {
    local host="$1" dir="$2" expected="$3" label="$4" actual
    if ! actual="$(tree_sums_remote "${host}" "${dir}")"; then
        die "cannot hash ${label} at ${dir}"
    fi
    [ "${actual}" = "${expected}" ] || die "${label} staged tree differs from dist"
}
# Precondition: dist itself matches its SHA256SUMS and RELEASE. Без этого
# повторный прогон мог бы «подтвердить» уже испорченный stagedir.
verify_dist_manifest() {
    [ -s "${DIST}/SHA256SUMS" ] || die "dist ${SHA} has no SHA256SUMS"
    ( cd "${DIST}" && sha256sum -c SHA256SUMS >/dev/null ) || die "dist ${SHA} SHA256SUMS does not verify"
    local rel_sha
    rel_sha="$(grep '^sha=' "${DIST}/RELEASE" | cut -d= -f2)"
    [ -n "${rel_sha}" ] || die "dist ${SHA} RELEASE has no sha"
    sha_match "${rel_sha}" "${SHA}" || die "dist RELEASE sha ${rel_sha} is not ${SHA}"
}
# Не оставлять мусор в /tmp хостов после неудачи.
cleanup_stage_tmp() {
    ssh_hub "rm -rf '/tmp/stage-${SHA}'" || true
    ssh_web "rm -rf '/tmp/webstage-${SHA}'" || true
    rm -rf "${WORKER_BASE}/.stage-${SHA}" || true
    rm -f "/tmp/web-dist-${SHA}.sums" "/tmp/web-staged-${SHA}.sums"
}
trap cleanup_stage_tmp EXIT
verify_dist_manifest

stage_hub() {
    log "staging hub ${SHA}"
    if ssh_hub "test -d '${HUB_RELEASES}/${SHA}'"; then
        # Повтор после частичного прогона: пропускаем, только если совпадает
        # полный пофайловый состав и RELEASE/SHA256SUMS, а не только бинарь.
        TOP="$(ssh_hub "ls -A '${HUB_RELEASES}/${SHA}'" | sort | tr '\n' ' ')"
        HUB_EXPECTED="$(printf 'bin\nRELEASE\nSHA256SUMS\n' | LC_ALL=C sort | tr '\n' ' ')"
        [ "${TOP}" = "${HUB_EXPECTED}" ] || die "hub ${SHA} already staged but differs from dist, wrong top entries: ${TOP}"
        ssh_hub "cat '${HUB_RELEASES}/${SHA}/RELEASE'" | cmp -s - "${DIST}/RELEASE" \
            || die "hub ${SHA} already staged but RELEASE differs, remove manually"
        ssh_hub "cat '${HUB_RELEASES}/${SHA}/SHA256SUMS'" | cmp -s - "${DIST}/SHA256SUMS" \
            || die "hub ${SHA} already staged but SHA256SUMS differs, remove manually"
        verify_remote_tree "${HUB_SSH}" "${HUB_RELEASES}/${SHA}" "$(hub_expected_sums)" "hub ${SHA}"
        log "hub ${SHA} already staged identically, skipping"
        return 0
    fi
    # Явный конвейер вместо rsync (урок: план говорил rsync).
    tar -C "${DIST}/hub" -cf - bin | recv_to "${HUB_SSH}" "rm -rf '/tmp/stage-${SHA}' && mkdir -p '/tmp/stage-${SHA}' && tar -C '/tmp/stage-${SHA}' -xf -"
    tar -C "${DIST}" -cf - RELEASE SHA256SUMS | recv_to "${HUB_SSH}" "tar -C '/tmp/stage-${SHA}' -xf -"
    # Полный состав stagedir проверяется ДО публикации, а не только бинарь.
    verify_remote_tree "${HUB_SSH}" "/tmp/stage-${SHA}" "$(hub_expected_sums)" "hub ${SHA}"
    # Хэш staged binary равен dist; staged binary запускается на CT и отдаёт openapi из репо.
    LOCAL_HASH="$(sha256sum "${DIST}/hub/bin/canterlot-hub" | cut -d' ' -f1)"
    STAGED_HASH="$(ssh_hub "sha256sum '/tmp/stage-${SHA}/bin/canterlot-hub'" | cut -d' ' -f1)"
    [ "${LOCAL_HASH}" = "${STAGED_HASH}" ] || die "hub staged binary hash mismatch"
    STAGED_OPENAPI="$(ssh_hub "'/tmp/stage-${SHA}/bin/canterlot-hub' openapi" | sha256sum | cut -d' ' -f1)"
    REPO_OPENAPI="$(sha256sum "${SOURCE_DIR}/docs/api/openapi.json" | cut -d' ' -f1)"
    [ "${STAGED_OPENAPI}" = "${REPO_OPENAPI}" ] || die "staged hub openapi differs from ${SOURCE_DIR}/docs/api/openapi.json"
    ssh_hub "cd '${HUB_RELEASES}' && mv -T '/tmp/stage-${SHA}' '${SHA}'"
    # Точный состав корня: bin, RELEASE, SHA256SUMS — и ничего лишнего.
    TOP="$(ssh_hub "ls -A '${HUB_RELEASES}/${SHA}'" | sort | tr '\n' ' ')"
    HUB_EXPECTED="$(printf 'bin\nRELEASE\nSHA256SUMS\n' | LC_ALL=C sort | tr '\n' ' ')"
    [ "${TOP}" = "${HUB_EXPECTED}" ] || die "unexpected hub staged top entries: ${TOP}"
    verify_remote_tree "${HUB_SSH}" "${HUB_RELEASES}/${SHA}" "$(hub_expected_sums)" "hub ${SHA}"
    log "hub staged: ${HUB_RELEASES}/${SHA}"
}

stage_web() {
    log "staging web ${SHA}"
    # Ожидаемый состав корня — из собранного dist этого релиза, не из зашитого списка.
    EXPECTED_TOP="$(web_top_entries "${DIST}/web")" || die "dist web is not a build root (see above)"
    log "expected web top entries (from dist): ${EXPECTED_TOP}"
    if ssh_web "test -d '${WEB_RELEASES}/${SHA}'"; then
        TOP="$(ssh_web "ls -A '${WEB_RELEASES}/${SHA}'" | LC_ALL=C sort | tr '\n' ' ')"
        [ "${TOP}" = "${EXPECTED_TOP}" ] || die "web ${SHA} already staged but has wrong top entries: ${TOP}"
        [ "$(ssh_web "cat '${WEB_RELEASES}/${SHA}/_app/version.json'")" = "{\"version\":\"${SHA}\"}" ] \
            || die "web ${SHA} already staged but version.json differs, remove manually"
        # Полный пофайловый состав: иначе испорченный файл при тех же top-level
        # именах и верном version.json проходил повторный прогон.
        verify_remote_tree "${WEB_SSH}" "${WEB_RELEASES}/${SHA}" "$(web_expected_sums)" "web ${SHA}"
        log "web ${SHA} already staged identically, skipping"
        return 0
    fi
    # Конвейер именно содержимого dist/web (урок: tar целого web давал лишний уровень web/).
    tar -C "${DIST}/web" -cf - . | recv_to "${WEB_SSH}" "rm -rf '/tmp/webstage-${SHA}' && mkdir -p '/tmp/webstage-${SHA}' && tar -C '/tmp/webstage-${SHA}' -xf -"
    # Полный пофайловый состав против dist (и dist/SHA256SUMS, проверенного выше).
    verify_remote_tree "${WEB_SSH}" "/tmp/webstage-${SHA}" "$(web_expected_sums)" "web ${SHA}"
    # Точный состав корня stagedir (урок: assert до flip, иначе битый build).
    for entry in ${EXPECTED_TOP}; do
        ssh_web "test -e '/tmp/webstage-${SHA}/${entry}'" || die "web staged dir lacks top entry: ${entry}"
    done
    TOP="$(ssh_web "ls -A '/tmp/webstage-${SHA}'" | LC_ALL=C sort | tr '\n' ' ')"
    [ "${TOP}" = "${EXPECTED_TOP}" ] || die "unexpected web staged top entries: ${TOP}"
    [ "$(ssh_web "cat '/tmp/webstage-${SHA}/_app/version.json'")" = "{\"version\":\"${SHA}\"}" ] \
        || die "staged version.json is not ${SHA}"
    ssh_web "mkdir -p '${WEB_RELEASES}' && mv -T '/tmp/webstage-${SHA}' '${WEB_RELEASES}/${SHA}'"
    verify_remote_tree "${WEB_SSH}" "${WEB_RELEASES}/${SHA}" "$(web_expected_sums)" "web ${SHA}"
    log "web staged: ${WEB_RELEASES}/${SHA}"
}

stage_worker() {
    log "staging worker ${SHA}"
    if [ -d "${WORKER_RELEASES}/${SHA}" ]; then
        # Повтор: только полный состав (бинарь + RELEASE), а не один бинарь.
        [ "$(tree_sums_local "${WORKER_RELEASES}/${SHA}")" = "$(worker_expected_sums)" ] \
            || die "worker ${SHA} already staged but differs from dist, remove manually"
        log "worker ${SHA} already staged identically, skipping"
        return 0
    fi
    # Публикация только после полной сборки: stage-каталог + атомарный mv -T.
    WORKER_STAGE="${WORKER_BASE}/.stage-${SHA}"
    rm -rf "${WORKER_STAGE}"
    install -d "${WORKER_STAGE}/bin"
    install -m 0755 "${DIST}/worker/bin/canterlot-worker" "${WORKER_STAGE}/bin/canterlot-worker.staged"
    cmp -s "${DIST}/worker/bin/canterlot-worker" "${WORKER_STAGE}/bin/canterlot-worker.staged" \
        || die "worker staged binary differs from dist"
    mv -T "${WORKER_STAGE}/bin/canterlot-worker.staged" "${WORKER_STAGE}/bin/canterlot-worker"
    cp "${DIST}/RELEASE" "${WORKER_STAGE}/RELEASE"
    [ "$(tree_sums_local "${WORKER_STAGE}")" = "$(worker_expected_sums)" ] \
        || die "worker ${SHA} staged tree differs from dist"
    mv -T "${WORKER_STAGE}" "${WORKER_RELEASES}/${SHA}"
    log "worker staged: ${WORKER_RELEASES}/${SHA}"
}

stage_hub
stage_web
stage_worker

# Финальные ворота: stagedir релиза существует везде, current/build не тронуты.
FINAL_HUB_NOW="$(deployed_sha hub)"
FINAL_WEB_NOW="$(deployed_sha web)"
[ "${FINAL_HUB_NOW}" != "${SHA}" ] || die "hub current already points at ${SHA}"
[ "${FINAL_WEB_NOW}" != "${SHA}" ] || die "web build already points at ${SHA}"
[ "$(readlink "${WORKER_CURRENT}")" != "${WORKER_RELEASES}/${SHA}" ] || die "worker current already points at ${SHA}"
ssh_hub "test -d '${HUB_RELEASES}/${SHA}/bin'" || die "hub staged dir missing after move"
ssh_web "test -d '${WEB_RELEASES}/${SHA}/_app'" || die "web staged dir missing after move"
[ -x "${WORKER_RELEASES}/${SHA}/bin/canterlot-worker" ] || die "worker staged binary missing after move"
trap - EXIT
log "staged ${SHA} everywhere; current/build untouched"
