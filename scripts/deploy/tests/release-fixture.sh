# shellcheck shell=bash
# Стенд scripts/release.sh: временный «основной чекаут» (git-репозиторий с origin в
# соседнем голом репозитории), настоящие release.sh, check-version.sh и deploy.conf,
# подменённые git tag -s, build-release.sh, stage.sh, cutover.sh, check-proxy.sh и
# publish-release.sh. Подключается из test-release.sh после lib-harness.sh.
# shellcheck disable=SC2034,SC2015,SC2119,SC2120

RVER=1.1.0
REAL_GIT="$(command -v git)"

# Значения стенда, которые не совпадают с примером (denylist release.sh).
release_site_env() {
    export SITE_ORIGIN=https://hub.canterlot.test WSS_ORIGIN=wss://hub.canterlot.test
    export SITE_CODES="hub.canterlot.test:200" HUB_IP=10.40.0.5
    export CANTERLOT_SITE_ORIGIN=https://hub.canterlot.test CANTERLOT_WSS_ORIGIN=wss://hub.canterlot.test
}

# Файлы версии, как в корне репозитория.
write_version_files() {
    local ver="$1" dir="$2"
    mkdir -p "${dir}/web" "${dir}/docs/api"
    cat >"${dir}/Cargo.toml" <<EOF
[workspace]
members = []

[workspace.package]
version = "${ver}"
edition = "2021"

[workspace.dependencies]
serde = { version = "1.0.0" }
EOF
    printf '# lock\nversion = 4\n' >"${dir}/Cargo.lock"
    cat >"${dir}/web/package.json" <<EOF
{
  "name": "canterlot-web",
  "private": true,
  "version": "${ver}",
  "dependencies": {
    "svelte": { "version": "5.0.0" }
  }
}
EOF
    cat >"${dir}/docs/api/openapi.json" <<EOF
{
  "openapi": "3.1.0",
  "info": {
    "title": "canterlot hub API",
    "version": "${ver}"
  },
  "components": { "schemas": { "Version": { "properties": { "version": { "type": "string" } } } } }
}
EOF
    cat >"${dir}/README.md" <<EOF
# canterlot

![версия](https://img.shields.io/badge/%D0%B2%D0%B5%D1%80%D1%81%D0%B8%D1%8F-${ver}-00f4ee)

Текущая версия — ${ver}. История — CHANGES.rst.
EOF
}
changes_entry() {
    printf '.. _v%s:\n\n%s — 14.11.2026\n-------------------\n\nИсправлено\n~~~~~~~~~~\n\n- запись %s.\n\n' "$1" "$1" "$1"
}

# Подменённые инструменты в scripts/ временного чекаута и git с подменой tag -s.
write_release_fakes() {
    local r="$1"
    mkdir -p "${r}/scripts/deploy" "${FAKE_ROOT}/gitbin"
    cat >"${FAKE_ROOT}/gitbin/git" <<EOF
#!/usr/bin/env bash
# tag -s без ключа подписи: тот же аннотированный тег без подписи.
args=()
for a in "\$@"; do
    if [ "\$a" = "-s" ] && [ "\${1:-}" = "tag" ]; then a=-a; fi
    args+=("\$a")
done
case "\${1:-}" in tag | push) echo "git \$*" >>"${FAKE_ROOT}/state/calls.log" ;; esac
exec "${REAL_GIT}" "\${args[@]}"
EOF
    cat >"${FAKE_ROOT}/fake-bin/cargo" <<EOF
#!/usr/bin/env bash
echo "cargo \$*" >>"${FAKE_ROOT}/state/calls.log"
# Как настоящий cargo update -w: версия членов workspace в Cargo.lock меняется.
# Только во временном чекауте стенда: остальные вызовы (spec_compat cutover) — запись в лог.
if [ "\$*" = "update -w --offline" ] && [ "\${PWD}" = "${FAKE_ROOT}/repo" ]; then
    printf '# workspace %s\n' "\$(sed -n 's/^version = "\\(.*\\)"/\\1/p' Cargo.toml | head -1)" >>Cargo.lock
fi
EOF
    cat >"${r}/scripts/build-release.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
echo "build-release $*" >>"${FAKE_ROOT}/state/calls.log"
[ "${2:-}" = --local ] || exit 2
if [ -e "${FAKE_ROOT}/state/build-fail" ]; then echo "build failed (fixture)"; exit 1; fi
sha="$(git rev-parse "$1^{commit}")"
d="${CANTERLOT_DIST_ROOT}/${sha:0:12}"
mkdir -p "${d}/hub/bin"
echo bin >"${d}/hub/bin/canterlot-hub"
printf 'sha=%s\nmigrations_last=0041_x.sql\nsource=%s\n' "${sha:0:12}" "${sha}" >"${d}/RELEASE"
printf 'format=1\ncapabilities=exec_v1 pilot\n' >"${d}/EXPECT"
(cd "${d}" && sha256sum hub/bin/canterlot-hub RELEASE EXPECT >SHA256SUMS)
echo "built ${d}"
EOF
    cat >"${r}/scripts/deploy/stage.sh" <<'EOF'
#!/usr/bin/env bash
echo "stage $*" >>"${FAKE_ROOT}/state/calls.log"
echo "[00:00:00] staged $1 everywhere; current/build untouched"
EOF
    cat >"${r}/scripts/check-proxy.sh" <<'EOF'
#!/usr/bin/env bash
echo "check-proxy $*" >>"${FAKE_ROOT}/state/calls.log"
if [ -e "${FAKE_ROOT}/state/proxy-fail" ]; then echo "check-proxy: FAIL: fixture"; exit 1; fi
echo "check-proxy: PASS"
EOF
    cat >"${r}/scripts/publish-release.sh" <<'EOF'
#!/usr/bin/env bash
echo "publish-release $*" >>"${FAKE_ROOT}/state/calls.log"
if [ -e "${FAKE_ROOT}/state/publish-fail" ]; then echo "GitHub upload failed (fixture)"; exit 1; fi
echo "published $1"
EOF
    # cutover.sh по сценарию: строка state/cutover-plan на вызов, «режим статус [ключ=значение…]».
    # Особые статусы: none — нет файла результата; truncated — обрезанный файл.
    cat >"${r}/scripts/deploy/cutover.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "cutover $*" >>"${FAKE_ROOT}/state/calls.log"
mode=""; result=""; sha=""
while (($#)); do
    case "$1" in
        --rehearse) mode=rehearse; shift ;;
        --execute) mode=execute; sha="$2"; shift 2 ;;
        --result | --confirm | --source | --dist) [ "$1" != --result ] || result="$2"; shift 2 ;;
        *) sha="$1"; shift ;;
    esac
done
n="$(cat "${FAKE_ROOT}/state/cutover-n" 2>/dev/null || echo 0)"; n=$((n + 1)); echo "${n}" >"${FAKE_ROOT}/state/cutover-n"
line="$(sed -n "${n}p" "${FAKE_ROOT}/state/cutover-plan")"
[ -n "${line}" ] || { echo "fixture: no cutover-plan line ${n}"; exit 99; }
read -r want status rest <<<"${line}"
[ "${want}" = "${mode}" ] || { echo "fixture: expected ${want}, got ${mode}"; exit 98; }
echo "fake cutover ${mode} -> ${status}"
[ "${status}" != none ] || exit 1
declare -A kv=([format]=1 [mode]="${mode}" [status]="${status}" [exit_code]=1 [signal]="" [release]="${sha}"
    [plan]=0123456789ab [token]="" [token_valid_until]="" [phase]=end [vzdump_done]=no [worker_stopped]=no
    [dump_verified]=no [hub_flipped]=no [worker_live]=no [recovery]="" [hard_failures]=0 [soft_failures]=0
    [soft_by_kind]="" [fail_codes]="" [speech_soft_failures]=0 [speech_summary]="speech ok" [web_release]=""
    [worker_downtime_s]="" [hub_interruption_s]="" [worker_stop_at]="" [vzdump_notes]="" [vzdump_archive]=""
    [dump]="" [stop_block]="" [log]="$(readlink /proc/$$/fd/1)")
case "${status}" in
    pass) kv[exit_code]=0; kv[token]="$(date -u +%s)-0123456789ab-${n}abcdef0"; kv[token_valid_until]=2099-01-01T00:00:00Z ;;
    done) kv[exit_code]=0; kv[vzdump_done]=yes; kv[worker_stopped]=yes; kv[dump_verified]=yes; kv[hub_flipped]=yes
          kv[worker_live]=yes; kv[worker_downtime_s]=24; kv[hub_interruption_s]=3; kv[dump]=/var/backups/canterlot/x.dump
          kv[vzdump_archive]=ct/130/2026-11-14T10:00:00Z; kv[vzdump_notes]=pre-x ;;
    stop_and_report) kv[hub_flipped]=yes; kv[stop_block]="${result%.result}.stop.txt"
          printf '=== STOP-AND-REPORT (fixture) ===\n' >"${kv[stop_block]}" ;;
esac
for pair in ${rest}; do kv[${pair%%=*}]="${pair#*=}"; done
keys="format mode status exit_code signal release plan token token_valid_until started_at finished_at phase vzdump_done worker_stopped dump_verified hub_flipped worker_live recovery hard_failures soft_failures soft_by_kind fail_codes speech_soft_failures speech_summary web_release worker_downtime_s hub_interruption_s worker_stop_at vzdump_notes vzdump_archive dump stop_block log"
{
    for k in ${keys}; do
        [ "${status}" != truncated ] || [ "${k}" != log ] || break
        printf '%s=%s\n' "${k}" "${kv[${k}]:-}"
    done
} >"${result}"
if [ "${status}" = pass ] || [ "${status}" = done ]; then
    printf '=== owner checklist: authenticated checks (the toolkit does not do these) ===\n  [ ] fixture item\n  [ ] paste the result of each line into the deploy log section\n'
fi
exit "${kv[exit_code]}"
EOF
    chmod +x "${FAKE_ROOT}/gitbin/git" "${FAKE_ROOT}/fake-bin/cargo" "${r}"/scripts/*.sh "${r}"/scripts/deploy/*.sh
}

# Новый «основной чекаут» в FAKE_ROOT/repo на версии RVER, origin запушен,
# аттестация гейта для HEAD, значения стенда не из примера. Для setup_case
# или поверх setup_cutover (тогда setup_case уже сделан).
make_release_repo() {
    RREPO="${FAKE_ROOT}/repo"
    export GIT_CONFIG_GLOBAL="${FAKE_ROOT}/gitconfig" GIT_CONFIG_NOSYSTEM=1
    "${REAL_GIT}" config --file "${GIT_CONFIG_GLOBAL}" user.name Fixture
    "${REAL_GIT}" config --file "${GIT_CONFIG_GLOBAL}" user.email fixture@canterlot.invalid
    "${REAL_GIT}" config --file "${GIT_CONFIG_GLOBAL}" init.defaultBranch main
    mkdir -p "${RREPO}/scripts/deploy"
    write_version_files 1.0.0 "${RREPO}"
    { printf 'История изменений\n=================\n\n'; changes_entry 1.0.0; } >"${RREPO}/CHANGES.rst"
    printf '.superpowers/\n' >"${RREPO}/.gitignore"
    cp "${REPO_ROOT}/scripts/release.sh" "${REPO_ROOT}/scripts/check-version.sh" "${RREPO}/scripts/"
    cp "${REPO_ROOT}/scripts/deploy/deploy.conf" "${RREPO}/scripts/deploy/"
    write_release_fakes "${RREPO}"
    export PATH="${FAKE_ROOT}/gitbin:${PATH}"
    (
        cd "${RREPO}" || exit 1
        git init -q
        git add -A
        git commit -q -m "Начало"
        git init -q --bare "${FAKE_ROOT}/origin.git"
        git remote add origin "${FAKE_ROOT}/origin.git"
        git push -q origin main
    )
    release_bump_commit "${RVER}"
    attest_head
    release_site_env
    export CANTERLOT_SITE_ENV="${FAKE_ROOT}/no-site.env"
    export RELEASE_RETRY_DELAY=0 RELEASE_MIN_FREE_MB=1
    unset CANTERLOT_RELEASE_DIR CANTERLOT_GATE_DIR CANTERLOT_DIST_ROOT
    RDIR="${RREPO}/.superpowers/deploy"
    : >"${FAKE_ROOT}/state/calls.log"
}
# Коммит бампа без release.sh (для ship): версия и запись CHANGES.
release_bump_commit() {
    (
        cd "${RREPO}" || exit 1
        write_version_files "$1" .
        { printf 'История изменений\n=================\n\n'; changes_entry "$1"; changes_entry 1.0.0; } >CHANGES.rst
        git add -A
        git commit -q -m "Выпустить версию $1"
    )
}
attest_head() {
    local sha="${1:-$(git -C "${RREPO}" rev-parse HEAD)}" content_sha="${2:-}"
    mkdir -p "${RREPO}/.superpowers/gate"
    printf 'sha=%s\nnpm_ci_exit=0\ncheck_exit=0\nweb_exit=%s\nsummary=fixture\n' \
        "${content_sha:-${sha}}" "${ATTEST_WEB_EXIT:-0}" >"${RREPO}/.superpowers/gate/${sha}.ok"
}
cutover_plan() { printf '%s\n' "$@" >"${FAKE_ROOT}/state/cutover-plan"; rm -f "${FAKE_ROOT}/state/cutover-n"; }

run_release() {
    RREL_RC=0
    (cd "${RREPO}" && bash scripts/release.sh "$@") >"${FAKE_ROOT}/release.out" 2>&1 || RREL_RC=$?
}
rres() { sed -n "s/^$1=//p" "${RDIR}/release-${RVER}.result" 2>/dev/null | head -1; }
rstate() { sed -n "s/^$1=//p" "${RDIR}/release-${RVER}.state" 2>/dev/null | head -1 | awk '{print $1}'; }
calls_count() { grep -c -- "$1" "${FAKE_ROOT}/state/calls.log" || true; }
origin_tag() { "${REAL_GIT}" --git-dir="${FAKE_ROOT}/origin.git" rev-parse -q --verify "refs/tags/v${RVER}^{commit}" 2>/dev/null || true; }
