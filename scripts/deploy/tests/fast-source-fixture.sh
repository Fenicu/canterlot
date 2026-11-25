# shellcheck shell=bash
# Исходный git-репозиторий для --source быстрого пути: две ревизии, развёрнутая
# (FAST_LIVE_FULL, короткое имя SHA_PREV) и цель (FAST_TARGET_FULL, SHA_NEW). Имена
# релизов стенда становятся настоящими хэшами коммитов, поэтому make_fast_source
# вызывается ДО setup_cutover. Подключается после cutover-fixture.sh.
# shellcheck disable=SC2034,SC2015

# Что меняет цель относительно развёрнутой (аргументы make_fast_source):
#   other          только CHANGES.rst (прикладной релиз без особенностей)
#   crate-src      исходник крейта (crates/canterlot-worker/src)
#   docs, web-src  документация, исходник веба
#   agents-md, claude-md, web-e2e   правила для агентов в корне и стенд Playwright (в сборку не входят)
#   nested-agents-md, web-config    AGENTS.md не в корне и конфиг веба вне web/e2e (вне списка)
#   version-bump   только номер версии в Cargo.toml, Cargo.lock, package.json
#   migration      новый файл миграции;  unicode-migration — с не-ASCII именем
#   rename-in      файл из docs/ переименован в каталог миграций; rename-out — обратно
#   dbrs, mainrs   crates/canterlot-hub/src/db.rs, main.rs (загрузка и запуск миграций)
#   installation, presets, presets-dir   то, что хаб исполняет при запуске вне манифеста
#   tooling        новый файл scripts/deploy/tests/note.txt;  release-sh, build-sh, publish-sh — скрипты выпуска
#   script         посторонний скрипт scripts/extra.sh
#   docker, toolchain, buildrs   Dockerfile сборки, rust-toolchain.toml, build.rs хаба
#   lock-dep       версия внешней зависимости в Cargo.lock;  toml-dep — зависимость в Cargo.toml
#   pkg-dep        зависимость в web/package.json
make_fast_source() {
    local what path g
    hash -r # PATH стенда прошлого кейса мог указывать на уже удалённый git
    FAST_SRC="$(mktemp -d /tmp/canterlot-deploy-src.XXXXXX)"
    g=(git -C "${FAST_SRC}" -c user.name=Fixture -c user.email=fixture@canterlot.invalid -c commit.gpgsign=false)
    for path in docs/api/openapi.json crates/canterlot-proto/proto crates/canterlot-worker/src/supervisor/mod.rs \
        crates/canterlot-worker/src/config.rs crates/canterlot-worker/tests/it/fixtures/spec \
        crates/canterlot-hub/src/config.rs crates/canterlot-speech/src/config.rs crates/canterlot-hub/migrations; do
        mkdir -p "${FAST_SRC}/$(dirname "${path}")"
        cp -r "${REPO_ROOT}/${path}" "${FAST_SRC}/${path}"
    done
    # Настоящая обвязка выката (без тестов): быстрый путь сверяет исполняемую обвязку с целью.
    mkdir -p "${FAST_SRC}/scripts/deploy"
    for path in "${REPO_ROOT}"/scripts/deploy/*; do
        [ -f "${path}" ] && cp "${path}" "${FAST_SRC}/scripts/deploy/"
    done
    cp "${REPO_ROOT}/scripts/release.sh" "${REPO_ROOT}/scripts/build-release.sh" "${REPO_ROOT}/scripts/publish-release.sh" "${FAST_SRC}/scripts/"
    mkdir -p "${FAST_SRC}/docs" "${FAST_SRC}/web/src" "${FAST_SRC}/web/static" "${FAST_SRC}/docker/release-build" "${FAST_SRC}/crates/canterlot-hub/src"
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "1.0.0"\n' >"${FAST_SRC}/Cargo.toml"
    printf 'version = 4\n\n[[package]]\nname = "canterlot-hub"\nversion = "1.0.0"\n\n[[package]]\nname = "serde"\nversion = "1.0.1"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n' >"${FAST_SRC}/Cargo.lock"
    printf '{\n  "name": "web",\n  "version": "1.0.0",\n  "dependencies": {"svelte": "5.0.0"}\n}\n' >"${FAST_SRC}/web/package.json"
    printf 'pub fn a() {}\n' >"${FAST_SRC}/crates/canterlot-hub/src/db.rs"
    printf 'fn main() {}\n' >"${FAST_SRC}/crates/canterlot-hub/src/main.rs"
    printf 'pub fn check() {}\n' >"${FAST_SRC}/crates/canterlot-hub/src/installation.rs"
    printf 'pub fn seed_all_missing() {}\n' >"${FAST_SRC}/crates/canterlot-hub/src/presets.rs"
    printf 'note\n' >"${FAST_SRC}/docs/note.md"
    printf 'moved.sql\n' >"${FAST_SRC}/docs/moved.sql"
    printf 'export const a = 1;\n' >"${FAST_SRC}/web/src/a.ts"
    printf 'x\n' >"${FAST_SRC}/web/static/x.txt"
    printf 'FROM scratch\n' >"${FAST_SRC}/docker/release-build/Dockerfile"
    printf 'История\n' >"${FAST_SRC}/CHANGES.rst"
    "${g[@]}" init -q
    "${g[@]}" add -A
    "${g[@]}" commit -q -m live
    FAST_LIVE_FULL="$("${g[@]}" rev-parse HEAD)"
    SHA_PREV="${FAST_LIVE_FULL:0:12}"
    printf 'выпуск\n' >>"${FAST_SRC}/CHANGES.rst"
    for what in "$@"; do
        case "${what}" in
            other) ;;
            crate-src) printf '// change\n' >>"${FAST_SRC}/crates/canterlot-worker/src/config.rs" ;;
            docs) printf 'more\n' >>"${FAST_SRC}/docs/note.md" ;;
            agents-md) printf 'rules\n' >"${FAST_SRC}/AGENTS.md" ;;
            claude-md) printf '@AGENTS.md\n' >"${FAST_SRC}/CLAUDE.md" ;;
            web-e2e) mkdir -p "${FAST_SRC}/web/e2e/screenshots" && printf 'export {};\n' >"${FAST_SRC}/web/e2e/screenshots/readme.spec.ts" ;;
            nested-agents-md) printf 'rules\n' >"${FAST_SRC}/scripts/AGENTS.md" ;;
            web-config) printf 'export default {};\n' >"${FAST_SRC}/web/playwright.config.ts" ;;
            web-src) printf 'export const b = 2;\n' >>"${FAST_SRC}/web/src/a.ts" ;;
            version-bump)
                sed -i 's/^version = "1.0.0"/version = "1.0.1"/' "${FAST_SRC}/Cargo.toml"
                sed -i '0,/^version = "1.0.0"/s//version = "1.0.1"/' "${FAST_SRC}/Cargo.lock"
                sed -i 's/"version": "1.0.0"/"version": "1.0.1"/' "${FAST_SRC}/web/package.json" ;;
            migration) printf -- '-- fixture\nselect 1;\n' >"${FAST_SRC}/crates/canterlot-hub/migrations/9999_fixture.sql" ;;
            unicode-migration) printf -- '-- fixture\nselect 1;\n' >"${FAST_SRC}/crates/canterlot-hub/migrations/0052_étape.sql" ;;
            rename-in) git -C "${FAST_SRC}" mv docs/moved.sql crates/canterlot-hub/migrations/9998_moved.sql ;;
            rename-out) git -C "${FAST_SRC}" mv "$(cd "${FAST_SRC}" && find crates/canterlot-hub/migrations -name '*.sql' | sort | head -1)" docs/out.sql ;;
            dbrs) printf '// change\n' >>"${FAST_SRC}/crates/canterlot-hub/src/db.rs" ;;
            mainrs) printf '// change\n' >>"${FAST_SRC}/crates/canterlot-hub/src/main.rs" ;;
            installation) printf '// change\n' >>"${FAST_SRC}/crates/canterlot-hub/src/installation.rs" ;;
            presets) printf '// change\n' >>"${FAST_SRC}/crates/canterlot-hub/src/presets.rs" ;;
            presets-dir) mkdir -p "${FAST_SRC}/crates/canterlot-hub/src/presets" && printf 'pub fn x() {}\n' >"${FAST_SRC}/crates/canterlot-hub/src/presets/seed.rs" ;;
            # Файл под scripts/deploy, но вне исполняемой обвязки (tests/): классификатор видит правку
            # обвязки, а исполняемая обвязка стенда остаётся равной коммиту цели.
            tooling) mkdir -p "${FAST_SRC}/scripts/deploy/tests" && printf '# change\n' >"${FAST_SRC}/scripts/deploy/tests/note.txt" ;;
            release-sh) printf '# change\n' >>"${FAST_SRC}/scripts/release.sh" ;;
            build-sh) printf '# change\n' >>"${FAST_SRC}/scripts/build-release.sh" ;;
            publish-sh) printf '# change\n' >>"${FAST_SRC}/scripts/publish-release.sh" ;;
            script) printf '#!/bin/sh\n' >"${FAST_SRC}/scripts/extra.sh" ;;
            docker) printf 'RUN true\n' >>"${FAST_SRC}/docker/release-build/Dockerfile" ;;
            toolchain) printf '[toolchain]\nchannel = "1.99"\n' >"${FAST_SRC}/rust-toolchain.toml" ;;
            buildrs) printf 'fn main() {}\n' >"${FAST_SRC}/crates/canterlot-hub/build.rs" ;;
            lock-dep) sed -i 's/^version = "1.0.1"/version = "1.0.2"/' "${FAST_SRC}/Cargo.lock" ;;
            toml-dep) printf '\n[workspace.dependencies]\nserde = "1"\n' >>"${FAST_SRC}/Cargo.toml" ;;
            pkg-dep) sed -i 's/"svelte": "5.0.0"/"svelte": "5.0.1"/' "${FAST_SRC}/web/package.json" ;;
            *) echo "make_fast_source: unknown change ${what}" >&2; return 1 ;;
        esac
    done
    "${g[@]}" add -A
    "${g[@]}" commit -q -m target
    FAST_TARGET_FULL="$("${g[@]}" rev-parse HEAD)"
    SHA_NEW="${FAST_TARGET_FULL:0:12}"
    SHA_NEW_FULL="${FAST_TARGET_FULL}"
    export SHA_PREV SHA_NEW SHA_NEW_FULL FAST_SRC FAST_LIVE_FULL FAST_TARGET_FULL
}
# Убирает временный репозиторий и проверяет, что его больше нет (вызывать ДО teardown_case:
# после него подменный rm уже удалён вместе со стендом).
drop_fast_source() {
    local gone="${FAST_SRC:-}"
    [ -n "${gone}" ] || return 0
    if [ -d "${gone}" ]; then rm -rf "${gone}"; fi  # lint: local-temp
    if [ -e "${gone}" ]; then case_fail "the scratch source ${gone} was not removed"; else case_ok "the scratch source repository is gone"; fi
    FAST_SRC=""
}

# Запись идентичности кластера у «прежнего дампа»: источник ожидаемого system_identifier для быстрого
# пути (как после настоящего полного прогона, который её пишет).
seed_identity_sidecar() {
    printf 'system_identifier=7000000000000000001\ndatabase_oid=16384\ndatabase=canterlot\nsocket=/var/run/postgresql\n' \
        >"${HUB_BACKUP_DIR}/canterlot-pre-seed-20261101T000000Z.dump.identity"
}

# --- стенд быстрого пути (второй проход сценариев test-cutover.sh и развилки test-fast*.sh) ---
# Стенд cutover на настоящем git-источнике; база уже на последней миграции релиза (0041), как
# при выпуске без новых миграций. setup_fast ИМЯ ИЗМЕНЕНИЯ... (изменения — как у make_fast_source).
setup_fast() {
    local name="$1"
    shift
    make_fast_source "$@"
    setup_cutover "${name}"
    printf 'MIGR_MAX=41\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
    cp "${FAKE_ROOT}/state/db.txt" "${FAKE_ROOT}/state/db-pre.txt"
    # Развёрнутый релиз уже на 0041: так выглядит выпуск без новых миграций.
    sed -i 's/^migrations_last=.*/migrations_last=0041_fake_harness.sql/' \
        "${HUB_RELEASES}/${SHA_PREV}/RELEASE" "${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
    # Развёрнутый релиз хранит полный хэш исходника, как настоящая сборка.
    printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${HUB_RELEASES}/${SHA_PREV}/RELEASE"
    printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
    seed_identity_sidecar
    : >"${FAKE_ROOT}/state/calls.log"
}
# Выпуск с новой миграцией на том же источнике: база и развёрнутый релиз на 0040, цель на 0041.
fast_to_full_migration() {
    printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >"${FAKE_ROOT}/state/db.txt"
    sed -i 's/^migrations_last=.*/migrations_last=0040_monthly_limits.sql/' \
        "${HUB_RELEASES}/${SHA_PREV}/RELEASE" "${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
}
finish_fast() { drop_fast_source; teardown_case; }
# fcut РЕЖИМ... : cutover.sh с источником-репозиторием; ssh-команды пишутся в state/ssh-cmds.log.
fcut() {
    FAKE_SSH_LOG_CMDS=1 bash "${DEPLOY_DIR}/cutover.sh" "$@" --source "${FAST_SRC}" --dist "${FAKE_ROOT}/dist" \
        --state "${FAKE_ROOT}/snap.state" >"${FAKE_ROOT}/cutover.log" 2>&1
}
rehearse_skip() { fcut "${SHA_NEW}" --rehearse --skip-ct-backup "$@"; }
# Отказ быстрого пути до первой записи: ни маркеров резерва, ни остановок, ни дампов.
no_writes_yet() {
    local why="$1"
    worker_stopped_ever && case_fail "${why}: worker stopped" || case_ok "${why}: worker untouched"
    grep -qE '^(vzdump|pg_dump) ' "${FAKE_ROOT}/state/calls.log" && case_fail "${why}: vzdump or pg_dump ran" || case_ok "${why}: no vzdump, no pg_dump"
    # Маркеры резерва снимаются при выходе, поэтому по остатку их не видно: проверяем сами команды ssh.
    if grep -q '\.reserved\.' "${FAKE_ROOT}/state/ssh-cmds.log" 2>/dev/null; then
        case_fail "${why}: a reservation command was sent to a host"
    else
        case_ok "${why}: zero reservation writes (no reserve command reached any host)"
    fi
}
# Печатаемая команда отката из лога cutover; run_printed исполняет её дословно из корня репозитория.
printed_rollback_cmd() { grep -m1 'scripts/deploy/rollback-db.sh' "${FAKE_ROOT}/cutover.log" | sed 's/^[[:space:]]*//'; }
run_printed() { RC=0; ( cd "${REPO_ROOT}" && eval "${RECOVERY_CMD}" ) >"${FAKE_ROOT}/rollback.log" 2>&1 || RC=$?; }
