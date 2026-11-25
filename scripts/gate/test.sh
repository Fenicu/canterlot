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

# --runner prefer — значение по умолчанию: без фикстуры конфига раннеров нет, гейт
# идёт локально. Тесты не должны читать настоящий конфиг воркера владельца.
export CANTERLOT_WORKER_CONFIG="$tmp/worker.toml"

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

# in-container.sh чистит /tmp: вне контейнера это /tmp машины. Отказ обязан стоять до чистки —
# и по метке контейнера (её ставят только gate-remote.sh и runner-gate.sh), и по чужому HEAD.
# Порядок сверяется по тексту всегда; само поведение — только в отдельном пространстве
# монтирования с tmpfs поверх /tmp, чтобы регрессия не снесла настоящий /tmp.
line_of() { grep -n -m1 -- "$1" scripts/gate/in-container.sh | cut -d: -f1; }
wipe_line=$(line_of '^find /tmp ')
mark_line=$(line_of 'CANTERLOT_GATE_CONTAINER')
head_line=$(line_of 'rev-parse --short=12 HEAD')
[[ -n "$wipe_line" ]] || fail 'in-container.sh: не найдена чистка /tmp (тест устарел)'
[[ -n "$mark_line" && "$mark_line" -lt "$wipe_line" ]] || fail 'in-container.sh: метка контейнера не проверяется до чистки /tmp'
[[ -n "$head_line" && "$head_line" -lt "$wipe_line" ]] || fail 'in-container.sh: HEAD сверяется после чистки /tmp'
if unshare -rm true 2>/dev/null; then
    # shellcheck disable=SC2016 # раскрывается во внутреннем bash
    in_ns='
set -u
mount -t tmpfs tmpfs /tmp || { echo NS_FAILED; exit 0; }
mkdir /tmp/out
: >/tmp/sentinel
rc=0
bash scripts/gate/in-container.sh --src "$PWD" --out /tmp/out --sha "$1" || rc=$?
echo "RC=$rc"
[ -e /tmp/sentinel ] && echo SENTINEL_KEPT
exit 0
'
    run 0 env -u CANTERLOT_GATE_CONTAINER unshare -rm bash -c "$in_ns" ns "$sha12"
    if ! grep -q NS_FAILED "$out"; then
        grep -q 'не в контейнере' "$out" || { cat "$out" >&2; fail 'in-container.sh вне контейнера не отказал'; }
        grep -qx 'RC=1' "$out" || { cat "$out" >&2; fail 'in-container.sh вне контейнера: код не 1'; }
        grep -qx SENTINEL_KEPT "$out" || { cat "$out" >&2; fail 'in-container.sh вне контейнера вычистил /tmp'; }
        # Метка есть, но HEAD рабочей копии не тот, что назван: отказ тоже до чистки.
        run 0 env CANTERLOT_GATE_CONTAINER=1 unshare -rm bash -c "$in_ns" ns 000000000000
        grep -q 'HEAD в .* не 000000000000' "$out" || { cat "$out" >&2; fail 'in-container.sh не отказал на чужом HEAD'; }
        grep -qx SENTINEL_KEPT "$out" || { cat "$out" >&2; fail 'in-container.sh на чужом HEAD вычистил /tmp до сверки'; }
    fi
else
    printf 'пропуск: unshare -rm недоступен, чистка /tmp проверена только по тексту\n'
fi

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
# С --runner local по-прежнему отказ: перезапуск требует состояние ровно этого HEAD.
# При prefer без состояния вместо отказа был бы полный гейт, поэтому здесь явно local.
run 1 "${rerun_env[@]}" bash "$g3/scripts/gate.sh" --runner local --rerun-failed --no-attest
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

# shellcheck disable=SC2016
fake_check_pass_early='echo "check $GATE_STEP_MODE" >>"$GATE_FAKE_CALLS"; exit 0'

# --- грязное дерево не отмывается перезапуском ---
# Шаг check «проверяет» рабочую копию: проходит, только когда в src.txt исправленное
# содержимое. Закоммичено сломанное, поэтому на чистом коммите check обязан упасть.
# shellcheck disable=SC2016
fake_check_tree='echo "check ran on: $(cat src.txt)" >>"$GATE_FAKE_CALLS"; grep -qx fixed src.txt'
tree_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=0
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="$fake_check_tree"
    CANTERLOT_GATE_STEP_DEPLOY="$fake_deploy" CANTERLOT_GATE_STEP_WEB="$fake_web"
)
new_tree_repo() {
    new_gate_repo "$1"
    printf 'broken\n' >"$1/src.txt"
    git -C "$1" add src.txt
    git -C "$1" commit -qm src
}
gd="$tmp/gd"
new_tree_repo "$gd"
gdsha=$(git -C "$gd" rev-parse HEAD)
gdstate="$gd/.superpowers/gate/${gdsha:0:8}/state.tsv"
gdatt="$gd/.superpowers/gate/$gdsha.ok"
# Проход --no-attest с незакоммиченной правкой зелёный; состояние помнит, что дерево было грязным.
printf 'fixed\n' >"$gd/src.txt"
run 0 "${tree_env[@]}" bash "$gd/scripts/gate.sh" --runner local --no-attest
grep -qF $'tree\tdirty' "$gdstate" || { cat "$gdstate" >&2; fail 'состояние прохода на грязном дереве не помечено tree=dirty'; }
# Правку убрали: HEAD тот же, дерево чистое. Перезапуск не вправе аттестовать коммит по шагам,
# которые прошли на другом содержимом.
git -C "$gd" checkout -q -- src.txt
: >"$GATE_FAKE_CALLS"
run 1 "${tree_env[@]}" bash "$gd/scripts/gate.sh" --runner local --rerun-failed
grep -q 'грязном дереве' "$out" || { cat "$out" >&2; fail 'нет отказа перезапуску после прохода на грязном дереве'; }
[[ ! -e "$gdatt" ]] || fail 'перезапуск аттестовал коммит по шагам, пройденным на грязном дереве'
[[ ! -s "$GATE_FAKE_CALLS" ]] || fail 'после отказа шаги всё же вызывались'
# Перезапуск без аттестации разрешён, но метку не снимает: следующий перезапуск с аттестацией
# отказывает так же.
run 0 "${tree_env[@]}" bash "$gd/scripts/gate.sh" --runner local --rerun-failed --no-attest
grep -qF $'tree\tdirty' "$gdstate" || fail 'перезапуск --no-attest снял метку грязного дерева'
run 1 "${tree_env[@]}" bash "$gd/scripts/gate.sh" --runner local --rerun-failed
[[ ! -e "$gdatt" ]] || fail 'метка грязного дерева отмыта перезапуском --no-attest'
# Полный гейт на чистом коммите гоняет шаги заново и падает на сломанном содержимом.
run 1 "${tree_env[@]}" bash "$gd/scripts/gate.sh" --runner local
grep -qF 'check ran on: broken' "$GATE_FAKE_CALLS" || fail 'полный гейт не проверил закоммиченное содержимое'
grep -qF $'tree\tclean' "$gdstate" || fail 'проход на чистом дереве не помечен tree=clean'
[[ ! -e "$gdatt" ]] || fail 'аттестация записана при упавшем check'

# Дерево стало грязным за проход с аттестацией: аттестации нет, и перезапуск после уборки
# её тоже не даёт.
# shellcheck disable=SC2016
fake_check_dirties='echo "check dirties" >>"$GATE_FAKE_CALLS"; : >stray; exit 0'
ge="$tmp/ge"
new_gate_repo "$ge"
gesha=$(git -C "$ge" rev-parse HEAD)
run 1 "${tree_env[@]}" CANTERLOT_GATE_STEP_CHECK="$fake_check_dirties" bash "$ge/scripts/gate.sh" --runner local
grep -q 'стала грязной' "$out" || { cat "$out" >&2; fail 'нет отказа в аттестации, когда дерево стало грязным за проход'; }
grep -qF $'tree\tdirty' "$ge/.superpowers/gate/${gesha:0:8}/state.tsv" || fail 'дерево, ставшее грязным за проход, не помечено tree=dirty'
rm -f "$ge/stray"
run 1 "${tree_env[@]}" CANTERLOT_GATE_STEP_CHECK="$fake_check_dirties" bash "$ge/scripts/gate.sh" --runner local --rerun-failed
[[ ! -e "$ge/.superpowers/gate/$gesha.ok" ]] || fail 'перезапуск аттестовал проход, за который дерево стало грязным'

# Состояние старого формата (без ключа tree) чистоты не доказывает: с аттестацией — отказ.
gf="$tmp/gf"
new_gate_repo "$gf"
gfsha=$(git -C "$gf" rev-parse HEAD)
run 0 "${tree_env[@]}" CANTERLOT_GATE_STEP_CHECK="$fake_check_pass_early" bash "$gf/scripts/gate.sh" --runner local
gfstate="$gf/.superpowers/gate/${gfsha:0:8}/state.tsv"
grep -qF $'tree\tclean' "$gfstate" || fail 'зелёный проход на чистом дереве не помечен tree=clean'
rm -f "$gf/.superpowers/gate/$gfsha.ok"
# Состояние чистого прохода перезапуск принимает, как раньше.
run 0 "${tree_env[@]}" CANTERLOT_GATE_STEP_CHECK="$fake_check_pass_early" bash "$gf/scripts/gate.sh" --runner local --rerun-failed
[[ -s "$gf/.superpowers/gate/$gfsha.ok" ]] || fail 'перезапуск после чистого прохода не записал аттестацию'
rm -f "$gf/.superpowers/gate/$gfsha.ok"
awk -F '\t' '$1!="tree"' "$gfstate" >"$tmp/gf-state-old"
cp "$tmp/gf-state-old" "$gfstate"
run 1 "${tree_env[@]}" CANTERLOT_GATE_STEP_CHECK="$fake_check_pass_early" bash "$gf/scripts/gate.sh" --runner local --rerun-failed
grep -q 'грязном дереве' "$out" || { cat "$out" >&2; fail 'состояние без ключа tree принято перезапуском с аттестацией'; }
[[ ! -e "$gf/.superpowers/gate/$gfsha.ok" ]] || fail 'перезапуск аттестовал состояние без признака чистого дерева'

# --- идентичности наборов выката ---
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
[[ "$FAKE_WEB_FAIL" == none ]]
'
web_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=1
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="$fake_check_pass"
    CANTERLOT_GATE_STEP_DEPLOY="$fake_deploy_fail" CANTERLOT_GATE_STEP_WEB="$fake_web_fail"
)
g6="$tmp/g6"
new_gate_repo "$g6"
export FAKE_DEPLOY_FAIL=release FAKE_WEB_FAIL=none
run 1 "${web_env[@]}" bash "$g6/scripts/gate.sh" --no-attest
g6short=$(git -C "$g6" rev-parse --short=8 HEAD)
state6="$g6/.superpowers/gate/$g6short/state.tsv"
grep -qF $'deploy_fail\tdeploy|release' "$state6" || fail 'нет идентичности упавшего набора выката'
grep -qF $'deploy_failed\ttests' "$state6" || fail 'падение deploy не отнесено к тестам'
export FAKE_DEPLOY_FAIL=none
: >"$GATE_FAKE_CALLS"
run 0 "${web_env[@]}" bash "$g6/scripts/gate.sh" --rerun-failed --no-attest
if grep -qE '^(npm|check|web) ' "$GATE_FAKE_CALLS"; then fail 'перезапуск задел прошедшие шаги npm/check/web'; fi
grep -qF 'deploy tests [release]' "$GATE_FAKE_CALLS" || fail 'deploy не перезапустил только упавший набор'
grep -qF "$(printf 'flaky\tdeploy|release')" "$state6" || fail 'нет отметки flaky для deploy|release'

# Падение веба без идентичностей тестов: браузерных тестов больше нет, поэтому
# такой провал не «тесты» — гейт повторяет весь веб-шаг, а не отдельные тесты.
g8="$tmp/g8"
new_gate_repo "$g8"
cp scripts/gate/fake-check-web.sh "$g8/scripts/check-web.sh"
git -C "$g8" add scripts/check-web.sh
git -C "$g8" commit -qm web
web_real_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=0
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="$fake_check_pass"
)
: >"$GATE_FAKE_CALLS"
run 1 "${web_real_env[@]}" bash "$g8/scripts/gate.sh" --no-attest
state8="$g8/.superpowers/gate/$(git -C "$g8" rev-parse --short=8 HEAD)/state.tsv"
grep -qF $'web_failed\tother' "$state8" || fail 'падение веба без идентичностей отнесено к тестам'
: >"$GATE_FAKE_CALLS"
run 1 "${web_real_env[@]}" bash "$g8/scripts/gate.sh" --rerun-failed --no-attest
grep -qF 'check-web full' "$GATE_FAKE_CALLS" || fail 'перезапуск не повторил весь веб'
if grep -qE '^(npm|check) ' "$GATE_FAKE_CALLS"; then fail 'перезапуск задел прошедшие шаги npm/check'; fi

# --- логи проходов, причины падений и отчёт о нестабильных тестах ---
# Подмена check печатает вывод nextest, как он ложится в check.log: заголовок FAIL, блоки
# stdout/stderr и итоговые блоки `--- STDERR:`. Следом она, как check.sh --all, прогоняет
# canterlot-speech с тем же профилем и перезаписывает junit.xml зелёным отчётом: упавшие
# тесты воркспейса остаются только в check.log. FAKE_CHECK_FAIL — имена упавших тестов
# через пробел (none — всё зелёно); mod::long печатает 16 строк паники.
fake_nextest="$tmp/fake-nextest-check.sh"
cat >"$fake_nextest" <<'FAKE'
#!/usr/bin/env bash
echo "check $GATE_STEP_MODE [$GATE_STEP_SELECTION]" >>"$GATE_FAKE_CALLS"
mkdir -p target/nextest/default
if [[ "$FAKE_CHECK_FAIL" == none ]]; then
    echo '     Summary [   1.000s] 1 tests run: 1 passed, 0 skipped'
    printf '%s\n' '<testsuites><testsuite name="canterlot-speech"><testcase name="speech::ok"/></testsuite></testsuites>' \
        >target/nextest/default/junit.xml
    exit 0
fi
echo '    Starting 5330 tests across 80 binaries'
n=0
for t in $FAKE_CHECK_FAIL; do
    n=$((n + 1))
    echo "        FAIL [   0.028s] ($n/5330) crate-a $t"
    echo '  stdout ───'
    echo
    echo '    running 1 test'
    echo "    test $t ... FAILED"
    echo
    echo '  stderr ───'
    echo
    echo "    thread '$t' panicked at src/lib.rs:10:5:"
    echo '    assertion `left == right` failed'
    echo '      left: 1'
    echo '     right: 2'
    echo
done
echo '------------'
echo "     Summary [   1.000s] 5330 tests run: 5329 passed, $n failed, 0 skipped"
for t in $FAKE_CHECK_FAIL; do
    echo "        FAIL [   0.028s] (4385/5330) crate-a $t"
done
for t in $FAKE_CHECK_FAIL; do
    echo "--- STDERR:              crate-a $t ---"
    if [[ "$t" == mod::long ]]; then
        for i in $(seq 1 16); do echo "thread '$t' panicked at src/long.rs:$i:1:"; done
    else
        echo "thread '$t' panicked at src/lib.rs:10:5:"
        echo 'assertion `left == right` failed'
        echo '  left: 1'
        echo ' right: 2'
        echo 'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace'
    fi
    echo
done
printf '%s\n' '<testsuites><testsuite name="canterlot-speech"><testcase name="speech::ok"/></testsuite></testsuites>' \
    >target/nextest/default/junit.xml
exit 1
FAKE
nextest_env=(
    env CANTERLOT_NO_LEASE=1 CANTERLOT_GATE_DEPLOY_NEEDED=0
    CANTERLOT_GATE_STEP_NPM="$fake_npm" CANTERLOT_GATE_STEP_CHECK="bash $fake_nextest"
    CANTERLOT_GATE_STEP_DEPLOY="$fake_deploy" CANTERLOT_GATE_STEP_WEB="$fake_web"
)
g9="$tmp/g9"
new_gate_repo "$g9"
g9short=$(git -C "$g9" rev-parse --short=8 HEAD)
g9dir="$g9/.superpowers/gate/$g9short"
export FAKE_CHECK_FAIL="mod::flaky mod::long mod::c mod::d mod::e mod::f mod::g"
run 1 "${nextest_env[@]}" bash "$g9/scripts/gate.sh" --no-attest
# Упавшие тесты берутся и из check.log: junit.xml перезаписан отчётом canterlot-speech.
grep -qF $'rust_fail\trust|crate-a|mod::flaky' "$g9dir/state.tsv" \
    || fail 'упавший тест воркспейса потерян, когда junit.xml перезаписан отчётом другого прогона'
grep -qF $'check_failed\ttests' "$g9dir/state.tsv" || fail 'падение check при перезаписанном junit не отнесено к тестам'
[[ $(grep -c '^rust_fail' "$g9dir/state.tsv") == 7 ]] || fail 'в состоянии не семь упавших тестов'
# Сводка: причины падений первых пяти тестов, не больше 12 строк на тест.
grep -qF 'причины падений' "$out" || { cat "$out" >&2; fail 'сводка не печатает причины падений'; }
grep -qF "panicked at src/lib.rs:10:5:" "$out" || fail 'в сводке нет строки panicked at'
grep -qF 'left: 1' "$out" || fail 'в сводке нет строк assertion left/right'
if grep -qF 'RUST_BACKTRACE' "$out"; then fail 'в причины попала строка note про backtrace'; fi
grep -qF 'mod::e' "$out" || fail 'в сводке нет пятого упавшего теста'
if grep -qE ' mod::f$' "$g9dir/failures.txt"; then fail 'причины показаны для шестого теста'; fi
grep -qF 'и ещё 2' "$out" || { cat "$out" >&2; fail 'нет пометки о непоказанных тестах'; }
[[ $(grep -c 'src/long.rs' "$out") == 12 ]] || { cat "$out" >&2; fail 'причина одного теста не обрезана до 12 строк'; }
[[ -s "$g9dir/failures.txt" ]] || fail 'причины не сохранены в failures.txt'
[[ ! -d "$g9dir/pass-1" ]] || fail 'после первого прохода уже есть pass-1'

# Зелёный перезапуск: упавший в первом проходе вывод остаётся в pass-1, верхний уровень —
# последний проход.
export FAKE_CHECK_FAIL=none
run 0 "${nextest_env[@]}" bash "$g9/scripts/gate.sh" --rerun-failed --no-attest
[[ -f "$g9dir/pass-1/check.log" ]] || fail 'нет pass-1/check.log'
grep -qF "panicked at src/lib.rs:10:5:" "$g9dir/pass-1/check.log" || fail 'в pass-1/check.log нет падения первого прохода'
[[ -f "$g9dir/pass-1/result.txt" && -f "$g9dir/pass-1/state.tsv" ]] || fail 'в pass-1 нет result.txt или state.tsv'
if grep -qF 'panicked at' "$g9dir/check.log"; then fail 'check.log верхнего уровня не относится к последнему проходу'; fi
[[ -f "$g9dir/web.log" && -f "$g9dir/npm-ci.log" ]] || fail 'пропущенные шаги потеряли логи на верхнем уровне'
grep -qF 'check tests [' "$GATE_FAKE_CALLS" || fail 'перезапуск не в режиме tests'
grep -qF 'binary_id(=crate-a) & test(=mod::flaky)' "$GATE_FAKE_CALLS" || fail 'в фильтр перезапуска не попал тест из check.log'
[[ -f "$g9dir/failures.txt" ]] && fail 'failures.txt последнего зелёного прохода не убран'
# Отчёт: тест назван нестабильным, рядом путь к логу первого прохода и причина.
sed -n '/отчёт о нестабильных тестах/,$p' "$out" >"$tmp/flaky-report"
grep -qF 'rust|crate-a|mod::flaky' "$tmp/flaky-report" || { cat "$out" >&2; fail 'в отчёте нет нестабильного теста'; }
grep -qF ".superpowers/gate/$g9short/pass-1/check.log" "$tmp/flaky-report" || { cat "$tmp/flaky-report" >&2; fail 'в отчёте нет пути к логу первого прохода'; }
grep -qF 'panicked at src/lib.rs:10:5:' "$tmp/flaky-report" || fail 'в отчёте нет причины первого прохода'
if grep -qF '  нет' "$tmp/flaky-report"; then fail 'отчёт пуст при нестабильном тесте'; fi
grep -qF $'flaky\trust|crate-a|mod::flaky\tpass-1' "$g9dir/state.tsv" || fail 'в состоянии нет прохода с падением'
# Старое состояние без третьего поля читается: тест остаётся в списке, пути нет.
awk -F '\t' -v OFS='\t' '$1=="flaky"{print $1,$2;next}{print}' "$g9dir/state.tsv" >"$tmp/g9-state-old"
cp "$tmp/g9-state-old" "$g9dir/state.tsv"
rm -rf "$g9dir/pass-1"
run 0 "${nextest_env[@]}" bash "$g9/scripts/gate.sh" --rerun-failed --no-attest
sed -n '/отчёт о нестабильных тестах/,$p' "$out" >"$tmp/flaky-report"
grep -qF 'rust|crate-a|mod::flaky' "$tmp/flaky-report" || fail 'старое состояние: тест пропал из отчёта'
[[ -d "$g9dir/pass-1" ]] || fail 'повторный перезапуск не сохранил проход'

# Битый индекс: git status падает, и проверка чистоты обязана провалиться, а не
# пропустить аттестацию из-за пустого вывода.
printf 'garbage' >"$scratch/.git/index"
run 1 env CANTERLOT_LEASE_DRY=1 bash "$scratch/scripts/gate.sh"
grep -q 'грязная' "$out" || { cat "$out" >&2; fail 'битый индекс не остановил гейт'; }

# --- scripts/gate.sh --runner: гейт на раннере ---
# Подмена canterlot-worker (CANTERLOT_WORKER_BIN) отвечает на `job run` и кладёт
# «собранные» result.txt и state.tsv; настоящих задач, docker и сети нет.
fake_worker="$PWD/scripts/gate/fake-worker"
fake_docker="$PWD/scripts/gate/fake-docker"
export FAKE_WORKER_CALLS="$tmp/worker-calls"
runner_env=(env CANTERLOT_WORKER_BIN="$fake_worker" FAKE_WORKER_OUT="$tmp/worker-out")
reset_worker() {
    : >"$FAKE_WORKER_CALLS"
    rm -rf "$tmp/worker-out"
    mkdir -p "$tmp/worker-out"
}
new_runner_repo() {
    new_gate_repo "$1"
    reset_worker
}

# Успех: ровно одна команда `job run` с зафиксированными аргументами, аттестация
# с runner= из состояния раннера, аренда на VM не берётся.
r1="$tmp/r1"
new_runner_repo "$r1"
r1sha=$(git -C "$r1" rev-parse HEAD)
run 0 "${runner_env[@]}" bash "$r1/scripts/gate.sh" --runner auto
[[ $(grep -c . "$FAKE_WORKER_CALLS") == 1 ]] || { cat "$FAKE_WORKER_CALLS" >&2; fail 'гейт на раннере сделал не один вызов воркера (аренда на VM не нужна)'; }
expected="job run --title Гейт ${r1sha:0:8} --machine auto --lease gate --timeout-minutes 240 --expected-minutes 45 --source $r1@$r1sha --output .gate-out --wait -- scripts/gate/runner-gate.sh $r1sha"
[[ $(cat "$FAKE_WORKER_CALLS") == "$expected" ]] || { cat "$FAKE_WORKER_CALLS" >&2; fail 'команда job run не та, что зафиксирована'; }
att="$r1/.superpowers/gate/$r1sha.ok"
[[ -s "$att" ]] || { cat "$out" >&2; fail 'успешный гейт на раннере не записал аттестацию'; }
grep -qx "sha=$r1sha" "$att" || fail 'аттестация раннера: нет полного sha'
grep -qx 'runner=runner-a' "$att" || fail 'аттестация раннера: нет runner= из отчёта раннера'
for key in started finished duration_s npm_ci_exit npm_ci_duration_s check_exit check_duration_s deploy_exit deploy_duration_s web_exit web_duration_s logs; do
    grep -q "^$key=" "$att" || fail "аттестация раннера: нет ключа $key"
done
grep -qx 'check_exit=0' "$att" || fail 'аттестация раннера: check_exit не взят из состояния'
grep -q '^summary=' "$att" || fail 'аттестация раннера: нет строк summary'
if grep -q '^test_seams=' "$att"; then fail 'аттестация раннера помечена тестовыми швами'; fi
for leftover in "$r1/.superpowers/gate/"*.tmp.*; do
    [[ ! -e "$leftover" ]] || fail 'после записи остался временный файл аттестации'
done
# Ровно те ключи, которые release.sh проверяет: все *_exit равны 0 или deploy skipped.
while IFS='=' read -r key value; do
    if [[ $key == *_exit && $value != 0 && $value != skipped ]]; then fail "аттестация раннера: $key=$value"; fi
done <"$att"

# --no-attest: гейт идёт, аттестации нет.
r1b="$tmp/r1b"
new_runner_repo "$r1b"
run 0 "${runner_env[@]}" bash "$r1b/scripts/gate.sh" --runner runner-a --no-attest
[[ ! -e "$r1b/.superpowers/gate/$(git -C "$r1b" rev-parse HEAD).ok" ]] || fail '--no-attest записал аттестацию на раннере'
grep -qF -- '--machine runner-a ' "$FAKE_WORKER_CALLS" || fail 'имя машины не дошло до job run'

# Чужой sha в result.txt, чужой sha в состоянии, упавший шаг, код задачи, нет
# каталога результатов: аттестации нет, причина названа.
fail_cases=(
    "FAKE_WORKER_RESULT_SHA=deadbeef|result.txt"
    "FAKE_WORKER_STATE_SHA=deadbeefdeadbeefdeadbeefdeadbeefdeadbeef|sha"
    "FAKE_WORKER_CHECK=1|check_exit"
    "FAKE_WORKER_CODE=1|код задачи"
    "FAKE_WORKER_NO_OUTPUTS=1|каталог результатов"
)
for case_spec in "${fail_cases[@]}"; do
    var=${case_spec%%|*}
    why=${case_spec#*|}
    rf="$tmp/rf"
    rm -rf "$rf"
    new_runner_repo "$rf"
    run 1 "${runner_env[@]}" "$var" bash "$rf/scripts/gate.sh" --runner auto
    [[ ! -e "$rf/.superpowers/gate/$(git -C "$rf" rev-parse HEAD).ok" ]] || fail "аттестация записана при $var"
    grep -q 'аттестация не пишется' "$out" || { cat "$out" >&2; fail "нет сообщения об отказе в аттестации при $var"; }
    grep -qF -- "$why" "$out" || { cat "$out" >&2; fail "в отказе при $var не названа причина «$why»"; }
done

# HEAD сместился или дерево стало грязным за прогон: аттестации нет.
rd="$tmp/rd"
new_runner_repo "$rd"
run 1 "${runner_env[@]}" FAKE_WORKER_DIRTY="$rd" bash "$rd/scripts/gate.sh" --runner auto
[[ ! -e "$rd/.superpowers/gate/$(git -C "$rd" rev-parse HEAD).ok" ]] || fail 'аттестация записана при грязном дереве'
grep -q 'грязной' "$out" || { cat "$out" >&2; fail 'нет отказа из-за грязного дерева после прогона'; }
rm -f "$rd/stray"
rm -rf "$rd" && new_runner_repo "$rd"
rd_before=$(git -C "$rd" rev-parse HEAD)
run 1 "${runner_env[@]}" FAKE_WORKER_COMMIT="$rd" bash "$rd/scripts/gate.sh" --runner auto
[[ ! -e "$rd/.superpowers/gate/$rd_before.ok" && ! -e "$rd/.superpowers/gate/$(git -C "$rd" rev-parse HEAD).ok" ]] \
    || fail 'аттестация записана после смещения HEAD'
grep -q 'HEAD сместился' "$out" || { cat "$out" >&2; fail 'нет отказа из-за смещения HEAD'; }

# Грязное дерево не отправляется на раннер, даже с --no-attest.
rg="$tmp/rg-dirty"
new_runner_repo "$rg"
: >"$rg/stray"
run 1 "${runner_env[@]}" bash "$rg/scripts/gate.sh" --runner auto
grep -q 'грязная' "$out" || { cat "$out" >&2; fail 'грязное дерево на раннере не отказано'; }
run 1 "${runner_env[@]}" bash "$rg/scripts/gate.sh" --runner auto --no-attest
[[ ! -s "$FAKE_WORKER_CALLS" ]] || fail 'грязное дерево дошло до воркера'

# --rerun-failed на раннере не поддерживается (v1).
run 2 "${runner_env[@]}" bash "$r1/scripts/gate.sh" --runner x --rerun-failed
grep -q 'rerun-failed.*runner' "$out" || { cat "$out" >&2; fail 'нет понятного отказа для --rerun-failed с --runner'; }
[[ ! -s "$FAKE_WORKER_CALLS" ]] || fail '--rerun-failed с --runner дошёл до воркера'
run 2 bash "$r1/scripts/gate.sh" --runner

# --runner local — прежнее поведение: аренда, а не job run.
rl="$tmp/rl"
new_gate_repo "$rl"
run 0 env CANTERLOT_LEASE_DRY=1 bash "$rl/scripts/gate.sh" --runner local
grep -q 'kind=gate' "$out" || { cat "$out" >&2; fail '--runner local не дошёл до аренды'; }

# --- --runner prefer: доступный раннер, иначе эта машина ---
# Фикстура конфига с нейтральными именами; проба идёт через шов, без сети.
cat >"$tmp/worker-prefer.toml" <<'EOF'
[[runners.ssh]]
machine = "build-host"
ssh = "build-host-ssh"

[[runners.ssh]]
machine = "runner-a"
ssh = "runner-a-ssh"
EOF
prefer_env=("${runner_env[@]}" CANTERLOT_WORKER_CONFIG="$tmp/worker-prefer.toml")

# Первая доступная запись уходит на раннер: ровно одна задача, без метки ядра.
p1="$tmp/p1"
new_runner_repo "$p1"
run 0 "${prefer_env[@]}" CANTERLOT_GATE_RUNNER_PROBE=true bash "$p1/scripts/gate.sh" --runner prefer
[[ $(grep -c . "$FAKE_WORKER_CALLS") == 1 ]] || fail 'prefer: не один вызов воркера'
grep -qF -- '--machine build-host ' "$FAKE_WORKER_CALLS" \
    || { cat "$FAKE_WORKER_CALLS" >&2; fail 'prefer: доступный раннер не получил машину'; }
if grep -q -- '--labels' "$FAKE_WORKER_CALLS"; then fail 'prefer: задача на раннере требует метку ядра'; fi
grep -q 'раннер build-host доступен' "$out" || { cat "$out" >&2; fail 'prefer: нет сообщения о выборе раннера'; }

# Ни один раннер не доступен: prefer идёт локально, воркера не вызывает.
p2="$tmp/p2"
new_gate_repo "$p2"
: >"$FAKE_WORKER_CALLS"
run 0 env CANTERLOT_WORKER_CONFIG="$tmp/worker-prefer.toml" CANTERLOT_GATE_RUNNER_PROBE=false \
    CANTERLOT_LEASE_DRY=1 bash "$p2/scripts/gate.sh"
grep -q 'раннеров нет или они недоступны' "$out" || { cat "$out" >&2; fail 'prefer: нет сообщения о переходе на эту машину'; }
grep -q 'kind=gate' "$out" || { cat "$out" >&2; fail 'prefer без доступного раннера не дошёл до аренды'; }
[[ ! -s "$FAKE_WORKER_CALLS" ]] || fail 'prefer без доступного раннера вызвал воркера'

# prefer + --rerun-failed: без локального состояния — полный гейт через prefer.
p3="$tmp/p3"
new_runner_repo "$p3"
run 0 "${prefer_env[@]}" CANTERLOT_GATE_RUNNER_PROBE=true bash "$p3/scripts/gate.sh" --rerun-failed
grep -q 'локального состояния этого коммита нет' "$out" \
    || { cat "$out" >&2; fail 'нет сообщения о полном гейте через prefer'; }
[[ $(grep -c . "$FAKE_WORKER_CALLS") == 1 ]] || fail 'полный гейт через prefer: не один вызов воркера'
grep -qF -- '--machine build-host ' "$FAKE_WORKER_CALLS" || fail 'полный гейт через prefer: не на выбранном раннере'
att="$p3/.superpowers/gate/$(git -C "$p3" rev-parse HEAD).ok"
if grep -q '^rerun_of=' "$att"; then fail 'полный гейт через prefer записал rerun_of'; fi

# С локальным состоянием — перезапуск только упавших тестов на этой машине, даже
# если prefer нашёл бы доступный раннер.
p4="$tmp/p4"
new_gate_repo "$p4"
run 1 "${rerun_env[@]}" FAKE_CHECK_FAIL=mod::flaky \
    CANTERLOT_WORKER_CONFIG="$tmp/worker.toml" bash "$p4/scripts/gate.sh" --no-attest
: >"$GATE_FAKE_CALLS"
: >"$FAKE_WORKER_CALLS"
run 0 "${rerun_env[@]}" FAKE_CHECK_FAIL=none CANTERLOT_WORKER_CONFIG="$tmp/worker-prefer.toml" \
    CANTERLOT_GATE_RUNNER_PROBE=true bash "$p4/scripts/gate.sh" --rerun-failed --no-attest
grep -q 'есть локальное состояние этого коммита' "$out" \
    || { cat "$out" >&2; fail 'prefer: нет сообщения о локальном перезапуске'; }
grep -qF 'check tests [' "$GATE_FAKE_CALLS" || fail 'prefer: локальный перезапуск не перезапустил упавший тест'
[[ ! -s "$FAKE_WORKER_CALLS" ]] || fail 'prefer: перезапуск с локальным состоянием ушёл на раннер'

# Прерывание: гейт не оставляет задачу молча, называет её и способ отмены.
rt="$tmp/rt"
new_runner_repo "$rt"
pidfile="$tmp/worker.pid"
rm -f "$pidfile"
"${runner_env[@]}" FAKE_WORKER_PIDFILE="$pidfile" bash "$rt/scripts/gate.sh" --runner auto >"$out" 2>&1 &
gate_pid=$!
for _ in $(seq 1 100); do
    grep -q 'accepted' "$out" 2>/dev/null && [[ -s "$pidfile" ]] && break
    sleep 0.1
done
[[ -s "$pidfile" ]] || { cat "$out" >&2; fail 'прерывание: задача не стартовала'; }
kill -TERM "$(cat "$pidfile")" "$gate_pid"
gate_rc=0
wait "$gate_pid" || gate_rc=$?
[[ "$gate_rc" == 143 || "$gate_rc" == 130 ]] || { cat "$out" >&2; fail "прерывание: код $gate_rc"; }
grep -q '00000000-0000-4000-8000-000000000001' "$out" || { cat "$out" >&2; fail 'прерывание: не назван идентификатор задачи'; }
grep -q 'продолжает работать' "$out" || { cat "$out" >&2; fail 'прерывание: нет предупреждения, что задача продолжает работать'; }
[[ ! -e "$rt/.superpowers/gate/$(git -C "$rt" rev-parse HEAD).ok" ]] || fail 'прерванный гейт записал аттестацию'

# --- scripts/gate/runner-gate.sh: на раннере, в подготовленной рабочей копии ---
export FAKE_DOCKER_CALLS="$tmp/docker-calls"
# Рабочая копия раннера — связанный worktree bare-репозитория, как её готовит
# носитель (<корень>/repos/<домен>/<ключ>.git и <корень>/jobs/<домен>/<задача>): её
# .git — файл, указывающий в общий каталог git вне рабочей копии.
new_runner_worktree() {
    local root=$1 seed dir bare
    seed="$root/seed"
    dir="$root/jobs/personal/job1"
    bare="$root/repos/personal/key.git"
    mkdir -p "$seed/scripts/gate" "$seed/docker/gate" "$seed/web" "$root/repos/personal" "$root/jobs/personal"
    cp scripts/gate/runner-gate.sh scripts/gate/in-container.sh "$seed/scripts/gate/"
    printf 'FROM scratch\n' >"$seed/docker/gate/Dockerfile"
    printf '[toolchain]\nchannel = "1.99.1"\n' >"$seed/rust-toolchain.toml"
    (
        cd "$seed"
        git init -q
        git config user.email gate@example.invalid
        git config user.name gate
        git config commit.gpgsign false
        printf '.gate-out/\n' >.gitignore
        git add .
        git commit -qm init
    )
    git clone -q --bare "$seed" "$bare"
    git -C "$bare" worktree add -q --detach "$dir" "$(git -C "$seed" rev-parse HEAD)"
    : >"$FAKE_DOCKER_CALLS"
}
# Видит ли контейнер git рабочей копии? Её .git — файл с путём вне рабочей копии, а
# внутри контейнера git читает его по тому же пути: каждый из каталогов (gitdir и общий
# каталог) должен лежать под монтированием, у которого путь в контейнере равен пути на
# хосте. Без этого `git -C /src rev-parse HEAD` в контейнере падает.
assert_git_mounted() {
    local line=$1 wt=$2 what=$3 gitdir common dir ok mount host cont mounts
    mapfile -t mounts < <(grep -oE -- '-v [^ ]+' <<<"$line" | cut -d' ' -f2)
    gitdir=$(sed -n 's/^gitdir: //p' "$wt/.git")
    [[ -n "$gitdir" && -d "$gitdir" ]] || fail "$what: у $wt нет связанного gitdir"
    common=$(cd "$gitdir" && cd "$(cat commondir)" && pwd -P)
    for dir in "$gitdir" "$common"; do
        ok=0
        for mount in "${mounts[@]}"; do
            host=${mount%%:*}
            cont=${mount#*:}
            cont=${cont%%:*}
            if [[ "$host" == "$cont" && ( "$dir" == "$host" || "$dir" == "$host"/* ) ]]; then ok=1; fi
        done
        ((ok)) || fail "$what: каталог git $dir не смонтирован в контейнер по своему пути: $line"
    done
}
rg_env=(env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u NO_PROXY \
    -u http_proxy -u https_proxy -u all_proxy -u no_proxy CANTERLOT_DOCKER="$fake_docker")

# Без worker.toml в корне раннера скрипт отказывает до docker.
rr="$tmp/runner-root"
new_runner_worktree "$rr"
rwt="$rr/jobs/personal/job1"
rsha=$(git -C "$rwt" rev-parse HEAD)
run 1 "${rg_env[@]}" bash "$rwt/scripts/gate/runner-gate.sh" "$rsha"
grep -q 'worker.toml' "$out" || { cat "$out" >&2; fail 'runner-gate: нет отказа без worker.toml'; }
[[ ! -s "$FAKE_DOCKER_CALLS" ]] || fail 'runner-gate: docker вызван без worker.toml'

# Рабочая копия не в <корень>/jobs/<домен>/<задача>: корень не найти.
mkdir -p "$tmp/not-a-runner"
cp -a "$rwt" "$tmp/not-a-runner/wt"
run 1 "${rg_env[@]}" bash "$tmp/not-a-runner/wt/scripts/gate/runner-gate.sh" "$rsha"
grep -q 'корень раннера' "$out" || { cat "$out" >&2; fail 'runner-gate: нет отказа вне <корень>/jobs/<домен>/<задача>'; }

# Другой sha, чем у HEAD: отказ до docker.
: >"$rr/worker.toml"
run 1 "${rg_env[@]}" bash "$rwt/scripts/gate/runner-gate.sh" "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
grep -q 'не совпадает' "$out" || { cat "$out" >&2; fail 'runner-gate: нет отказа для чужого sha'; }
[[ ! -s "$FAKE_DOCKER_CALLS" ]] || fail 'runner-gate: docker вызван при несовпадении sha'
run 2 "${rg_env[@]}" bash "$rwt/scripts/gate/runner-gate.sh"
run 1 "${rg_env[@]}" bash "$rwt/scripts/gate/runner-gate.sh" "not-a-sha"

# Успешный запуск: образ уже есть, контейнер именован, лимиты и кэши под корнем
# раннера, результат в ./.gate-out с именем раннера.
export FAKE_DOCKER_WRITE=1
run 0 "${rg_env[@]}" CANTERLOT_RUNNER_NAME=runner-a bash "$rwt/scripts/gate/runner-gate.sh" "$rsha"
run_line=$(grep '^run ' "$FAKE_DOCKER_CALLS") || { cat "$FAKE_DOCKER_CALLS" >&2; fail 'runner-gate: нет docker run'; }
grep -q -- "--name canterlot-gate-${rsha:0:12}-[0-9a-f]\{8\} " <<<"$run_line" || fail "runner-gate: нет имени canterlot-gate-<sha12>-<случайное>: $run_line"
grep -q -- '--cpus [0-9]' <<<"$run_line" || fail 'runner-gate: нет --cpus'
grep -q -- '--memory [0-9]\+m' <<<"$run_line" || fail 'runner-gate: нет --memory'
for cache in cargo target npm; do
    grep -qF -- "-v $rr/cache/$cache:" <<<"$run_line" || fail "runner-gate: кэш $cache не под корнем раннера"
done
assert_git_mounted "$run_line" "$rwt" 'runner-gate'
grep -qF -- "-v $rwt/.gate-out:/out" <<<"$run_line" || fail 'runner-gate: результат не в ./.gate-out'
grep -qF -- "-v $rwt/scripts/gate/in-container.sh:/build/in-container.sh:ro" <<<"$run_line" || fail 'runner-gate: in-container.sh не из этой рабочей копии'
grep -qE -- "canterlot-gate:[0-9a-f]{12} bash /build/in-container.sh --src /src --out /out --sha ${rsha:0:12}" <<<"$run_line" \
    || fail "runner-gate: не та команда в контейнере: $run_line"
grep -qF -- ' -e CANTERLOT_GATE_CONTAINER=1 ' <<<"$run_line" || fail "runner-gate: контейнер не получил метку CANTERLOT_GATE_CONTAINER: $run_line"
if grep -q '^build ' "$FAKE_DOCKER_CALLS"; then fail 'runner-gate: образ пересобран, хотя он есть'; fi
grep -qx 'runner	runner-a' "$rwt/.gate-out/state.tsv" || fail 'runner-gate: имя раннера не дописано в state.tsv'
[[ -s "$rwt/.gate-out/result.txt" ]] || fail 'runner-gate: нет result.txt в .gate-out'

# Образа нет: он собирается из Dockerfile ЭТОЙ рабочей копии с версиями из неё.
rm -rf "$rwt/.gate-out"
: >"$FAKE_DOCKER_CALLS"
run 0 "${rg_env[@]}" FAKE_DOCKER_IMAGE_RC=1 bash "$rwt/scripts/gate/runner-gate.sh" "$rsha"
build_line=$(grep '^build ' "$FAKE_DOCKER_CALLS") || fail 'runner-gate: образ не собран'
grep -qF -- "-f $rwt/docker/gate/Dockerfile" <<<"$build_line" || fail "runner-gate: Dockerfile не из этой рабочей копии: $build_line"
grep -qF -- 'RUST_VERSION=1.99.1' <<<"$build_line" || fail 'runner-gate: нет версии Rust из rust-toolchain.toml'
# Без прокси сетевые аргументы docker не меняются.
if grep -q -- '--network host' <<<"$build_line"; then fail 'runner-gate: без прокси сборка получила хостовую сеть'; fi
run_line=$(grep '^run ' "$FAKE_DOCKER_CALLS") || fail 'runner-gate: нет docker run'
if grep -q -- '--network host' <<<"$run_line"; then fail 'runner-gate: без прокси контейнер получил хостовую сеть'; fi
if grep -qE -- '(^| )-e [A-Za-z_]*PROXY=' <<<"$run_line"; then fail 'runner-gate: без прокси контейнер получил переменные прокси'; fi
if grep -q -- '--build-arg [A-Za-z_]*PROXY=' <<<"$build_line"; then fail 'runner-gate: без прокси сборка получила переменные прокси'; fi

# Прокси облачного раннера: cloud-init кладёт SOCKS5 на loopback машины, поэтому и сборка,
# и контейнер идут в хостовой сети и получают те же переменные. Сборка заодно проверяет
# каждый set-вариант восьми имён.
rm -rf "$rwt/.gate-out"
: >"$FAKE_DOCKER_CALLS"
proxy_env=(env CANTERLOT_DOCKER="$fake_docker"
    "HTTP_PROXY=socks5h://127.0.0.1:1080" "HTTPS_PROXY=socks5h://127.0.0.1:1080"
    "ALL_PROXY=socks5h://127.0.0.1:1080" "NO_PROXY=localhost,127.0.0.1"
    "http_proxy=socks5h://127.0.0.1:1080" "https_proxy=socks5h://127.0.0.1:1080"
    "all_proxy=socks5h://127.0.0.1:1080" "no_proxy=localhost,127.0.0.1")
run 0 "${proxy_env[@]}" FAKE_DOCKER_IMAGE_RC=1 bash "$rwt/scripts/gate/runner-gate.sh" "$rsha"
build_line=$(grep '^build ' "$FAKE_DOCKER_CALLS") || fail 'runner-gate: с прокси образ не собран'
grep -q -- '--network host' <<<"$build_line" || fail 'runner-gate: с прокси сборка без хостовой сети'
run_line=$(grep '^run ' "$FAKE_DOCKER_CALLS") || fail 'runner-gate: с прокси нет docker run'
grep -q -- '--network host' <<<"$run_line" || fail 'runner-gate: с прокси контейнер без хостовой сети'
for name in HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY http_proxy https_proxy all_proxy no_proxy; do
    grep -qE -- "(^| )--build-arg $name=[^ ]+" <<<"$build_line" \
        || fail "runner-gate: с прокси сборка без --build-arg $name"
    grep -qE -- "(^| )-e $name=[^ ]+" <<<"$run_line" \
        || fail "runner-gate: с прокси контейнер без -e $name"
done

# Код гейта проходит наружу; без state.tsv и result.txt — провал даже при коде 0.
rm -rf "$rwt/.gate-out"
run 3 "${rg_env[@]}" FAKE_DOCKER_RUN_RC=3 bash "$rwt/scripts/gate/runner-gate.sh" "$rsha"
rm -rf "$rwt/.gate-out"
unset FAKE_DOCKER_WRITE
run 1 "${rg_env[@]}" bash "$rwt/scripts/gate/runner-gate.sh" "$rsha"
grep -q 'result.txt' "$out" || { cat "$out" >&2; fail 'runner-gate: не сказал, что результата нет'; }

# TERM: контейнер убирается по имени.
rm -rf "$rwt/.gate-out"
: >"$FAKE_DOCKER_CALLS"
dpid="$tmp/docker.pid"
rm -f "$dpid"
"${rg_env[@]}" FAKE_DOCKER_PIDFILE="$dpid" bash "$rwt/scripts/gate/runner-gate.sh" "$rsha" >"$out" 2>&1 &
rg_pid=$!
for _ in $(seq 1 100); do
    [[ -s "$dpid" ]] && break
    sleep 0.1
done
[[ -s "$dpid" ]] || { cat "$out" >&2; fail 'runner-gate: контейнер не стартовал'; }
kill -TERM "$rg_pid"
rg_rc=0
wait "$rg_pid" || rg_rc=$?
[[ "$rg_rc" == 143 ]] || { cat "$out" >&2; fail "runner-gate: код после TERM $rg_rc"; }
name=$(sed -n 's/^run .*--name \(canterlot-gate-[0-9a-f]*-[0-9a-f]*\) .*/\1/p' "$FAKE_DOCKER_CALLS")
[[ -n "$name" ]] || { cat "$FAKE_DOCKER_CALLS" >&2; fail 'runner-gate: не нашли имя контейнера в docker run'; }
grep -qx "kill $name" "$FAKE_DOCKER_CALLS" || { cat "$FAKE_DOCKER_CALLS" >&2; fail 'runner-gate: TERM не убил контейнер по имени'; }

# gate-remote.sh --local: рабочая копия — связанный worktree этого репозитория, и
# контейнеру нужен общий каталог git по его собственному пути (иначе in-container.sh не
# сверит HEAD). Docker подменён, рабочая копия убирается скриптом сама.
: >"$FAKE_DOCKER_CALLS"
export FAKE_DOCKER_WRITE=1
run 0 env CANTERLOT_DOCKER="$fake_docker" CANTERLOT_GATE_OUT="$tmp/gr-out" CANTERLOT_GATE_CACHE="$tmp/gr-cache" \
    bash "$gate" HEAD --local
unset FAKE_DOCKER_WRITE
run_line=$(grep '^run ' "$FAKE_DOCKER_CALLS") || { cat "$FAKE_DOCKER_CALLS" >&2; fail 'gate-remote --local: нет docker run'; }
common=$(cd "$(git rev-parse --git-common-dir)" && pwd -P)
src_host=$(sed -n 's/.* -v \([^ ]*\):\/src .*/\1/p' <<<"$run_line")
[[ "$src_host" == "$common"/canterlot-gate/src-* ]] || fail "gate-remote --local: рабочая копия не там: $src_host"
grep -qF -- " -v $common:$common:ro " <<<"$run_line" || fail "gate-remote --local: общий каталог git не смонтирован только на чтение: $run_line"
grep -qF -- ' -e CANTERLOT_GATE_CONTAINER=1 ' <<<"$run_line" || fail "gate-remote --local: контейнер не получил метку CANTERLOT_GATE_CONTAINER: $run_line"

if command -v shellcheck >/dev/null 2>&1; then
    shellcheck scripts/gate.sh scripts/gate-coverage.sh scripts/gate-remote.sh \
        scripts/gate/in-container.sh scripts/gate/runner-gate.sh scripts/gate/test.sh \
        scripts/gate/fake-check-web.sh scripts/gate/fake-worker scripts/gate/fake-docker \
        || fail "shellcheck"
fi

printf 'все быстрые проверки прошли\n'
