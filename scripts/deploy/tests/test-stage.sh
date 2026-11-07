#!/usr/bin/env bash
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
# Тесты stage.sh: хэши, точный состав корней, идемпотентность, порядок сортировки.
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

SHA_NEW=aaaaaaaaaaaa
SHA_OLD=bbbbbbbbbbbb
OPENAPI_REAL="${REPO_ROOT}/docs/api/openapi.json"

setup_stage() {
    setup_case "stage-$1"
    DIST="${FAKE_ROOT}/dist/${SHA_NEW}"
    mkdir -p "${DIST}/hub/bin" "${DIST}/worker/bin" "${DIST}/web/_app" "${DIST}/web/icons"
    # shellcheck disable=SC2016 # format string is intentional
    printf '#!/bin/sh\nif [ "$1" = openapi ]; then cat "%s"; else exit 99; fi\n' "${OPENAPI_REAL}" >"${DIST}/hub/bin/canterlot-hub"
    chmod +x "${DIST}/hub/bin/canterlot-hub"
    echo fakeworker >"${DIST}/worker/bin/canterlot-worker"
    echo '<html></html>' >"${DIST}/web/200.html"
    # Настоящий состав build/ релиза 1.3.0 (84497592e8d6): 200.html _app icons manifest.webmanifest theme-init.js.
    # service-worker.js убран в 1.0.0; список не зашит, а берётся из dist.
    for f in manifest.webmanifest theme-init.js; do echo x >"${DIST}/web/${f}"; done
    echo x >"${DIST}/web/icons/icon.png"
    printf '{"version":"%s"}' "${SHA_NEW}" >"${DIST}/web/_app/version.json"
    printf 'sha=%s\ndate=x\nmigrations_last=0040_monthly_limits.sql\n' "${SHA_NEW}" >"${DIST}/RELEASE"
    (cd "${DIST}" && find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)
    deploy_release hub "${SHA_OLD}"
    deploy_release web "${SHA_OLD}"
    deploy_release worker "${SHA_OLD}"
}

# S1: корректный релиз — staged везде, current/build не тронуты, tmp чист.
case_begin "stage happy"
setup_stage happy
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/stage.log"
fi
[ "$(readlink "${HUB_CURRENT}")" = "releases/${SHA_OLD}" ] && case_ok "hub current untouched" || case_fail "hub current moved"
[ "$(readlink "${WEB_BUILD_LINK}")" = "releases/${SHA_OLD}" ] && case_ok "web build untouched" || case_fail "web build moved"
[ "$(readlink "${WORKER_CURRENT}")" = "releases/${SHA_OLD}" ] && case_ok "worker current untouched" || case_fail "worker current moved"
[ -x "${HUB_RELEASES}/${SHA_NEW}/bin/canterlot-hub" ] && case_ok "hub staged" || case_fail "hub not staged"
[ -f "${WEB_RELEASES}/${SHA_NEW}/_app/version.json" ] && case_ok "web staged" || case_fail "web not staged"
[ -x "${WORKER_RELEASES}/${SHA_NEW}/bin/canterlot-worker" ] && case_ok "worker staged" || case_fail "worker not staged"
[ -z "$(ls -A "${FAKE_ROOT}/tmp" 2>/dev/null)" ] && case_ok "no tmp leftovers" || case_fail "tmp junk left"
teardown_case
case_end

# S2: лишний файл в корне УЖЕ ЗАЛИТОГО stagedir (в dist его нет) — отказ повторного прогона.
case_begin "stage extra top entry in a staged dir"
setup_stage extratop
bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >/dev/null 2>&1 \
    || { case_fail "first run failed"; }
echo junk >"${WEB_RELEASES}/${SHA_NEW}/EXTRA"
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_fail "exit 0 despite extra entry"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/stage.log" "already staged but has wrong top entries" "top-entry assert fired"
teardown_case
case_end

# S2b: ожидаемый состав берётся из dist: новый top-level файл сборки (tokens.css)
# заливается и сверяется точно; а service-worker.js в сборке нет — и не требуется.
case_begin "stage derives the web top entries from the dist"
setup_stage derived
echo tokens >"${DIST}/web/tokens.css"
(cd "${DIST}" && find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_ok "exit 0 with a new top-level file and without service-worker.js"
else
    case_fail "exit nonzero"; tail -3 "${FAKE_ROOT}/stage.log"
fi
[ "$(find "${WEB_RELEASES}/${SHA_NEW}" -mindepth 1 -maxdepth 1 -printf '%f\n' | LC_ALL=C sort | tr '\n' ' ')" = "200.html _app icons manifest.webmanifest theme-init.js tokens.css " ] \
    && case_ok "staged top entries equal the dist exactly" || case_fail "staged top entries differ: $(find "${WEB_RELEASES}/${SHA_NEW}" -mindepth 1 -maxdepth 1 -printf '%f ')"
teardown_case
case_end

# S2c: dist, не похожий на содержимое build/ (лишний уровень web/ или нет 200.html), — отказ.
case_begin "stage rejects a dist web that is not a build root"
setup_stage nestedweb
mkdir -p "${DIST}/web/web"
echo x >"${DIST}/web/web/nested"
(cd "${DIST}" && find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_fail "exit 0 despite a nested web/ level"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/stage.log" "nested web/ level" "nested level reported"
[ ! -e "${WEB_RELEASES}/${SHA_NEW}" ] && case_ok "nothing staged on web" || case_fail "web staged"
teardown_case
case_end

# S3: чужой version.json — отказ до flip.
case_begin "stage wrong version"
setup_stage wrongver
printf '{"version":"other"}' >"${DIST}/web/_app/version.json"
(cd "${DIST}" && find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_fail "exit 0 despite wrong version"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/stage.log" "staged version.json is not" "version assert fired"
teardown_case
case_end

# S4: повтор идемпотентен.
case_begin "stage idempotent"
setup_stage idempotent
bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >/dev/null 2>&1 \
    || { case_fail "first run failed"; teardown_case; case_end; }
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage2.log" 2>&1; then
    case_ok "second run exit 0"
else
    case_fail "second run failed"
fi
[ "$(grep -c "already staged identically, skipping" "${FAKE_ROOT}/stage2.log")" = "3" ] \
    && case_ok "all three skipped" || case_fail "skip count wrong"
teardown_case
case_end

# S5: порядок сортировки — C-локаль: RELEASE < SHA256SUMS < bin.
case_begin "stage C locale order"
[ "$(printf 'bin\nRELEASE\nSHA256SUMS\n' | LC_ALL=C sort | tr '\n' ' ')" = "RELEASE SHA256SUMS bin " ] \
    && case_ok "C-sorted hub order" || case_fail "unexpected C order"
setup_stage order
if LC_ALL=C bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >/dev/null 2>&1; then
    case_ok "exit 0 under LC_ALL=C"
else
    case_fail "exit nonzero under LC_ALL=C"
fi
teardown_case
case_end

# S6: stagedir с отличающимся содержимым — громкий отказ, не молчаливое использование.
case_begin "stage differing stagedir"
setup_stage differs
mkdir -p "${HUB_RELEASES}/${SHA_NEW}/bin"
echo different >"${HUB_RELEASES}/${SHA_NEW}/bin/canterlot-hub"
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_fail "exit 0 despite differing staged dir"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/stage.log" "already staged but differs" "differing stagedir refused"
teardown_case
case_end

# S8: повтор web-релиза с испорченным файлом при тех же top-level именах и
# верном version.json — полный пофайловый состав обязан отклонить.
case_begin "stage web retry detects a corrupt file"
setup_stage retryweb
bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >/dev/null 2>&1 \
    || { case_fail "first run failed"; teardown_case; case_end; }
printf 'corrupted\n' >"${WEB_RELEASES}/${SHA_NEW}/200.html"
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_fail "exit 0 despite a corrupt staged web file"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/stage.log" "staged tree differs from dist" "corrupt web retry refused"
teardown_case
case_end

# S9: повтор worker-релиза с испорченным RELEASE — раньше сравнивался только бинарь.
case_begin "stage worker retry checks RELEASE"
setup_stage retryworker
bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >/dev/null 2>&1 \
    || { case_fail "first run failed"; teardown_case; case_end; }
printf 'sha=other\ndate=x\n' >"${WORKER_RELEASES}/${SHA_NEW}/RELEASE"
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_fail "exit 0 despite a corrupt worker RELEASE"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/stage.log" "already staged but differs" "corrupt worker retry refused"
teardown_case
case_end

# S10: dist не совпадает со своим SHA256SUMS — отказ до копирования.
case_begin "stage rejects a dist that fails SHA256SUMS"
setup_stage badmanifest
printf 'tampered\n' >>"${DIST}/web/200.html"
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_fail "exit 0 despite broken dist SHA256SUMS"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/stage.log" "SHA256SUMS does not verify" "dist manifest failure reported"
[ ! -e "${HUB_RELEASES}/${SHA_NEW}" ] && case_ok "nothing staged" || case_fail "hub staged despite bad manifest"
teardown_case
case_end

# S11: worker публикуется только целиком, без stage-каталогов.
case_begin "stage worker publishes atomically"
setup_stage atomicworker
bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >/dev/null 2>&1
[ -x "${WORKER_RELEASES}/${SHA_NEW}/bin/canterlot-worker" ] \
    && [ -f "${WORKER_RELEASES}/${SHA_NEW}/RELEASE" ] \
    && case_ok "worker release complete" || case_fail "worker release incomplete"
[ -z "$(find "${WORKER_RELEASES}" -maxdepth 1 -name '.stage-*' -print -quit)" ] \
    && case_ok "no stage dir left behind" || case_fail "stage dir left behind"
teardown_case
case_end

# S7: SHA с shell-метасимволами отклоняется до любой удалённой команды.
case_begin "stage rejects invalid release id"
setup_stage badsha
if ( cd "${FAKE_ROOT}" && bash "${DEPLOY_DIR}/stage.sh" "';>PWN;#xxxx" --dist "${FAKE_ROOT}/dist" ) >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_fail "exit 0 despite an invalid release id"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/stage.log" "invalid release id" "invalid release id reported"
[ ! -e "${FAKE_ROOT}/PWN" ] && case_ok "no injected file created" || case_fail "shell injection executed"
[ ! -e "${HUB_RELEASES}/';>PWN;#xxxx" ] && case_ok "nothing staged" || case_fail "odd release dir created"
teardown_case
case_end

printf 'stage harness: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
