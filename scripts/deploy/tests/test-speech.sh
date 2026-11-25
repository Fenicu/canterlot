#!/usr/bin/env bash
# Служба диктовки canterlot-speech в наборе выката. Диктовка не в ядре, и любой её сбой мягкий,
# поэтому прогонов cutover.sh здесь три, а остальные ветки идут прямым вызовом той же функции:
# 1) счастливый путь (рестарт после живого воркера, health от имени хаба), затем на том же
#    стенде speech_sync напрямую: рестарт дольше предела, сокета нет, нет speech.toml, юнит
#    выключен руками, юнита нет (и verify), health висит — ожидание ограничено бюджетом;
# 2) сбои службы мягкие: dry-run и репетиция печатают шаги и не теряют токен, выкат с
#    failing health и WARN-видами движка в журналах службы и хаба доходит до done;
# 3) цель без бинаря: stage только сообщает, config-check speech.toml по примеру из
#    docs/configuration.md и по плохим файлам, мягкий preflight, репетиция печатает стоп и
#    выключение, выкат останавливает и выключает юнит;
# 4) rollback-db ведёт юнит за релизом: без бинаря — стоп и выключение, с бинарём — рестарт.
# Отчёт STOP-AND-REPORT с состоянием юнита проверяет test-cutover.sh (кейс NRestarts).
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=cutover-fixture.sh
source "${HARNESS_DIR}/cutover-fixture.sh"

SPEECH_ST=""
setup_speech() {
    setup_cutover "speech-$1"
    SPEECH_ST="${FAKE_ROOT}/state/unit-fakehub-canterlot-speech"
}
# speech_sync для релиза $2 (по умолчанию SHA_NEW) в режиме $1 (real|rehearse); вывод и
# итог SUMMARY=… — в sync.log.
sync_direct() {
    # shellcheck disable=SC2016 # переменные раскрывает внутренний bash
    DEPLOY_MODE="$1" bash -c 'source "$1/lib.sh"; speech_sync "$2"; printf "SUMMARY=%s\n" "${SPEECH_SUMMARY}"' \
        _ "${DEPLOY_DIR}" "${2:-${SHA_NEW}}" >"${FAKE_ROOT}/sync.log" 2>&1
}
speech_restarts() { grep -c 'systemctl fakehub restart canterlot-speech' "${FAKE_ROOT}/state/calls.log" || true; }
HUB_WARN_LINE() {
    printf 'Nov 03 12:00:05 canterlot-hub canterlot-hub[101]: 2026-11-03T12:00:05.000000Z  WARN %s kind="%s"' "$1" "$2"
}
SPEECH_LINE() {
    printf 'Nov 03 12:00:06 canterlot-hub canterlot-speech[303]: 2026-11-03T12:00:06.000000Z  %s %s kind="%s"' "$1" "$2" "$3"
}

# --- 1. счастливый путь: рестарт после старта воркера, health от имени хаба ---
case_begin "speech restarted and healthy"
setup_speech happy
install_fake_speech "${SHA_PREV}"
install_fake_speech "${SHA_NEW}"
install_speech_unit
export FAKE_SPEECH_JOURNAL_LINES
FAKE_SPEECH_JOURNAL_LINES="$(SPEECH_LINE INFO "speech service started" speech_started)"
if run_cutover; then case_ok "exit 0"; else case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/cutover.log"; fi
unset FAKE_SPEECH_JOURNAL_LINES
L="${FAKE_ROOT}/cutover.log"
assert_eq "$(speech_restarts)" "1" "exactly one speech restart"
# Рестарт службы — после доказанного старта воркера: простой воркера он не удлиняет.
W="$(grep -n 'recorded session PIDs and ticks unchanged after worker start' "${L}" | cut -d: -f1 | head -1)"
S="$(grep -n 'restarting canterlot-speech (bounded to 30 s)' "${L}" | cut -d: -f1 | head -1)"
[ -n "${W}" ] && [ -n "${S}" ] && [ "${S}" -gt "${W}" ] && case_ok "speech restart after the worker is live" || case_fail "speech restart order (worker ${W:-none}, speech ${S:-none})"
assert_contains "${L}" "speech: health ok:" "health ok after the restart"
assert_contains "${L}" "speech: health ok as canterlot:" "verify asked health as the hub user"
assert_contains "${FAKE_ROOT}/state/calls.log" "canterlot-speech health --socket /run/canterlot-speech/speech.sock user=canterlot" "local CLI over the socket, as the hub user"
assert_contains "${L}" "speech journal: WARN/ERROR kinds: none; speech_unavailable (INFO, expected): 0" "speech journal read"
assert_contains "${L}" ", speech: ok (speech soft issues 0)" "summary: speech ok"
assert_contains "${L}" "verify PASS" "verify passed"
assert_not_contains "${L}" "SOFT-FAIL" "no soft failures"
assert_eq "$(speech_state_is)" "ActiveState=active UnitFileState=enabled" "unit active and enabled"
# Остальные ветки speech_sync — та же функция, которую cutover зовёт после старта
# воркера (порядок и итоговую строку проверяет прогон выше), напрямую на этом стенде.
: >"${FAKE_ROOT}/state/calls.log"
# shellcheck disable=SC2016 # переменная раскрывается во внутреннем bash
assert_eq "$(env -u SPEECH_RESTART_TIMEOUT bash -c 'source "$1/deploy.conf"; printf %s "${SPEECH_RESTART_TIMEOUT}"' _ "${DEPLOY_DIR}")" "30" "default restart bound is 30 s"
# Рестарт дольше предела: боевой предел 30 с, здесь 1 с, старт службы «висит» 3 с.
START="$(date +%s)"
SPEECH_RESTART_TIMEOUT=1 FAKE_SPEECH_START_DELAY=3 sync_direct real
S="${FAKE_ROOT}/sync.log"
assert_contains "${S}" "SPEECH-SOFT: canterlot-speech restart timed out after 1 s" "restart timeout reported"
assert_contains "${S}" "SUMMARY=restart timed out after 1 s" "summary names the timeout"
[ $(( $(date +%s) - START )) -lt 30 ] && case_ok "not stuck on the speech restart" || case_fail "speech restart not bounded"
# Сокета нет (служба не слушает): строка speech_unavailable, мягко.
echo unavailable >"${FAKE_ROOT}/state/speech-health"
sync_direct real
assert_contains "${S}" "health failed after 1 s: INFO kind=\"speech_unavailable\"" "speech_unavailable line shown"
assert_contains "${S}" "SUMMARY=health failing" "unavailable socket: health failing"
rm -f "${FAKE_ROOT}/state/speech-health"
# Нет speech.toml: служба не тронута.
mv "${SPEECH_CONFIG}" "${SPEECH_CONFIG}.aside"
: >"${FAKE_ROOT}/state/calls.log"
sync_direct real
assert_contains "${S}" "speech: dictation unavailable: no ${SPEECH_CONFIG}; canterlot-speech not touched" "no config: unavailable"
assert_eq "$(speech_restarts)" "0" "no restart without a config"
mv "${SPEECH_CONFIG}.aside" "${SPEECH_CONFIG}"
# Юнит выключен руками: остаётся как его оставил владелец.
rm -f "${SPEECH_ST}.enabled"
echo 0 >"${SPEECH_ST}.active"
sync_direct real
assert_contains "${S}" "dictation switched off by hand: canterlot-speech is disabled" "disabled unit respected"
assert_eq "$(speech_restarts)" "0" "disabled unit not restarted"
assert_eq "$(speech_state_is)" "ActiveState=inactive UnitFileState=disabled" "unit left as the owner set it"
# Юнита нет: диктовка недоступна, это не сбой ни в cutover, ни в verify.
rm -f "${SPEECH_ST}.installed"
sync_direct real
assert_contains "${S}" "speech: dictation unavailable: unit canterlot-speech is not installed; canterlot-speech not touched" "no unit: unavailable (cutover)"
assert_not_contains "${S}" "SPEECH-SOFT" "no unit: no speech soft issue"
bash "${DEPLOY_DIR}/verify.sh" "${SHA_NEW}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/verify.log" 2>&1 || true
assert_contains "${FAKE_ROOT}/verify.log" "speech: dictation unavailable: unit canterlot-speech is not installed" "no unit: unavailable (verify)"
assert_not_contains "${FAKE_ROOT}/verify.log" "SPEECH-SOFT" "no unit: verify has no speech soft issue"
assert_eq "$(speech_restarts)" "0" "no unit: no speech restart"
grep -q 'canterlot-speech health' "${FAKE_ROOT}/state/calls.log" && case_fail "health asked without a unit" || case_ok "health not asked without a unit"
# health висит: ожидание ограничено бюджетом по часам, а не числом попыток.
BIN="${HUB_RELEASES}/${SHA_NEW}/bin/canterlot-speech"
printf '#!/usr/bin/env bash\nsleep 30\n' >"${BIN}"
START="$(date +%s)"
# shellcheck disable=SC2016 # переменные раскрывает внутренний bash
if SPEECH_HEALTH_WAIT=2 bash -c 'source "$1/lib.sh"; speech_wait_health "$2"' _ "${DEPLOY_DIR}" "${BIN}"; then
    case_fail "hanging health reported ok"
else
    case_ok "hanging health reports failure"
fi
ELAPSED=$(( $(date +%s) - START ))
[ "${ELAPSED}" -le 5 ] && case_ok "wait bounded by the 2 s budget (${ELAPSED} s)" || case_fail "wait took ${ELAPSED} s for a 2 s budget"
teardown_case
case_end

# --- 2. сбои службы мягкие: репетиция с токеном, выкат до done, диктовка считается отдельно ---
case_begin "speech failures are soft: failing health and engine WARN kinds keep the token and the release"
setup_speech soft
install_fake_speech "${SHA_PREV}"
install_fake_speech "${SHA_NEW}"
install_speech_unit
echo fail >"${FAKE_ROOT}/state/speech-health"
: >"${FAKE_ROOT}/state/calls.log"
bash "${DEPLOY_DIR}/cutover.sh" "${SHA_NEW}" --dry-run --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" \
    >"${FAKE_ROOT}/dry.log" 2>&1 || true
D="${FAKE_ROOT}/dry.log"
assert_contains "${D}" "dry-run: speech: canterlot-speech installed and enabled, ${SPEECH_CONFIG} present, release ${SHA_NEW} has the binary; the real run restarts canterlot-speech after the worker is live (bounded to 30 s) and asks health (soft)" "dry-run names the speech action"
assert_contains "${D}" "- speech: restart canterlot-speech (bounded to 30 s) + health, or stop and disable it" "NOT RUN list has the speech step"
assert_contains "${D}" "9 speech: restart canterlot-speech (bounded to 30 s)" "plan has the speech step"
assert_eq "$(speech_restarts)" "0" "dry-run restarts nothing"
if bash "${DEPLOY_DIR}/cutover.sh" "${SHA_NEW}" --rehearse --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" \
        --state "${FAKE_ROOT}/rehearse.state" >"${FAKE_ROOT}/rehearse.log" 2>&1; then
    case_ok "rehearsal PASS despite failing speech health"
else
    case_fail "rehearsal failed"; tail -8 "${FAKE_ROOT}/rehearse.log"
fi
RL="${FAKE_ROOT}/rehearse.log"
assert_contains "${RL}" "(restart canterlot-speech):" "WOULD-WRITE printed for the speech restart"
assert_contains "${RL}" "SPEECH-SOFT: canterlot-speech health failed" "health failure reported in the rehearsal"
assert_contains "${RL}" "rehearse: speech: health failing" "rehearsal summary names speech"
grep -q '^  CONFIRM=' "${RL}" && case_ok "token printed (speech never blocks)" || case_fail "no token"
assert_eq "$(speech_restarts)" "0" "rehearsal restarts nothing"
export FAKE_SPEECH_JOURNAL_LINES FAKE_HUB_JOURNAL_LINES
FAKE_SPEECH_JOURNAL_LINES="$(SPEECH_LINE WARN "engine load failed" engine_load_failed)
$(SPEECH_LINE WARN "engine exited" engine_exited)
$(SPEECH_LINE WARN "engine exited" engine_exited)
$(SPEECH_LINE WARN "socket bind failed" socket_bind_failed)
$(SPEECH_LINE INFO "speech unavailable" speech_unavailable)"
FAKE_HUB_JOURNAL_LINES="$(HUB_WARN_LINE "dictation engine failed" speech_engine_failed)
$(HUB_WARN_LINE "dictation protocol error" speech_protocol_error)"
if run_cutover; then case_ok "exit 0 (soft)"; else case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/cutover.log"; fi
unset FAKE_SPEECH_JOURNAL_LINES FAKE_HUB_JOURNAL_LINES
L="${FAKE_ROOT}/cutover.log"
assert_contains "${L}" "SPEECH-SOFT: canterlot-speech health failed after 1 s: {\"ok\":false" "health failure reported after the restart"
assert_contains "${L}" "SPEECH-SOFT: canterlot-speech health failed as canterlot:" "health failure reported by verify"
assert_contains "${L}" "verify PASS" "verify passes with failing health and speech WARN kinds"
assert_not_contains "${L}" "HARD-FAIL:" "no hard failure"
assert_contains "${L}" "SPEECH-SOFT: canterlot-speech journal: 1 WARN engine_load_failed in 30 min (engine failure, expected kind)" "engine_load_failed soft"
assert_contains "${L}" "SPEECH-SOFT: canterlot-speech journal: 2 WARN engine_exited in 30 min (engine failure, expected kind)" "engine_exited soft"
assert_contains "${L}" "SPEECH-SOFT: canterlot-speech journal: 1 WARN socket_bind_failed in 30 min (NEW kind)" "unexpected speech kind still soft"
assert_contains "${L}" "SPEECH-SOFT: hub journal: 1 WARN speech_engine_failed in 30 min (speech engine failure)" "speech_engine_failed in the hub journal is soft"
assert_contains "${L}" "SPEECH-SOFT: hub journal: 1 WARN speech_protocol_error in 30 min (speech engine failure)" "speech_protocol_error in the hub journal is soft"
assert_contains "${L}" "speech_unavailable (INFO, expected): 1" "speech_unavailable is INFO and expected"
assert_not_contains "${L}" "new hub WARN kind not in the baseline: speech_engine_failed" "speech_engine_failed is not a new hard kind"
assert_contains "${L}" "soft failures 0, speech: health failing" "summary: release soft failures untouched, speech failing"
# Файл результата считает диктовку отдельно: публикации она не мешает.
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" status)" "done" "result file: done"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" soft_failures)" "0" "result file: no non-speech soft failures"
[ "$(result_of "${FAKE_ROOT}/cutover.result" speech_soft_failures)" -ge 2 ] \
    && case_ok "result file: speech soft failures counted separately (cutover and verify)" \
    || case_fail "result file: speech soft failures $(result_of "${FAKE_ROOT}/cutover.result" speech_soft_failures)"
assert_eq "$(result_of "${FAKE_ROOT}/cutover.result" speech_summary)" "health failing" "result file: speech summary"
hub_is "${SHA_NEW}" && worker_is "${SHA_NEW}" && case_ok "release on new despite speech" || case_fail "release not on new"
# Список мягких WARN-видов хаба покрывает все speech_* из исходников (статическая сверка).
ALLOW="$(bash -c 'source "$1/lib.sh"; printf "%s" "${SPEECH_WARN_KINDS_SOFT}"' _ "${DEPLOY_DIR}")"
export ALLOW REPO_ROOT
if python3 - <<'PY'
import os
import pathlib
import re
import sys

allow = set(os.environ["ALLOW"].split())
kinds = set()
for path in pathlib.Path(os.environ["REPO_ROOT"], "crates").rglob("*.rs"):
    text = path.read_text(encoding="utf-8", errors="replace")
    for call in re.finditer(r"tracing::warn!\(", text):
        end = text.find(");", call.end())
        body = text[call.end() : end if end >= 0 else len(text)]
        kinds.update(re.findall(r'kind\s*=\s*"(speech_[^"]+)"', body))
if not kinds:
    print("no speech_* WARN kinds found in crates/*.rs")
    sys.exit(1)
missing = sorted(kinds - allow)
if missing:
    print("speech WARN kinds missing from SPEECH_WARN_KINDS_SOFT: " + " ".join(missing))
    sys.exit(1)
print("speech WARN kinds covered: " + " ".join(sorted(kinds)))
PY
then
    case_ok "every speech_* WARN kind is in SPEECH_WARN_KINDS_SOFT"
else
    case_fail "a speech_* WARN kind is missing from SPEECH_WARN_KINDS_SOFT"
fi
unset ALLOW
teardown_case
case_end

# --- 3. в целевом релизе нет бинаря: stage только сообщает, cutover останавливает и
# выключает юнит; speech.toml сверяется со строгостью службы, в preflight — мягко ---
setup_speech_stage() {
    setup_case "speech-stage-$1"
    DIST="${FAKE_ROOT}/dist/${SHA_NEW}"
    mkdir -p "${DIST}/hub/bin" "${DIST}/worker/bin" "${DIST}/web/_app"
    # shellcheck disable=SC2016 # format string is intentional
    printf '#!/bin/sh\nif [ "$1" = openapi ]; then cat "%s"; else exit 99; fi\n' "${REPO_ROOT}/docs/api/openapi.json" >"${DIST}/hub/bin/canterlot-hub"
    chmod +x "${DIST}/hub/bin/canterlot-hub"
    if [ "$1" = "with" ]; then printf '#!/bin/sh\nexit 0\n' >"${DIST}/hub/bin/canterlot-speech"; chmod +x "${DIST}/hub/bin/canterlot-speech"; fi
    echo fakeworker >"${DIST}/worker/bin/canterlot-worker"
    echo '<html></html>' >"${DIST}/web/200.html"
    printf '{"version":"%s"}' "${SHA_NEW}" >"${DIST}/web/_app/version.json"
    printf 'sha=%s\ndate=x\nmigrations_last=0040_monthly_limits.sql\n' "${SHA_NEW}" >"${DIST}/RELEASE"
    (cd "${DIST}" && find . -type f ! -name 'SHA256SUMS*' -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)
    deploy_release hub "${SHA_PREV}"
    deploy_release web "${SHA_PREV}"
    deploy_release worker "${SHA_PREV}"
}
case_begin "target release without canterlot-speech: stage reports it, the cutover stops and disables the unit, speech.toml checks are soft"
setup_speech_stage with
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_ok "stage exit 0"
else
    case_fail "stage exit nonzero"; tail -5 "${FAKE_ROOT}/stage.log"
fi
[ -x "${HUB_RELEASES}/${SHA_NEW}/bin/canterlot-speech" ] && case_ok "canterlot-speech staged with the hub" || case_fail "canterlot-speech not staged"
assert_contains "${FAKE_ROOT}/stage.log" "hub ${SHA_NEW}: bin/canterlot-speech staged (dictation)" "staged binary reported"
teardown_case
setup_speech_stage without
if bash "${DEPLOY_DIR}/stage.sh" "${SHA_NEW}" --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/stage.log" 2>&1; then
    case_ok "stage exit 0 without canterlot-speech"
else
    case_fail "stage failed without canterlot-speech"; tail -5 "${FAKE_ROOT}/stage.log"
fi
assert_contains "${FAKE_ROOT}/stage.log" "WARNING: release ${SHA_NEW} has no hub/bin/canterlot-speech: dictation will be unavailable on this release (not an error)" "missing binary reported, not failed"
teardown_case
# speech.toml: пример из docs/configuration.md принят, плохие файлы отвергнуты строгостью службы.
setup_speech nobinary
CHECK=(python3 "${DEPLOY_DIR}/config-check.py" --require --hex64 model.sha256 --hex64 vad.sha256
    "${REPO_ROOT}/crates/canterlot-speech/src/config.rs" Config)
HEX="$(printf 'a%.0s' $(seq 64))"
awk '/^## Служба распознавания речи \(speech.toml\)/ { s = 1 } s && /^```toml/ { f = 1; next } f && /^```/ { exit } f' \
    "${REPO_ROOT}/docs/configuration.md" | sed "s/<64 шестнадцатеричных символа>/${HEX}/" >"${FAKE_ROOT}/good.toml"
grep -q '^\[limits\]' "${FAKE_ROOT}/good.toml" && grep -q '^\[live\]' "${FAKE_ROOT}/good.toml" \
    && case_ok "doc example extracted" || case_fail "doc example not found"
if "${CHECK[@]}" "${FAKE_ROOT}/good.toml" >"${FAKE_ROOT}/good.log" 2>&1; then case_ok "doc example accepted"; else case_fail "doc example rejected"; cat "${FAKE_ROOT}/good.log"; fi
printf '[model]\npath = "m.gguf"\nsha256 = "%s"\n[vad]\npath = "v.gguf"\nsha256 = "%s"\n' "${HEX}" "${HEX}" >"${FAKE_ROOT}/minimal.toml"
"${CHECK[@]}" "${FAKE_ROOT}/minimal.toml" >/dev/null 2>&1 && case_ok "minimal config (defaults) accepted" || case_fail "minimal config rejected"
sed 's/^threads = 4 .*/thread = 4/' "${FAKE_ROOT}/good.toml" >"${FAKE_ROOT}/unknown.toml"
sed '/^\[vad\]/,/^$/{/^sha256/d}' "${FAKE_ROOT}/good.toml" >"${FAKE_ROOT}/nosha.toml"
sed "0,/^sha256 = \"${HEX}\"/s//sha256 = \"abc\"/" "${FAKE_ROOT}/good.toml" >"${FAKE_ROOT}/shortsha.toml"
printf '[model]\npath = "m.gguf"\nsha256 = "%s"\n' "${HEX}" >"${FAKE_ROOT}/novad.toml"
for bad in "unknown:unknown key 'thread'" "nosha:missing required key 'vad.sha256'" \
        "shortsha:key 'model.sha256' must be 64 hexadecimal characters" "novad:missing required key 'vad'"; do
    name="${bad%%:*}"; want="${bad#*:}"
    if "${CHECK[@]}" "${FAKE_ROOT}/${name}.toml" >"${FAKE_ROOT}/${name}.log" 2>&1; then
        case_fail "${name}.toml accepted"
    else
        case_ok "${name}.toml rejected"
    fi
    assert_contains "${FAKE_ROOT}/${name}.log" "${want}" "${name}: ${want}"
done
assert_not_contains "${FAKE_ROOT}/shortsha.log" "abc" "values are never printed"
# preflight на этом стенде: нет speech.toml — нормально, плохой — мягко с именами ключей.
preflight_speech() {
    bash "${DEPLOY_DIR}/preflight.sh" "${SHA_NEW}" --source "${REPO_ROOT}" --dist "${FAKE_ROOT}/dist" >"${FAKE_ROOT}/pf.log" 2>&1
}
if preflight_speech; then case_ok "preflight PASS without speech.toml"; else case_fail "preflight failed"; tail -5 "${FAKE_ROOT}/pf.log"; fi
assert_contains "${FAKE_ROOT}/pf.log" "speech.toml: no ${SPEECH_CONFIG} on the hub, dictation off (fine)" "absence is fine"
printf 'socket = "/run/x.sock"\nbogus = 1\n[model]\npath = "m.gguf"\nsha256 = "zz"\n' >"${SPEECH_CONFIG}"
if preflight_speech; then case_ok "preflight PASS with a bad speech.toml (soft)"; else case_fail "preflight failed on speech.toml"; tail -5 "${FAKE_ROOT}/pf.log"; fi
assert_contains "${FAKE_ROOT}/pf.log" "unknown key 'bogus'" "unknown key named"
assert_contains "${FAKE_ROOT}/pf.log" "missing required key 'vad'" "missing table named"
assert_contains "${FAKE_ROOT}/pf.log" "SPEECH-SOFT: ${SPEECH_CONFIG} does not match the new canterlot-speech Config" "soft issue reported"
# Дальше — выкат на этот же стенд: юнит установлен, в цели бинаря нет.
install_fake_speech "${SHA_PREV}"
install_speech_unit
# Репетиция печатает стоп и выключение, но не делает их.
sync_direct rehearse
assert_contains "${FAKE_ROOT}/sync.log" "(stop and disable canterlot-speech):" "WOULD-WRITE printed for stop+disable"
assert_eq "$(speech_state_is)" "ActiveState=active UnitFileState=enabled" "rehearsal left the unit alone"
if run_cutover; then case_ok "exit 0 (dictation never blocks)"; else case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/cutover.log"; fi
L="${FAKE_ROOT}/cutover.log"
hub_is "${SHA_NEW}" && case_ok "hub on new" || case_fail "hub not new"
worker_is "${SHA_NEW}" && case_ok "worker on new" || case_fail "worker not new"
assert_contains "${L}" "release ${SHA_NEW} has no bin/canterlot-speech; stopping and disabling canterlot-speech" "stop+disable announced"
assert_contains "${L}" "speech: canterlot-speech stopped and disabled" "stop+disable read back"
assert_eq "$(speech_state_is)" "ActiveState=inactive UnitFileState=disabled" "unit inactive and disabled"
assert_eq "$(speech_restarts)" "0" "no restart into a missing binary"
assert_contains "${L}" "soft failures 0, speech: dictation unavailable: release ${SHA_NEW} has no bin/canterlot-speech; unit stopped and disabled" "summary names the state"
assert_not_contains "${L}" "SPEECH-SOFT" "nothing failing is left"
assert_contains "${L}" "speech.toml: OK against the new canterlot-speech Config" "preflight checked speech.toml"
teardown_case
case_end

# --- 4. rollback-db: юнит следует за релизом хаба; без бинаря — [dictation] до старта старого хаба ---
setup_speech_rollback() {
    setup_case "speech-rollback-$1"
    write_configs
    deploy_release hub "${SHA_PREV}"
    deploy_release hub "${SHA_NEW}"
    ln -sfn "releases/${SHA_NEW}" "${HUB_CURRENT}"
    deploy_release worker "${SHA_PREV}"
    export FAKE_READYZ_MODE=byhub FAKE_READYZ_NEW_SHA="${SHA_NEW}"
    FAKE_HOST=fakehub "${FAKE_ROOT}/fake-bin/systemctl" start canterlot-hub >/dev/null
    printf 'MIGR_MAX=41\nFAILED=0\npayload-new\n' >"${FAKE_ROOT}/state/db.txt"
    printf 'TOC-LINE-01\nTOC-LINE-02 _sqlx_migrations table\nTOC-LINE-03\nTOC-LINE-04\nTOC-LINE-05\n' >"${FAKE_ROOT}/hub/backups/can-pre.dump"
    printf 'MIGR_MAX=40\nFAILED=0\npayload-predump\n' >>"${FAKE_ROOT}/hub/backups/can-pre.dump"
    install_fake_speech "${SHA_NEW}"
    install_speech_unit
}
case_begin "rollback-db leads the speech unit to the release: stop and disable without the binary, restart with it"
setup_speech_rollback withbinary
install_fake_speech "${SHA_PREV}"
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "exit 0 (release with the binary)"
else
    case_fail "exit nonzero (release with the binary)"; tail -5 "${FAKE_ROOT}/rb.log"
fi
assert_eq "$(speech_restarts)" "1" "speech restarted once on the old release"
assert_contains "${FAKE_ROOT}/rb.log" "speech: ok (speech soft issues 0)" "report: speech ok"
assert_eq "$(speech_state_is)" "ActiveState=active UnitFileState=enabled" "unit active and enabled"
teardown_case
setup_speech_rollback nobinary
if bash "${DEPLOY_DIR}/rollback-db.sh" "${FAKE_ROOT}/hub/backups/can-pre.dump" --to "${SHA_PREV}" >"${FAKE_ROOT}/rb.log" 2>&1; then
    case_ok "exit 0"
else
    case_fail "exit nonzero"; tail -5 "${FAKE_ROOT}/rb.log"
fi
assert_contains "${FAKE_ROOT}/rb.log" "remove [dictation] from" "[dictation] reminder before the restart"
assert_contains "${FAKE_ROOT}/rb.log" "release ${SHA_PREV} has no bin/canterlot-speech; stopping and disabling canterlot-speech" "stop+disable announced"
assert_eq "$(speech_state_is)" "ActiveState=inactive UnitFileState=disabled" "unit stopped and disabled"
assert_contains "${FAKE_ROOT}/rb.log" "rollback-db done: hub on ${SHA_PREV}, worker stays stopped (start it by hand); speech: dictation unavailable" "report names the speech state"
teardown_case
case_end

printf 'speech: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
