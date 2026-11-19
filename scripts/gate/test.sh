#!/usr/bin/env bash
# Быстрые проверки аргументов и предусловий scripts/gate.sh,
# scripts/gate-remote.sh и scripts/gate/in-container.sh: без сети, без docker
# и без прогона гейта. Полный прогон проверяет scripts/gate-remote.sh --local.
set -euo pipefail
export LC_ALL=C

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
gate=scripts/gate-remote.sh
# Литеральный путь: тест удаляет только свой каталог и только с меткой.
tmp=/tmp/canterlot-gate-test
if [[ -e "$tmp" ]]; then
    [[ -f "$tmp/.canterlot-gate-test" ]] || {
        echo "FATAL: $tmp занят не этим тестом" >&2
        exit 1
    }
    rm -rf /tmp/canterlot-gate-test
fi
mkdir -p /tmp/canterlot-gate-test
: >/tmp/canterlot-gate-test/.canterlot-gate-test
trap 'rm -rf /tmp/canterlot-gate-test' EXIT

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
out="$tmp/log"
run() {
    local want=$1
    shift
    local got=0
    "$@" >"$out" 2>&1 || got=$?
    [[ "$got" == "$want" ]] || { cat "$out" >&2; fail "ожидался код $want, получен $got: $*"; }
}

run 2 bash "$gate" --bogus
grep -q 'Использование' "$out" || fail "нет подсказки при неизвестном аргументе"
run 0 bash "$gate" --help
grep -q 'Использование' "$out" || fail "--help не печатает подсказку"

run 1 bash "$gate" nosuchrev --local
grep -q 'нет такого коммита' "$out" || { cat "$out" >&2; fail "нет отказа для отсутствующего коммита"; }

sha12=$(git rev-parse --short=12 HEAD)

# Существующий каталог результата отказывается до обращения к docker: CANTERLOT_DOCKER
# заведомо несуществующий, и сообщение должно быть именно про каталог.
mkdir -p "$tmp/out/$sha12"
: >"$tmp/out/$sha12/marker"
run 1 env CANTERLOT_GATE_OUT="$tmp/out" CANTERLOT_DOCKER=/nonexistent/canterlot-docker \
    bash "$gate" HEAD --local
grep -q 'уже существует' "$out" || { cat "$out" >&2; fail "нет отказа для существующего каталога"; }

# --local без docker: пустой каталог результата, docker подменён на несуществующий путь.
mkdir -p "$tmp/out-empty"
run 1 env CANTERLOT_GATE_OUT="$tmp/out-empty" CANTERLOT_DOCKER=/nonexistent/canterlot-docker \
    bash "$gate" HEAD --local
grep -q 'нет docker' "$out" || { cat "$out" >&2; fail "нет отказа без docker"; }

# Аргументы in-container.sh проверяются до базы, npm и docker.
mkdir -p "$tmp/src" "$tmp/outdir" "$tmp/full-out"
: >"$tmp/full-out/marker"
run 2 bash scripts/gate/in-container.sh
run 1 bash scripts/gate/in-container.sh --src "$tmp/src" --out "$tmp/outdir" --sha not-a-sha
grep -q 'не sha12' "$out" || { cat "$out" >&2; fail "нет отказа для неверного --sha"; }
run 1 bash scripts/gate/in-container.sh --src "$tmp/missing" --out "$tmp/outdir" \
    --sha "$sha12"
grep -q 'нет каталога исходников' "$out" || { cat "$out" >&2; fail "нет отказа без --src"; }
run 1 bash scripts/gate/in-container.sh --src "$tmp/src" --out "$tmp/full-out" \
    --sha "$sha12"
grep -q 'не пуст' "$out" || { cat "$out" >&2; fail "нет отказа для непустого --out"; }

# --- scripts/gate.sh: отказы и аренда без прогона проверок ---
# Гейт проверяет чистоту копии до аренды, а аренду — до npm ci: это проверяется
# в пустом репозитории, без единого реального шага.
scratch="$tmp/scratch"
mkdir -p "$scratch/scripts"
cp scripts/gate.sh scripts/lease.sh "$scratch/scripts/"
chmod 0755 "$scratch/scripts/gate.sh"
(
    cd "$scratch"
    git init -q
    git config user.email gate@example.invalid
    git config user.name gate
    git config commit.gpgsign false
    : >tracked
    git add tracked scripts
    git commit -qm init
)
: >"$scratch/dirty"
run 1 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh"
grep -q -- '--no-attest' "$out" || { cat "$out" >&2; fail 'грязная копия: нет подсказки --no-attest'; }
[[ ! -e "$scratch/.superpowers" ]] || fail 'грязная копия успела создать .superpowers'
run 0 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh" --no-attest
grep -q 'kind=gate' "$out" || { cat "$out" >&2; fail '--no-attest не дошёл до аренды'; }
rm -f "$scratch/dirty"
run 0 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh"
grep -q 'kind=gate' "$out" || { cat "$out" >&2; fail 'чистая копия не дошла до аренды'; }
run 2 bash "$scratch/scripts/gate.sh" --bogus
run 0 bash "$scratch/scripts/gate.sh" --help

# --- машиночитаемое состояние полного прогона ---
# Шаги подменены короткими командами: они пишут те же отчёты, что настоящие шаги,
# и Notes в журнал вызовов, чтобы тест видел, что именно перезапускалось.
new_gate_repo() {
    local dir=$1
    mkdir -p "$dir/scripts"
    cp scripts/gate.sh scripts/lease.sh "$dir/scripts/"
    chmod 0755 "$dir/scripts/gate.sh"
    (
        cd "$dir"
        git init -q
        git config user.email gate@example.invalid
        git config user.name gate
        git config commit.gpgsign false
        printf '.superpowers/\ntarget/\n' >.gitignore
        : >tracked
        git add .gitignore tracked scripts
        git commit -qm init
    )
}
g2="$tmp/g2"
new_gate_repo "$g2"
export GATE_FAKE_CALLS="$tmp/g2-calls"
: >"$GATE_FAKE_CALLS"
# shellcheck disable=SC2016 # подмены исполняются через bash -c, где переменные раскрываются
fake_npm='echo "npm $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 0'
# shellcheck disable=SC2016
fake_check='mkdir -p target/nextest/default; printf "%s\n" "<testsuites><testsuite name=\"crate-a\"><testcase name=\"mod::flaky\"><failure/></testcase><testcase name=\"mod::ok\"/></testsuite></testsuites>" >target/nextest/default/junit.xml; echo "check $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 1'
# shellcheck disable=SC2016
fake_deploy='echo "deploy $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 0'
# shellcheck disable=SC2016
fake_web='echo "web $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 0'
gate2_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=0
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="$fake_check"
    CANTERLOT_GATE_STEP_DEPLOY="$fake_deploy" CANTERLOT_GATE_STEP_WEB="$fake_web"
)
run 1 "${gate2_env[@]}" bash "$g2/scripts/gate.sh" --no-attest
short=$(git -C "$g2" rev-parse --short=8 HEAD)
state="$g2/.superpowers/gate/$short/state.tsv"
[[ -f "$state" ]] || fail 'нет машиночитаемого состояния прогона'
grep -qF $'schema\t1' "$state" || fail 'нет schema в состоянии'
grep -qF "$(printf 'sha\t%s' "$(git -C "$g2" rev-parse HEAD)")" "$state" || fail 'нет полного sha в состоянии'
grep -qF $'completed\t1' "$state" || fail 'нет признака завершения в состоянии'
grep -qF $'npm_ci_exit\t0' "$state" || fail 'нет кода npm ci в состоянии'
grep -qF $'check_exit\t1' "$state" || fail 'нет кода check в состоянии'
grep -qF $'check_failed\ttests' "$state" || fail 'падение check не отнесено к тестам'
grep -qF $'rust_fail\trust|crate-a|mod::flaky' "$state" || fail 'нет идентичности упавшего Rust-теста'
grep -qF $'deploy_exit\tskipped' "$state" || fail 'нет пропущенного deploy в состоянии'
if grep -qF $'rust_fail\trust|crate-a|mod::ok' "$state"; then fail 'прошедший тест попал в упавшие'; fi

# --- перезапуск только упавших тестов ---
# Подмена check пишет junit с указанным FAKE_CHECK_FAIL (или без падений, если none).
# shellcheck disable=SC2016
fake_check_rerun='
echo "check $GATE_STEP_MODE [$GATE_STEP_SELECTION]" >>"$GATE_FAKE_CALLS"
mkdir -p target/nextest/default
if [[ "$FAKE_CHECK_FAIL" == none ]]; then
  printf "%s\n" "<testsuites><testsuite name=\"crate-a\"><testcase name=\"mod::flaky\"/></testsuite></testsuites>" >target/nextest/default/junit.xml
  exit 0
fi
printf "%s\n" "<testsuites><testsuite name=\"crate-a\"><testcase name=\"$FAKE_CHECK_FAIL\"><failure/></testcase></testsuite></testsuites>" >target/nextest/default/junit.xml
exit 1
'
rerun_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=0
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="$fake_check_rerun"
    CANTERLOT_GATE_STEP_DEPLOY="$fake_deploy" CANTERLOT_GATE_STEP_WEB="$fake_web"
)
# Первый прогон в g2 уже оставил состояние с упавшим mod::flaky. Перезапуск должен
# тронуть только check, с точным фильтром, и принять тест, который прошёл во второй раз.
export FAKE_CHECK_FAIL=none
: >"$GATE_FAKE_CALLS"
run 0 "${rerun_env[@]}" bash "$g2/scripts/gate.sh" --rerun-failed --no-attest
[[ $(grep -c '^check ' "$GATE_FAKE_CALLS") == 1 ]] || fail 'перезапуск не вызвал check'
if grep -qE '^(npm|deploy|web) ' "$GATE_FAKE_CALLS"; then fail 'перезапуск задел прошедшие шаги'; fi
grep -qF 'check tests [' "$GATE_FAKE_CALLS" || fail 'check вызван не в режиме tests'
grep -qF 'binary_id(=crate-a) & test(=mod::flaky)' "$GATE_FAKE_CALLS" \
    || fail 'в выборку не попал точный фильтр упавшего теста'
grep -qF $'mode\trerun' "$state" || fail 'режим rerun не записан'
grep -qF $'check_exit\t0' "$state" || fail 'успешный перезапуск не дал check_exit=0'
grep -qF $'flaky\trust|crate-a|mod::flaky' "$state" || fail 'нет отметки нестабильного теста'
grep -qF $'rerun_done\trust|crate-a|mod::flaky' "$state" || fail 'нет отметки использованного перезапуска'
started_full=$(awk -F '\t' '$1=="started"{print $2}' "$state")
grep -qF "$(printf 'rerun_of\t%s' "$started_full")" "$state" || fail 'rerun_of не сохранил время первого прогона'

# Второй прогон того же теста падает: гейт обязан провалиться и запретить ещё один перезапуск.
g3="$tmp/g3"
new_gate_repo "$g3"
export FAKE_CHECK_FAIL=mod::flaky
run 1 "${rerun_env[@]}" bash "$g3/scripts/gate.sh" --no-attest
short3=$(git -C "$g3" rev-parse --short=8 HEAD)
state3="$g3/.superpowers/gate/$short3/state.tsv"
# Перезапуск того же теста, который падает снова, — настоящий провал гейта.
run 1 "${rerun_env[@]}" bash "$g3/scripts/gate.sh" --rerun-failed --no-attest
grep -qF $'rerun_done\trust|crate-a|mod::flaky' "$state3" || fail 'после второго падения нет отметки перезапуска'
grep -qF $'flaky\trust|crate-a|mod::flaky' "$state3" && fail 'повторно упавший тест помечен нестабильным'
: >"$GATE_FAKE_CALLS"
run 1 "${rerun_env[@]}" bash "$g3/scripts/gate.sh" --rerun-failed --no-attest
grep -q 'уже перезапускался' "$out" || { cat "$out" >&2; fail 'нет отказа на повторный перезапуск теста'; }
[[ ! -s "$GATE_FAKE_CALLS" ]] || fail 'после отказа шаги всё же вызывались'

# Перезапуск возможен только для того же коммита и чистого дерева.
printf 'next\n' >>"$g3/tracked"
git -C "$g3" add tracked && git -C "$g3" commit -qm next
run 1 "${rerun_env[@]}" bash "$g3/scripts/gate.sh" --rerun-failed --no-attest
grep -q 'нет состояния предыдущего' "$out" || { cat "$out" >&2; fail 'нет отказа для другого sha'; }
g4="$tmp/g4"
new_gate_repo "$g4"
run 1 "${rerun_env[@]}" bash "$g4/scripts/gate.sh" --no-attest
printf 'грязно\n' >"$g4/stray"
run 1 "${rerun_env[@]}" bash "$g4/scripts/gate.sh" --rerun-failed --no-attest
grep -q 'грязная' "$out" || { cat "$out" >&2; fail 'нет отказа для грязного дерева'; }
export FAKE_CHECK_FAIL=none

# --- аттестация после перезапуска: прежние ключи плюс rerun_of и flaky ---
g5="$tmp/g5"
new_gate_repo "$g5"
export FAKE_CHECK_FAIL=mod::flaky
run 1 "${rerun_env[@]}" bash "$g5/scripts/gate.sh"
g5short=$(git -C "$g5" rev-parse --short=8 HEAD)
first_started=$(awk -F '\t' '$1=="started"{print $2}' "$g5/.superpowers/gate/$g5short/state.tsv")
export FAKE_CHECK_FAIL=none
run 0 "${rerun_env[@]}" bash "$g5/scripts/gate.sh" --rerun-failed
att="$g5/.superpowers/gate/$(git -C "$g5" rev-parse HEAD).ok"
[[ -s "$att" ]] || fail 'перезапуск не записал аттестацию'
for key in sha started finished duration_s npm_ci_exit check_exit deploy_exit web_exit logs; do
    grep -q "^$key=" "$att" || fail "в аттестации нет ключа $key"
done
grep -q '^npm_ci_exit=0$' "$att" || fail 'аттестация: npm_ci_exit не обнулился'
grep -q '^check_exit=0$' "$att" || fail 'аттестация: check_exit не обнулился'
grep -q "^rerun_of=$first_started$" "$att" || fail 'аттестация: нет rerun_of времени первого прогона'
grep -qF 'flaky=rust|crate-a|mod::flaky' "$att" || fail 'аттестация: нет строки flaky'
grep -q '^test_seams=1$' "$att" || fail 'аттестация с тестовыми швами не помечена test_seams=1'
grep -q 'ВНИМАНИЕ: включены тестовые швы' "$out" || fail 'нет громкого предупреждения о тестовых швах'
grep -qF $'seams\t1' "$state" || fail 'в состоянии нет отметки о тестовых швах'
grep -q 'отчёт о нестабильных тестах' "$out" || fail 'нет отчёта о нестабильных тестах'
grep -q 'flaky-tests.md' "$out" || fail 'нет напоминания про flaky-tests.md'

# --- идентичности наборов выката, Playwright и сквозных сценариев ---
# shellcheck disable=SC2016
fake_check_pass='echo "check $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 0'
# shellcheck disable=SC2016
fake_deploy_fail='
echo "deploy $GATE_STEP_MODE [$GATE_STEP_SELECTION]" >>"$GATE_FAKE_CALLS"
mkdir -p "$RUN_LOG_DIR"
for t in changed release; do
  if [[ "$FAKE_DEPLOY_FAIL" == "$t" ]]; then printf "1 5\n" >"$RUN_LOG_DIR/$t.rc"; else printf "0 5\n" >"$RUN_LOG_DIR/$t.rc"; fi
done
[[ "$FAKE_DEPLOY_FAIL" == none ]]
'
# shellcheck disable=SC2016
fake_web_fail='
echo "web $GATE_STEP_MODE [$GATE_STEP_SELECTION]" >>"$GATE_FAKE_CALLS"
pw="${CANTERLOT_PW_JSON_REPORT}.json"
st="${CANTERLOT_STACK_JSON_REPORT}-1.json"
mkdir -p "${pw%/*}"
if [[ "$FAKE_WEB_FAIL" == none ]]; then
  SELECTION="$GATE_STEP_SELECTION" PW_REPORT="$pw" ST_REPORT="$st" MISSING="${FAKE_WEB_MISSING:-}" python3 - <<PY
import json, os
missing = os.environ["MISSING"]
pw_specs, st_specs = [], []
for line in os.environ["SELECTION"].splitlines():
    parts = line.split("|")
    if not parts:
        continue
    if parts[0] == "pw" and missing != "pw":
        project, loc, full = parts[1], parts[2], parts[3]
        file, _, lineno = loc.rpartition(":")
        head, sep, tail = full.partition(" › ")
        pw_specs.append({"file": file, "line": int(lineno), "title": head, "tests": [{"projectName": project, "title": tail, "status": "expected", "results": [{"status": "passed"}]}]})
    elif parts[0] == "stack" and missing != "stack":
        loc, full = parts[1], parts[2]
        file, _, lineno = loc.rpartition(":")
        head, sep, tail = full.partition(" › ")
        st_specs.append({"file": file, "line": int(lineno), "title": head, "tests": [{"title": tail, "status": "expected", "results": [{"status": "passed"}]}]})
def report(specs):
    return {"suites": [{"specs": specs}]}
with open(os.environ["PW_REPORT"], "w") as f:
    json.dump(report(pw_specs), f)
with open(os.environ["ST_REPORT"], "w") as f:
    json.dump(report(st_specs), f)
PY
  exit 0
fi
printf "%s\n" "{\"suites\":[{\"specs\":[{\"file\":\"e2e/ui/x.spec.ts\",\"line\":42,\"title\":\"x spec\",\"tests\":[{\"projectName\":\"phone\",\"title\":\"flaky title\",\"results\":[{\"status\":\"failed\"}]}]},{\"file\":\"e2e/ui/z.spec.ts\",\"line\":9,\"title\":\"z spec\",\"tests\":[{\"projectName\":\"phone\",\"title\":\"second title\",\"results\":[{\"status\":\"failed\"}]}]}]}]}" >"$pw"
printf "%s\n" "{\"suites\":[{\"specs\":[{\"file\":\"e2e/stack/y.spec.ts\",\"line\":7,\"title\":\"stack spec\",\"tests\":[{\"title\":\"stack title\",\"results\":[{\"status\":\"timedOut\"}]}]}]}]}" >"$st"
exit 1
'
web_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=1
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="$fake_check_pass"
    CANTERLOT_GATE_STEP_DEPLOY="$fake_deploy_fail" CANTERLOT_GATE_STEP_WEB="$fake_web_fail"
)
g6="$tmp/g6"
new_gate_repo "$g6"
export FAKE_DEPLOY_FAIL=release FAKE_WEB_FAIL=fail
run 1 "${web_env[@]}" bash "$g6/scripts/gate.sh" --no-attest
g6short=$(git -C "$g6" rev-parse --short=8 HEAD)
state6="$g6/.superpowers/gate/$g6short/state.tsv"
grep -qF $'deploy_fail\tdeploy|release' "$state6" || fail 'нет идентичности упавшего набора выката'
grep -qF $'pw_fail\tpw|phone|e2e/ui/x.spec.ts:42|x spec › flaky title' "$state6" || fail 'нет идентичности Playwright из JSON'
grep -qF $'pw_fail\tpw|phone|e2e/ui/z.spec.ts:9|z spec › second title' "$state6" || fail 'нет второй идентичности Playwright из JSON'
grep -qF $'stack_fail\tstack|e2e/stack/y.spec.ts:7|stack spec › stack title' "$state6" || fail 'нет идентичности сценария из JSON'
grep -qF $'deploy_failed\ttests' "$state6" || fail 'падение deploy не отнесено к тестам'
grep -qF $'web_failed\ttests' "$state6" || fail 'падение web не отнесено к тестам'
export FAKE_DEPLOY_FAIL=none FAKE_WEB_FAIL=none
: >"$GATE_FAKE_CALLS"
run 0 "${web_env[@]}" bash "$g6/scripts/gate.sh" --rerun-failed --no-attest
if grep -qE '^(npm|check) ' "$GATE_FAKE_CALLS"; then fail 'перезапуск задел прошедшие шаги npm/check'; fi
grep -qF 'deploy tests [release]' "$GATE_FAKE_CALLS" || fail 'deploy не перезапустил только упавший набор'
[[ $(grep -c '^web ' "$GATE_FAKE_CALLS") == 1 ]] || fail 'Playwright одного проекта не сгруппирован в один вызов'
grep -qF 'pw|phone|e2e/ui/x.spec.ts:42' "$GATE_FAKE_CALLS" || fail 'web не получил селектор file:line'
grep -qF 'pw|phone|e2e/ui/z.spec.ts:9' "$GATE_FAKE_CALLS" || fail 'второй селектор не попал в ту же группу'
grep -qF 'stack|e2e/stack/y.spec.ts:7' "$GATE_FAKE_CALLS" || fail 'web не получил идентичность сценария'
if grep -qF -- '--grep' "$GATE_FAKE_CALLS"; then fail 'перезапуск снова выбрал тесты по заголовку через --grep'; fi
for id in 'deploy|release' 'pw|phone|e2e/ui/x.spec.ts:42|x spec › flaky title' \
          'pw|phone|e2e/ui/z.spec.ts:9|z spec › second title' 'stack|e2e/stack/y.spec.ts:7|stack spec › stack title'; do
    grep -qF "$(printf 'flaky\t%s' "$id")" "$state6" || fail "нет отметки flaky для $id"
done

# Если отчёт перезапуска не подтвердил прохождение идентичности, гейт обязан
# провалиться, а не счесть её нестабильной и прошедшей.
g7="$tmp/g7"
new_gate_repo "$g7"
export FAKE_DEPLOY_FAIL=none FAKE_WEB_FAIL=fail
run 1 "${web_env[@]}" bash "$g7/scripts/gate.sh" --no-attest
export FAKE_WEB_FAIL=none FAKE_WEB_MISSING=stack
run 1 "${web_env[@]}" bash "$g7/scripts/gate.sh" --rerun-failed --no-attest
grep -q 'не подтвердил прохождение' "$out" || { cat "$out" >&2; fail 'нет отказа из-за неподтверждённой идентичности'; }
grep -qF 'stack|e2e/stack/y.spec.ts:7' "$out" || fail 'в отказе не названа missing-идентичность'
grep -qF $'rerun_done\tstack|e2e/stack/y.spec.ts:7|stack spec › stack title' \
    "$g7/.superpowers/gate/$(git -C "$g7" rev-parse --short=8 HEAD)/state.tsv" \
    || fail 'неподтверждённая идентичность не попала в rerun_done'
unset FAKE_WEB_MISSING

# Настоящий web_rerun: упавшие тесты в двух проектах Playwright. Вызовы по проектам
# не должны писать отчёт в один файл, иначе второй затирает первый (гейт 1.42.0).
g8="$tmp/g8"
new_gate_repo "$g8"
mkdir -p "$g8/web" "$tmp/fake-bin"
: >"$g8/web/package-lock.json"
cp scripts/gate/fake-check-web.sh "$g8/scripts/check-web.sh"
cp scripts/gate/fake-npx "$tmp/fake-bin/npx"
chmod 0755 "$tmp/fake-bin/npx"
git -C "$g8" add web/package-lock.json scripts/check-web.sh
git -C "$g8" commit -qm web
web_real_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=0 PORT=1 PATH="$tmp/fake-bin:$PATH"
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="$fake_check_pass"
)
: >"$GATE_FAKE_CALLS"
run 1 "${web_real_env[@]}" bash "$g8/scripts/gate.sh" --no-attest
run 0 "${web_real_env[@]}" bash "$g8/scripts/gate.sh" --rerun-failed --no-attest
[[ $(grep -c '^npx ' "$GATE_FAKE_CALLS") == 2 ]] || fail 'web_rerun не вызвал Playwright по разу на проект'
grep -qF 'npx phone ui/x.spec.ts:42' "$GATE_FAKE_CALLS" || fail 'web_rerun не перезапустил тест телефона'
grep -qF 'npx desktop ui/w.spec.ts:5' "$GATE_FAKE_CALLS" || fail 'web_rerun не перезапустил тест десктопа'

# Упал UI, а сквозные сценарии не запускались (отчёта стека нет): перезапуск одних
# упавших UI-тестов дал бы аттестацию без стека, поэтому повторяется весь веб.
g9="$tmp/g9"
new_gate_repo "$g9"
mkdir -p "$g9/web"
: >"$g9/web/package-lock.json"
cp scripts/gate/fake-check-web.sh "$g9/scripts/check-web.sh"
git -C "$g9" add web/package-lock.json scripts/check-web.sh
git -C "$g9" commit -qm web
: >"$GATE_FAKE_CALLS"
run 1 env FAKE_NO_STACK=1 "${web_real_env[@]}" bash "$g9/scripts/gate.sh" --no-attest
state9="$g9/.superpowers/gate/$(git -C "$g9" rev-parse --short=8 HEAD)/state.tsv"
grep -qF $'web_failed\tother' "$state9" || fail 'падение UI без отчёта стека отнесено к тестам'
grep -q 'сквозные сценарии не запускались' "$out" || fail 'нет предупреждения о незапущенном стеке'
: >"$GATE_FAKE_CALLS"
run 1 env FAKE_NO_STACK=1 "${web_real_env[@]}" bash "$g9/scripts/gate.sh" --rerun-failed --no-attest
grep -qF 'check-web --stack' "$GATE_FAKE_CALLS" || fail 'перезапуск без отчёта стека не повторил весь веб'
if grep -q '^npx ' "$GATE_FAKE_CALLS"; then fail 'перезапуск без отчёта стека перезапустил одни UI-тесты'; fi

# Битый индекс: git status падает, и проверка чистоты обязана провалиться, а не
# пропустить аттестацию из-за пустого вывода.
printf 'garbage' >"$scratch/.git/index"
run 1 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh"
grep -q 'грязная' "$out" || { cat "$out" >&2; fail 'битый индекс не остановил гейт'; }

# Выбор упавших тестов по file:line проверяем на настоящих спеках без браузера.
if [[ -x web/node_modules/.bin/playwright ]]; then
    bash scripts/gate/test-pw-selection.sh || fail 'выбор Playwright по file:line'
else
    printf 'выбор Playwright по file:line: пропущен, нет web/node_modules\n'
fi

if command -v shellcheck >/dev/null 2>&1; then
    shellcheck scripts/gate.sh scripts/gate-coverage.sh scripts/gate/test-pw-selection.sh scripts/gate-remote.sh \
        scripts/gate/in-container.sh scripts/gate/test.sh scripts/gate/fake-npx scripts/gate/fake-check-web.sh \
        || fail "shellcheck"
fi

printf 'все быстрые проверки прошли\n'
