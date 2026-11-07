#!/usr/bin/env bash
# Тесты retain.sh: план/удаление старых релизов, живые и current всегда целы,
# защита от странных путей, ротация дампов.
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

setup_retain() {
    setup_case "retain-$1"
    write_configs
    # По 5 релизов на компонент: rel1..rel5 (rel5 новее всех — трогаем по порядку).
    for i in 1 2 3 4 5; do
        deploy_release hub "00000000000${i}"
        deploy_release web "00000000000${i}"
        deploy_release worker "00000000000${i}"
        sleep 0.05
    done
    # Живой хаб поднимаем из rel1, затем возвращаем current на rel5: живой релиз
    # обязан быть старше трёх свежих и удержаться только как live.
    ln -sfn "releases/000000000001" "${HUB_CURRENT}"
    FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-hub >/dev/null
    ln -sfn "releases/000000000005" "${HUB_CURRENT}"
    FAKE_HOST=local "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-worker >/dev/null
    # current/build на rel5. Caddy обслуживает rel1 — тоже старше трёх свежих.
    printf '{"version":"000000000001"}' >"${FAKE_ROOT}/container/version.json"
    # Живой воркер-процесс из rel1 — СТАРШЕ трёх свежих: должен остаться только
    # потому, что из него живёт процесс.
    mkdir -p "${WORKER_RELEASES}/000000000001/bin"
    cp /bin/sleep "${WORKER_RELEASES}/000000000001/bin/live-proc"
    chmod +x "${WORKER_RELEASES}/000000000001/bin/live-proc"
    setsid "${WORKER_RELEASES}/000000000001/bin/live-proc" 1000 >/dev/null 2>&1 <&- &
    HARNESS_PIDS+=("$!")
    # Создание bin обновило mtime каталога: возвращаем rel1 в самый старый край.
    touch -d '2020-01-01 00:00:00' "${WORKER_RELEASES}/000000000001"
    # Дампы: 5 штук разной свежести.
    for i in 1 2 3 4 5; do
        printf 'dump-%s\n' "${i}" >"${HUB_BACKUP_DIR}/canterlot-pre-00000000000${i}.dump"
        sleep 0.05
    done
}

# R1: план — удалять rel1,rel2 (старые, не живые), держать остальное + дампы 4,5 + текущий.
case_begin "retain plan"
setup_retain plan
if bash "${DEPLOY_DIR}/retain.sh" >"${FAKE_ROOT}/retain.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/retain.log" "keep ${HUB_RELEASES}/000000000001 (live or current)" "hub rel1 kept (live, outside newest three)"
assert_contains "${FAKE_ROOT}/retain.log" "REMOVE ${HUB_RELEASES}/000000000002" "hub rel2 planned"
assert_contains "${FAKE_ROOT}/retain.log" "keep ${HUB_RELEASES}/000000000003" "hub rel3 kept (within newest three)"
assert_contains "${FAKE_ROOT}/retain.log" "keep ${HUB_RELEASES}/000000000004" "hub rel4 kept (within newest three)"
assert_contains "${FAKE_ROOT}/retain.log" "keep ${HUB_RELEASES}/000000000005" "hub rel5 kept (current)"
assert_contains "${FAKE_ROOT}/retain.log" "keep ${WEB_RELEASES}/000000000001 (live or current)" "web rel1 kept (served by caddy, outside newest three)"
assert_contains "${FAKE_ROOT}/retain.log" "REMOVE ${WEB_RELEASES}/000000000002" "web rel2 planned (old, not served)"
assert_contains "${FAKE_ROOT}/retain.log" "keep ${WORKER_RELEASES}/000000000001 (live or current)" "worker rel1 kept (live, outside newest three)"
assert_contains "${FAKE_ROOT}/retain.log" "REMOVE ${WORKER_RELEASES}/000000000002" "worker rel2 planned (old, not live)"
assert_contains "${FAKE_ROOT}/retain.log" "keep dump ${FAKE_ROOT}/hub/backups/canterlot-pre-000000000003.dump" "third-newest dump kept"
assert_contains "${FAKE_ROOT}/retain.log" "REMOVE ${FAKE_ROOT}/hub/backups/canterlot-pre-000000000002.dump" "dump 2 planned"
assert_contains "${FAKE_ROOT}/retain.log" "REMOVE ${FAKE_ROOT}/hub/backups/canterlot-pre-000000000001.dump" "old dump planned"
[ -d "${HUB_RELEASES}/000000000001" ] && case_ok "nothing deleted without --apply" || case_fail "deleted in plan mode"
teardown_case
case_end

# R2: --apply удаляет только старое, живые и current целы.
case_begin "retain apply"
setup_retain apply
if bash "${DEPLOY_DIR}/retain.sh" --apply >"${FAKE_ROOT}/retain.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/retain.log"
fi
[ -d "${HUB_RELEASES}/000000000001" ] && [ ! -e "${HUB_RELEASES}/000000000002" ] \
    && case_ok "live hub release kept, old hub release removed" || case_fail "live hub kept/deleted wrong"
[ -d "${HUB_RELEASES}/000000000003" ] && [ -d "${HUB_RELEASES}/000000000004" ] \
    && [ -d "${HUB_RELEASES}/000000000005" ] \
    && case_ok "three newest hub releases kept" || case_fail "newest hub releases not kept"
[ -d "${WEB_RELEASES}/000000000001" ] && [ ! -e "${WEB_RELEASES}/000000000002" ] \
    && case_ok "caddy-served web release kept, old web release removed" \
    || case_fail "caddy-served web release removed or old kept"
[ -d "${WORKER_RELEASES}/000000000001" ] && [ ! -e "${WORKER_RELEASES}/000000000002" ] \
    && case_ok "live worker release kept, old worker release removed" \
    || case_fail "live worker release removed or old worker kept"
[ -f "${HUB_BACKUP_DIR}/canterlot-pre-000000000005.dump" ] \
    && [ -f "${HUB_BACKUP_DIR}/canterlot-pre-000000000004.dump" ] \
    && [ -f "${HUB_BACKUP_DIR}/canterlot-pre-000000000003.dump" ] \
    && case_ok "3 newest dumps kept" || case_fail "dump kept wrong"
[ ! -e "${HUB_BACKUP_DIR}/canterlot-pre-000000000002.dump" ] \
    && [ ! -e "${HUB_BACKUP_DIR}/canterlot-pre-000000000001.dump" ] \
    && case_ok "old dumps removed" || case_fail "old dumps kept"
teardown_case
case_end

# R3: каталог с неправильным именем — громкий отказ, не молчаливый пропуск.
case_begin "retain odd path refused"
setup_retain oddpath
mkdir -p "${HUB_RELEASES}/waytoolongdirname"
touch -d '2020-01-01 00:00:00' "${HUB_RELEASES}/waytoolongdirname"
if bash "${DEPLOY_DIR}/retain.sh" --apply >"${FAKE_ROOT}/retain.log" 2>&1; then
    case_fail "exit 0 despite odd path"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/retain.log" "refusing odd path" "odd path refused loudly"
[ -d "${HUB_RELEASES}/waytoolongdirname" ] && case_ok "odd dir itself not removed" || case_fail "odd dir removed!"
teardown_case
case_end

# R4: финальная проверка current и удаление — одна удалённая команда под flock.
case_begin "retain locks the final check and deletion"
setup_retain lockapply
export FAKE_SSH_LOG_CMDS=1
if bash "${DEPLOY_DIR}/retain.sh" --apply >"${FAKE_ROOT}/retain.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/retain.log"
fi
unset FAKE_SSH_LOG_CMDS
HUB_DEL="$(awk -F'\t' '$1 == "fakehub" && $2 ~ /flock/ && $2 ~ /readlink/ && $2 ~ /rm -rf/ {print $2}' "${FAKE_ROOT}/state/ssh-cmds.log" | head -1)"
if [ -n "${HUB_DEL}" ] && [[ "${HUB_DEL}" == *"${HUB_BASE}/.deploy.lock"* ]]; then
    case_ok "hub check+delete runs under the deploy-root lock"
else
    case_fail "hub check+delete not under one locked invocation"
fi
WEB_DEL="$(awk -F'\t' '$1 == "fakeweb" && $2 ~ /flock/ && $2 ~ /docker exec/ && $2 ~ /rm -rf/ {print $2}' "${FAKE_ROOT}/state/ssh-cmds.log" | head -1)"
[ -n "${WEB_DEL}" ] \
    && case_ok "web guard+delete runs under one lock" || case_fail "web check+delete not locked"
[ -f "${WORKER_BASE}/.deploy.lock" ] \
    && case_ok "worker lock file exists" || case_fail "worker lock file missing"
teardown_case
case_end

# R5: настоящая конкуренция. Retain удаляет hub-релиз под flock (удаление замедлено
# подменённым rm и пишет маркер); ровно в этот момент конкурент берёт ТОТ ЖЕ lock и
# пытается сделать flip current на удаляемый релиз. Под lock конкурент ждёт конца
# удаления и получает отказ (цели нет), current не повисает. Без lock конкурент
# переключает current посреди удаления, и ссылка повисает: этот же сценарий на копии
# скриптов без flock обязан это показать — иначе тест ничего не доказывает.
run_race() {
    local scripts="$1" label="$2" rel _
    setup_retain "${label}"
    rm -f "${FAKE_ROOT}/state/rm-started" "${FAKE_ROOT}/state/comp-rc" "${FAKE_ROOT}/state/retain-rc"
    export FAKE_RM_DELAY=2 FAKE_RM_MATCH="${HUB_RELEASES}/"
    ( bash "${scripts}/retain.sh" --apply >"${FAKE_ROOT}/retain.log" 2>&1; echo $? >"${FAKE_ROOT}/state/retain-rc" ) &
    RET_BG=$!
    for _ in $(seq 1 200); do [ -s "${FAKE_ROOT}/state/rm-started" ] && break; command sleep 0.1; done
    RACE_MARKER=0
    if [ -s "${FAKE_ROOT}/state/rm-started" ]; then
        RACE_MARKER=1
        rel="$(basename "$(head -1 "${FAKE_ROOT}/state/rm-started")")"
        ( cd "${FAKE_ROOT}" && bash -c 'source "$1/lib.sh"; hub_locked "${HUB_FLIP_SCRIPT}" "${HUB_BASE}" "$2" "${HUB_CURRENT}"' _ "${scripts}" "${rel}" \
            >"${FAKE_ROOT}/comp.log" 2>&1; echo $? >"${FAKE_ROOT}/state/comp-rc" )
    fi
    wait "${RET_BG}" || true
    unset FAKE_RM_DELAY FAKE_RM_MATCH
    RACE_COMP_RC="$(cat "${FAKE_ROOT}/state/comp-rc" 2>/dev/null || echo none)"
    if [ -e "$(readlink -m "${HUB_CURRENT}")" ]; then RACE_RESOLVES=1; else RACE_RESOLVES=0; fi
}

case_begin "retain locking holds under real contention"
run_race "${DEPLOY_DIR}" lockedrace
[ "${RACE_MARKER}" = "1" ] && case_ok "the competitor started inside retain's critical section" || case_fail "retention never reached the delete (no marker)"
[ "${RACE_COMP_RC}" != "none" ] && case_ok "the competing flip actually ran (rc ${RACE_COMP_RC})" || case_fail "the competing flip never ran"
[ "${RACE_COMP_RC}" != "0" ] && case_ok "competing flip was refused after the deletion (serialised by the lock)" || case_fail "competing flip succeeded concurrently with the delete"
[ "${RACE_RESOLVES}" = "1" ] && case_ok "hub current still resolves" || case_fail "hub current dangles (target deleted)"
[ -d "${HUB_RELEASES}/000000000005" ] && case_ok "previous current kept" || case_fail "previous current deleted"
teardown_case
case_end

case_begin "the race test detects removed locking"
MUT="$(mktemp -d /tmp/canterlot-deploy-mut.XXXXXX)"
cp "${DEPLOY_DIR}"/*.sh "${DEPLOY_DIR}"/*.py "${DEPLOY_DIR}/deploy.conf" "${MUT}/"
cat >>"${MUT}/lib.sh" <<'EOF'
# МУТАЦИЯ ТЕСТА: lock убран везде.
locked_cmd_text() { local lock="$1" script="$2"; shift 2; printf 'bash -c %s --%s' "$(shq "${script}")" "$(_quote_args "$@")"; }
worker_locked() { local script="$1"; shift; bash -c "${script}" -- "$@"; }
EOF
run_race "${MUT}" unlockedrace
rm -rf "${MUT}"
[ "${RACE_MARKER}" = "1" ] && case_ok "mutant: the competitor still started inside the critical section" || case_fail "mutant: no marker"
[ "${RACE_COMP_RC}" = "0" ] && case_ok "mutant: the competing flip succeeded mid-delete" || case_fail "mutant: competing flip rc ${RACE_COMP_RC}"
[ "${RACE_RESOLVES}" = "0" ] && case_ok "mutant: current dangles, so the lock test is meaningful" || case_fail "TEST IS VACUOUS: current resolves even without locking"
teardown_case
case_end

# R6: имя релиза с shell-метасимволами не попадает в удалённую команду:
# раньше 12-символьное имя вставлялось в shell и исполнялось.
case_begin "retain refuses shell metacharacters in release names"
setup_retain metachars
mkdir -p "${HUB_RELEASES}/';>PWN;#xxxx"
touch -d '2020-01-01 00:00:00' "${HUB_RELEASES}/';>PWN;#xxxx"
if ( cd "${FAKE_ROOT}" && bash "${DEPLOY_DIR}/retain.sh" --apply ) >"${FAKE_ROOT}/retain.log" 2>&1; then
    case_fail "exit 0 despite a metacharacter release name"
else
    case_ok "exit nonzero"
fi
assert_contains "${FAKE_ROOT}/retain.log" "refusing odd path" "odd release dir refused"
[ ! -e "${FAKE_ROOT}/PWN" ] && case_ok "no injected file created" || case_fail "shell injection executed"
[ -d "${HUB_RELEASES}/';>PWN;#xxxx" ] && case_ok "odd dir itself not removed" || case_fail "odd dir removed!"
teardown_case
case_end

# R7: релиз, зарезервированный идущей операцией, не удаляется; остальное чистится.
case_begin "retain honours a reservation file"
setup_retain reserved
: >"${HUB_BASE}/.reserved.000000000002"
: >"${WEB_SITES_BASE}/.reserved.000000000002"
: >"${WORKER_BASE}/.reserved.000000000002"
if bash "${DEPLOY_DIR}/retain.sh" --apply >"${FAKE_ROOT}/retain.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/retain.log"
fi
[ -d "${HUB_RELEASES}/000000000002" ] && [ -d "${WEB_RELEASES}/000000000002" ] && [ -d "${WORKER_RELEASES}/000000000002" ] \
    && case_ok "reserved release kept on all three hosts" || case_fail "reserved release was removed"
assert_contains "${FAKE_ROOT}/retain.log" "reserved by a running operation" "skip reported"
assert_not_contains "${FAKE_ROOT}/retain.log" "removed ${HUB_RELEASES}/000000000002" "not reported as removed"
[ ! -e "${HUB_BACKUP_DIR}/canterlot-pre-000000000001.dump" ] && case_ok "unrelated dump rotation still happens" || case_fail "dump rotation stopped"
rm -f "${HUB_BASE}/.reserved.000000000002"
bash "${DEPLOY_DIR}/retain.sh" --apply >"${FAKE_ROOT}/retain2.log" 2>&1 || case_fail "second retain failed"
[ ! -e "${HUB_RELEASES}/000000000002" ] && case_ok "without the reservation the release is removed" || case_fail "release kept without reservation"
teardown_case
case_end

printf 'retain harness: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
