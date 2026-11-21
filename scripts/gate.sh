#!/usr/bin/env bash
# Полный гейт перед выпуском. Заменяет частный /tmp/gate/run.sh и гоняет веб
# ровно один раз:
#   1. npm ci в web/ — один раз на весь прогон;
#   2. scripts/check.sh --all --no-web — версия, fmt, clippy, nextest, речь;
#   3. scripts/check-web.sh --stack — вся веб-половина (статические проверки,
#      vitest, две сборки PWA, vitest.build, Playwright e2e/ui и e2e/parity)
#      и сквозные сценарии на настоящем хабе.
#
#   scripts/gate.sh                  полный гейт с аттестацией .superpowers/gate/<full sha>.ok
#   scripts/gate.sh --rerun-failed   перезапустить только то, что упало на этом же коммите
#   scripts/gate.sh --no-attest      гейт без аттестации (нужен, если рабочая копия грязная)
#   scripts/gate.sh --runner local|auto|<машина>
#                                    где гонять гейт: local (по умолчанию) — на этой VM, как
#                                    описано ниже; auto или имя машины — на раннере через
#                                    canterlot-worker job run (см. «Гейт на раннере»)
#   scripts/gate.sh --help
#
# Обычный прогон пишет в .superpowers/gate/<sha8>/ логи, сводку и состояние state.tsv
# (коды шагов, идентичности упавших тестов). --rerun-failed читает состояние предыдущего
# завершённого прогона того же HEAD: прошедшие шаги не трогает, упавшие по тестам шаги
# перезапускает только на этих тестах, упавшие не по тестам — целиком. Каждый тест можно
# перезапустить не более одного раза на коммит; повторное падение — настоящий провал.
# Гейт берёт одну аренду gate на весь прогон (scripts/lease.sh). Переменные:
#   CANTERLOT_TEST_PG_PORT  порт тестовой PostgreSQL (по умолчанию 55441);
#   CANTERLOT_WORKER_BIN    бинарник воркера для аренды (по умолчанию установка
#                           воркера в $HOME/.local/opt/canterlot/current/bin);
#   CANTERLOT_GATE_TMPDIR   временный каталог гейта (по умолчанию /tmp: сокет
#                           исполнения не влезает в SUN_LEN при длинном пути);
#   CANTERLOT_NO_LEASE=1    идти без аренды (отладка).
#
# Аттестация пишется только при полном успехе, чистой рабочей копии и неизменном HEAD:
# release.sh ship принимает только аттестацию ровно того коммита, который выкатывает.
#
# Гейт на раннере (--runner auto|<машина>). Нужна чистая рабочая копия: на раннер уходит
# ровно полный sha HEAD. Скрипт зовёт
#   canterlot-worker job run --machine <значение> --lease gate --source <repo>@<sha> \
#       --output .gate-out --wait -- scripts/gate/runner-gate.sh <sha>
# носитель кладёт этот коммит в чистую рабочую копию на раннере, scripts/gate/runner-gate.sh
# гоняет в ней контейнерный гейт (те же шаги, что и здесь), а результат
# (.gate-out/result.txt и state.tsv) возвращается в каталог результатов задачи на этой VM.
# Аттестацию .superpowers/gate/<full sha>.ok в прежнем формате плюс строка runner=<машина,
# как её назвал раннер> гейт пишет сам, только если result.txt называет этот sha, каждый шаг
# вышел с 0, код задачи 0, а HEAD не сместился и дерево осталось чистым. Рабочий гейт на
# раннере берёт аренду gate на самом раннере (--lease gate), а не на этой VM.
# Отмены у `job run` нет: Ctrl-C останавливает только ожидание, задача на раннере
# продолжает работать. Скрипт называет её идентификатор; отменить её можно в PWA.
# --rerun-failed с раннером в v1 не поддерживается.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
original_args=("$@")

usage() {
    echo 'Использование: scripts/gate.sh [--rerun-failed] [--no-attest] [--runner local|auto|<машина>]'
}

attest=1
rerun=0
runner=local
while (($#)); do
    case "$1" in
        --no-attest) attest=0 ;;
        --rerun-failed) rerun=1 ;;
        --runner)
            [[ $# -ge 2 ]] || { usage >&2; exit 2; }
            runner=$2
            shift
            ;;
        --runner=*) runner=${1#--runner=} ;;
        --help | -h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
    shift
done
if [[ "$runner" != local && ! "$runner" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
    printf 'гейт: --runner: ожидается local, auto или имя машины, а не «%s»\n' "$runner" >&2
    exit 2
fi
if [[ "$runner" != local ]] && ((rerun)); then
    printf 'гейт: --rerun-failed не поддерживается вместе с --runner (v1): гейт на раннере идёт целиком; уберите --rerun-failed или --runner\n' >&2
    exit 2
fi

# Значения частного скрипта остаются умолчаниями и перекрываются окружением.
export CANTERLOT_WORKER_BIN="${CANTERLOT_WORKER_BIN:-$HOME/.local/opt/canterlot/current/bin/canterlot-worker}"
export CANTERLOT_TEST_PG_PORT="${CANTERLOT_TEST_PG_PORT:-55441}"
# Сокет исполнения — <TMPDIR>/canterlot/<uuid>.sock, а supervisor::ipc отвергает
# путь длиннее 107 байт: длинный TMPDIR сессии роняет тесты хаба, супервизоров и
# адаптеров. Как и контейнерный гейт, берём короткий /tmp; свой путь — в
# CANTERLOT_GATE_TMPDIR.
export TMPDIR="${CANTERLOT_GATE_TMPDIR:-/tmp}"

# Тестовые швы: подмена шагов и выбора наборов выката. В обычном прогоне пусто;
# они используются только scripts/gate/test.sh, чтобы гонять гейт без сети.
step_npm="${CANTERLOT_GATE_STEP_NPM:-}"
step_check="${CANTERLOT_GATE_STEP_CHECK:-}"
step_deploy="${CANTERLOT_GATE_STEP_DEPLOY:-}"
step_web="${CANTERLOT_GATE_STEP_WEB:-}"
step_needed="${CANTERLOT_GATE_DEPLOY_NEEDED:-}"
# Подменённый шаг — уже не доказательство качества: такой прогон не имеет права
# дать аттестацию, которую примет release.sh. Метка живёт и в состоянии, чтобы
# перезапуск не «отмыл» швы, использованные в первом прогоне.
seams_active=0
for seam in "$step_npm" "$step_check" "$step_deploy" "$step_web" "$step_needed"; do
    [[ -n "$seam" ]] && seams_active=1
done
if ((seams_active)); then
    printf 'ВНИМАНИЕ: включены тестовые швы CANTERLOT_GATE_STEP_* / CANTERLOT_GATE_DEPLOY_NEEDED: это НЕ настоящий гейт; аттестация получит test_seams=1 и будет отклонена release.sh\n' >&2
fi

die() { printf 'гейт: %s\n' "$*" >&2; exit 1; }

clean_tree() {
    # git status может упасть (битый индекс, отказ safe.directory): пустой вывод
    # тогда ничего не значит, поэтому проверка обязана провалиться, а не пройти.
    local out
    out=$(git status --porcelain) || return 1
    [[ -z "$out" ]]
}

# --- гейт на раннере ---------------------------------------------------------------
# Идёт мимо аренды этой VM и мимо шагов ниже: всё делает scripts/gate/runner-gate.sh на
# раннере. Здесь только отправка, ожидание и проверка собранного результата.
rj_err=""
rj_fifo=""
rj_note() { printf 'гейт: %s\n' "$*" >&2; }

# Задача на раннере переживает этот скрипт: у `job run` нет отмены.
runner_interrupted() {
    local code=$1 id=""
    [[ -z "$rj_fifo" ]] || rm -f "$rj_fifo"
    [[ -f "$rj_err" ]] && id=$(sed -n 's/^canterlot job: \([0-9a-f-]\{36\}\) accepted$/\1/p' "$rj_err" | head -n 1)
    printf '\nгейт: прерван. Задача%s на раннере продолжает работать: у canterlot-worker job run нет отмены. Отмените её в PWA (карточка задачи) или дождитесь конца; результат останется в каталоге результатов воркера.\n' \
        "${id:+ $id}" >&2
    exit "$code"
}

# Значение ключа в state.tsv раннера (первое вхождение).
runner_state() { awk -F '\t' -v k="$2" '$1==k{print $2;exit}' "$1"; }

runner_gate() {
    local sha short log_dir started_at started_s tee_pid code=0 outputs collected state result
    local first key value ran_on why requested=$runner attestation tmp total_s line
    if ! clean_tree; then
        rj_note 'рабочая копия грязная: на раннер уходит ровно закоммиченный sha; закоммитьте или уберите изменения (--no-attest этого не отменяет)'
        git status --short >&2 || true
        exit 1
    fi
    [[ -x "$CANTERLOT_WORKER_BIN" ]] || die "нет воркера $CANTERLOT_WORKER_BIN (переменная CANTERLOT_WORKER_BIN)"
    sha=$(git rev-parse HEAD)
    short=${sha:0:8}
    log_dir=".superpowers/gate/$short"
    mkdir -p "$log_dir"
    rj_err="$log_dir/runner-job.err"
    : >"$rj_err"
    started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    started_s=$SECONDS
    printf 'гейт %s на раннере %s: логи %s\n' "$short" "$requested" "$log_dir"

    # stderr команды идёт и на экран, и в файл (из него берутся идентификатор задачи и
    # каталог результатов); через канал, чтобы дождаться tee до чтения файла.
    rj_fifo="$log_dir/.runner-job.fifo.$$"
    rm -f "$rj_fifo"
    mkfifo "$rj_fifo"
    tee "$rj_err" <"$rj_fifo" >&2 &
    tee_pid=$!
    trap 'runner_interrupted 130' INT
    trap 'runner_interrupted 143' TERM
    "$CANTERLOT_WORKER_BIN" job run --title "Гейт $short" --machine "$requested" \
        --labels kernel-6.12 --lease gate --timeout-minutes 240 --expected-minutes 45 \
        --source "$PWD@$sha" --output .gate-out --wait \
        -- scripts/gate/runner-gate.sh "$sha" \
        >"$log_dir/runner-job.out" 2>"$rj_fifo" || code=$?
    wait "$tee_pid" 2>/dev/null || true
    trap - INT TERM
    rm -f "$rj_fifo"
    cat "$log_dir/runner-job.out"

    outputs=$(sed -n 's/^canterlot job: outputs: //p' "$rj_err" | tail -n 1)
    collected="$outputs/.gate-out"
    state="$collected/state.tsv"
    result="$collected/result.txt"
    if [[ -n "$outputs" && -d "$collected" ]]; then
        mkdir -p "$log_dir/runner"
        cp -a "$collected/." "$log_dir/runner/" || true
        [[ -f "$result" ]] && cat "$result"
    fi

    # Причина отказа в аттестации: первая из проверок, не прошедшая.
    why=""
    if ((code != 0)); then
        why="код задачи $code, не 0"
    elif [[ -z "$outputs" || ! -d "$outputs" ]]; then
        why='задача не вернула каталог результатов'
    elif [[ ! -f "$result" || ! -f "$state" ]]; then
        why="в каталоге результатов нет .gate-out/result.txt или state.tsv ($outputs)"
    else
        read -r first _ <"$result" || first=""
        if [[ "$first" != "$short" ]]; then
            why="result.txt называет коммит «${first:-пусто}», а не $short"
        elif [[ "$(runner_state "$state" schema)" != 1 || "$(runner_state "$state" completed)" != 1 ]]; then
            why='state.tsv: неизвестный формат или прогон не завершён'
        elif [[ "$(runner_state "$state" sha)" != "$sha" ]]; then
            why="sha в state.tsv не $sha"
        elif [[ "$(runner_state "$state" seams)" == 1 ]]; then
            why='state.tsv: на раннере были включены тестовые швы'
        else
            for key in npm_ci_exit check_exit deploy_exit web_exit; do
                value=$(runner_state "$state" "$key")
                if [[ "$value" != 0 && ! ("$key" == deploy_exit && "$value" == skipped) ]]; then
                    why="$key=${value:-нет}"
                    break
                fi
            done
        fi
    fi
    if [[ -n "$why" ]]; then
        rj_note "провал на раннере: $why; аттестация не пишется (логи $log_dir)"
        exit 1
    fi

    total_s=$((SECONDS - started_s))
    if ((!attest)); then
        printf 'гейт: успех без аттестации (--no-attest), логи в %s\n' "$log_dir"
        exit 0
    fi
    # Аттестация честна только для того же чистого коммита, что был отправлен.
    if [[ "$(git rev-parse HEAD)" != "$sha" ]]; then
        rj_note "HEAD сместился с $sha за прогон; аттестация не пишется"
        exit 1
    fi
    if ! clean_tree; then
        rj_note 'рабочая копия стала грязной за прогон; аттестация не пишется'
        git status --short >&2 || true
        exit 1
    fi

    ran_on=$(runner_state "$state" runner)
    if [[ -z "$ran_on" ]]; then
        ran_on=$requested
        [[ "$ran_on" != auto ]] || ran_on=unknown
    fi
    attestation=".superpowers/gate/$sha.ok"
    tmp="$attestation.tmp.$$"
    {
        printf 'sha=%s\n' "$sha"
        printf 'started=%s\n' "$started_at"
        printf 'finished=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'duration_s=%s\n' "$total_s"
        printf 'npm_ci_exit=%s\n' "$(runner_state "$state" npm_ci_exit)"
        printf 'npm_ci_duration_s=%s\n' "$(runner_state "$state" npm_ci_duration_s)"
        printf 'check_exit=%s\n' "$(runner_state "$state" check_exit)"
        printf 'check_duration_s=%s\n' "$(runner_state "$state" check_duration_s)"
        printf 'deploy_exit=%s\n' "$(runner_state "$state" deploy_exit)"
        printf 'deploy_duration_s=%s\n' "$(runner_state "$state" deploy_duration_s)"
        printf 'web_exit=%s\n' "$(runner_state "$state" web_exit)"
        printf 'web_duration_s=%s\n' "$(runner_state "$state" web_duration_s)"
        printf 'logs=%s\n' "$log_dir"
        printf 'runner=%s\n' "$ran_on"
        while IFS= read -r line; do
            printf 'summary=%s\n' "$line"
        done <"$result"
    } >"$tmp"
    mv -f "$tmp" "$attestation"
    printf 'гейт %s за %ds на раннере %s\n' "$short" "$total_s" "$ran_on"
    printf 'гейт: аттестация записана в %s\n' "$attestation"
    exit 0
}

if [[ "$runner" != local ]]; then
    runner_gate
fi

# Грязная копия не даёт аттестации, а без неё гейт нужен только по явной просьбе.
if ((attest)) && ! clean_tree; then
    echo 'гейт: рабочая копия грязная, аттестация невозможна; запустите с --no-attest' >&2
    git status --short >&2 || true
    exit 1
fi

# shellcheck disable=SC1091 # путь к scripts/lease.sh задаётся рабочей копией
source scripts/lease.sh
lease_self_wrap gate "gate.sh${original_args[*]:+ ${original_args[*]}}" \
    bash "$PWD/scripts/gate.sh" "${original_args[@]}"

if [[ -n "${CANTERLOT_GATE_DEPLOY_NEEDED:-}" ]]; then
    deploy_needed() { [[ "${CANTERLOT_GATE_DEPLOY_NEEDED}" == 1 ]]; }
else
    # shellcheck disable=SC1091 # путь к scripts/deploy/changed.sh задаётся рабочей копией
    source scripts/deploy/changed.sh
    deploy_needed() { deploy_tests_needed all; }
fi

# Аренда могла ждать долго: начало прогона и HEAD фиксируются после неё.
sha=$(git rev-parse HEAD)
short=${sha:0:8}
log_dir=".superpowers/gate/$short"
mkdir -p "$log_dir"
started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
started=$SECONDS

if ((attest)) && ! clean_tree; then
    echo 'гейт: рабочая копия стала грязной, пока ждали аренду; аттестация невозможна' >&2
    git status --short >&2 || true
    exit 1
fi

printf 'гейт %s: pg=127.0.0.1:%s, логи %s\n' "$short" "$CANTERLOT_TEST_PG_PORT" "$log_dir"

# --- чтение состояния предыдущего прогона -------------------------------------
state_value() { awk -F '\t' -v k="$2" '$1==k{print $2;exit}' "$1"; }
state_values() { awk -F '\t' -v k="$2" '$1==k{print $2}' "$1"; }
state_has() {
    awk -F '\t' -v k="$2" -v v="$3" '$1==k&&$2==v{f=1}END{exit !f}' "$1"
}
in_array() {
    local needle=$1 item
    shift
    for item in "$@"; do [[ "$item" == "$needle" ]] && return 0; done
    return 1
}

prev=""
prev_started=""
declare -a done_ids=() prev_flaky=() prev_rust=() prev_deploy=() prev_pw=() prev_stack=() to_rerun=()
if ((rerun)); then
    prev="$log_dir/state.tsv"
    [[ -f "$prev" ]] || die "нет состояния предыдущего завершённого прогона для --rerun-failed на коммите $short"
    [[ "$(state_value "$prev" schema)" == 1 ]] || die 'неизвестный формат состояния предыдущего прогона'
    [[ "$(state_value "$prev" completed)" == 1 ]] || die 'предыдущий прогон не завершён'
    [[ "$(state_value "$prev" sha)" == "$sha" ]] \
        || die "--rerun-failed требует состояние для того же коммита $sha, а не для другого"
    clean_tree || die '--rerun-failed требует чистого дерева: рабочая копия грязная'
    prev_started=$(state_value "$prev" started)
    [[ "$(state_value "$prev" seams)" == 1 ]] && seams_active=1
    mapfile -t done_ids < <(state_values "$prev" rerun_done)
    mapfile -t prev_flaky < <(state_values "$prev" flaky)
    mapfile -t prev_rust < <(state_values "$prev" rust_fail)
    mapfile -t prev_deploy < <(state_values "$prev" deploy_fail)
    mapfile -t prev_pw < <(state_values "$prev" pw_fail)
    mapfile -t prev_stack < <(state_values "$prev" stack_fail)
    npm_status=$(state_value "$prev" npm_ci_exit); npm_s=$(state_value "$prev" npm_ci_duration_s)
    check_status=$(state_value "$prev" check_exit); check_s=$(state_value "$prev" check_duration_s)
    deploy_exit=$(state_value "$prev" deploy_exit); deploy_s=$(state_value "$prev" deploy_duration_s)
    web_status=$(state_value "$prev" web_exit); web_s=$(state_value "$prev" web_duration_s)
else
    npm_status=0 npm_s=0 check_status=0 check_s=0 deploy_exit=skipped deploy_s=0 web_status=0 web_s=0
fi

# Что перезапускаем: skip — прошедший шаг берём из состояния, full — весь шаг,
# tests/suites/stack — только упавшие тесты.
npm_action=full check_action=full deploy_action=full web_action=full
check_sel="" deploy_sel="" web_sel=""
if ((rerun)); then
    [[ "$npm_status" == 0 ]] && npm_action=skip || npm_action=full
    if [[ "$check_status" == 0 ]]; then
        check_action=skip
    elif [[ "$(state_value "$prev" check_failed)" == tests ]]; then
        check_action=tests
    else
        check_action=full
    fi
    if [[ "$deploy_exit" == 0 || "$deploy_exit" == skipped ]]; then
        deploy_action=skip
    elif [[ "$(state_value "$prev" deploy_failed)" == tests ]]; then
        deploy_action=tests
    else
        deploy_action=full
    fi
    if [[ "$web_status" == 0 ]]; then
        web_action=skip
    elif [[ "$(state_value "$prev" web_failed)" == tests ]]; then
        web_action=tests
    else
        web_action=full
    fi

    if [[ "$check_action" == tests ]]; then to_rerun+=("${prev_rust[@]}"); fi
    if [[ "$deploy_action" == tests ]]; then to_rerun+=("${prev_deploy[@]}"); fi
    if [[ "$web_action" == tests ]]; then to_rerun+=("${prev_pw[@]}" "${prev_stack[@]}"); fi
    for id in "${to_rerun[@]}"; do
        if in_array "$id" "${done_ids[@]}"; then
            die "тест $id уже перезапускался на этом коммите: повторный перезапуск запрещён"
        fi
    done
fi

# Точный фильтр nextest: binary_id(=<id>) & test(=<name>) для каждой идентичности.
nextest_quote() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    printf '%s' "$s"
}
if [[ "$check_action" == tests ]]; then
    for id in "${prev_rust[@]}"; do
        IFS='|' read -r _ binary name <<<"$id"
        check_sel+="${check_sel:+ | }(binary_id(=$(nextest_quote "$binary")) & test(=$(nextest_quote "$name")))"
    done
fi
if [[ "$deploy_action" == tests ]]; then
    for id in "${prev_deploy[@]}"; do
        deploy_sel+="${deploy_sel:+ }${id#deploy|}"
    done
fi
if [[ "$web_action" == tests ]]; then
    web_sel=$(printf '%s\n' "${prev_pw[@]}" "${prev_stack[@]}")
fi

# Куда шаги кладут machine-readable отчёты. JUnit nextest — фиксированный путь,
# отчёты Playwright задаются окружением и читаются конфигом web/playwright.config.ts.
# При перезапуске web отчёты идут в новые файлы, чтобы старые падения не попали в итог.
junit="target/nextest/default/junit.xml"
pw_report_base="$PWD/$log_dir/pw-report"
stack_report_base="$PWD/$log_dir/stack-report"
if [[ "$web_action" == tests ]]; then
    rerun_stamp=$(date -u +%Y%m%dT%H%M%SZ)
    pw_report_base="$PWD/$log_dir/pw-rerun-$rerun_stamp"
    stack_report_base="$PWD/$log_dir/stack-rerun-$rerun_stamp"
fi
export CANTERLOT_PW_JSON_REPORT="$pw_report_base"
export CANTERLOT_STACK_JSON_REPORT="$stack_report_base"

# --- запуск шага ---------------------------------------------------------------
# В обычном прогоне вызывает настоящий скрипт; в тесте — подменённую команду.
# Режим и выбор передаются подмене через окружение (GATE_STEP_*).
run_gate_step() {
    local kind=$1 cmd=$2 mode=$3
    shift 3
    local selection="$*"
    if [[ -n "$cmd" ]]; then
        GATE_STEP_KIND="$kind" GATE_STEP_MODE="$mode" GATE_STEP_SELECTION="$selection" \
            bash -c "$cmd" gate "$@"
    else
        case "$kind" in
            npm) (cd web && npm ci --no-audit --no-fund) ;;
            check)
                if [[ "$mode" == tests ]]; then
                    (
                        # shellcheck disable=SC1090 # окружение cargo из домашнего каталога разработчика
                        source ~/.cargo/env 2>/dev/null || true
                        export DATABASE_URL="${DATABASE_URL:-postgres://canterlot:canterlot@127.0.0.1:${CANTERLOT_TEST_PG_PORT}/canterlot}"
                        cargo nextest run --workspace --exclude canterlot-speech --all-features \
                            --profile default --test-threads "${CANTERLOT_TEST_THREADS:-$(nproc)}" \
                            -E "$1" --no-tests=fail
                    )
                else
                    bash scripts/check.sh --all --no-web
                fi
                ;;
            deploy)
                if [[ "$mode" == tests ]]; then
                    RUN_SUITES="$1" bash scripts/deploy/tests/run.sh
                else
                    bash scripts/deploy/tests/run.sh
                fi
                ;;
            web)
                if [[ "$mode" == tests ]]; then
                    (web_rerun "$1")
                else
                    bash scripts/check-web.sh --stack
                fi
                ;;
        esac
    fi
}

# Перезапуск только упавших Playwright-тестов и сквозных сценариев. Идентичности
# приходят строками pw|проект|файл:строка|заголовок и stack|файл:строка|заголовок.
# --grep здесь не годится: Playwright сверяет его со строкой «проект файл describe
# заголовок теги», соединённой пробелами, поэтому точный заголовок вроде «A › B» не
# совпадёт. Выбираем тесты по расположению из JSON-отчёта (file:line) — на том же
# коммите номера строк стабильны — и группируем все локации одного проекта в один
# вызов. У каждого вызова свой отчёт <база>-N.json, его читают pw_failures и
# missing_rerun_ids. После прогона missing_rerun_ids проверяет, что каждая идентичность
# реально выполнилась и прошла: пропущенная или пропущенная как skipped — провал.
web_rerun() {
    local selection=$1 id kind f1 f2 _ status=0 project
    local -A pw_loc=()
    local stack_sel=""
    [[ -f web/package-lock.json ]] || die 'нет web/package-lock.json'
    export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/canterlot-playwright}"
    export TOKIO_WORKER_THREADS="${TOKIO_WORKER_THREADS:-4}" RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-4}"
    export MOZ_TASKCONTROLLER_THREADCOUNT="${MOZ_TASKCONTROLLER_THREADCOUNT:-4}" GSETTINGS_BACKEND="${GSETTINGS_BACKEND:-memory}"
    export PORT="${PORT:-$(node -e "const s=require('net').createServer();s.listen(0,'127.0.0.1',()=>{console.log(s.address().port);s.close()})")}"
    export PATH="${HOME}/.cargo/bin:${PATH}"
    while IFS= read -r id; do
        [[ -n "$id" ]] || continue
        IFS='|' read -r kind f1 f2 _ <<<"$id"
        case "$kind" in
            pw) pw_loc[$f1]+="${pw_loc[$f1]:+ }$f2" ;;
            stack) stack_sel+="${stack_sel:+ }$f1" ;;
        esac
    done <<<"$selection"
    local -a locations=()
    local slot=0
    cd web
    for project in "${!pw_loc[@]}"; do
        slot=$((slot + 1))
        read -r -a locations <<<"${pw_loc[$project]}"
        CANTERLOT_PW_JSON_REPORT="$CANTERLOT_PW_JSON_REPORT-$slot" \
            npx --no-install playwright test "${locations[@]}" "--project=$project" || status=1
    done
    if [[ -n "$stack_sel" ]]; then
        read -r -a locations <<<"$stack_sel"
        CANTERLOT_STACK_RUN=1 npx --no-install playwright test "${locations[@]}" || status=1
    fi
    return "$status"
}

# --- разбор отчётов в идентичности упавших тестов ------------------------------
# Идентичности канонические: rust|binary|test, pw|project|file:line|title,
# deploy|suite, stack|file:line|title. Те же строки идут в состояние и в flaky=
# аттестации; файл:строка берётся из JSON-отчёта Playwright и служит селектором
# при перезапуске.
rust_failures() {
    [[ -f "$junit" ]] || return 0
    python3 - "$junit" <<'PY'
import sys
import xml.etree.ElementTree as ET
try:
    root = ET.parse(sys.argv[1]).getroot()
except Exception:
    sys.exit(0)
for suite in root.iter('testsuite'):
    binary = suite.get('name', '')
    for case in suite.iter('testcase'):
        if any(child.tag in ('failure', 'error') for child in case):
            print(f"rust|{binary}|{case.get('name', '')}")
PY
}

report_present() {
    local prefix=$1 slot
    [[ -f "$prefix.json" ]] && return 0
    for slot in 1 2 3 4 5 6 7 8; do
        [[ -f "$prefix-$slot.json" ]] && return 0
    done
    return 1
}

pw_failures() {
    local prefix=$1 kind=${2:-pw} f files=() slot
    [[ -f "$prefix.json" ]] && files+=("$prefix.json")
    for slot in 1 2 3 4 5 6 7 8; do
        [[ -f "$prefix-$slot.json" ]] && files+=("$prefix-$slot.json")
    done
    ((${#files[@]})) || return 0
    python3 - "$kind" "${files[@]}" <<'PY'
import json
import sys

kind = sys.argv[1]


def walk(suites, out):
    for suite in suites or []:
        for spec in suite.get('specs', []) or []:
            path = spec.get('file', '')
            if spec.get('line'):
                path = f"{path}:{spec['line']}"
            spec_title = spec.get('title', '')
            for test in spec.get('tests', []) or []:
                results = test.get('results', []) or []
                if any(r.get('status') in ('failed', 'timedOut', 'crashed') or r.get('error') for r in results):
                    project = test.get('projectName') or spec.get('projectName') or ''
                    title = test.get('title', '')
                    full = f"{spec_title} › {title}" if spec_title and title else (spec_title or title)
                    if kind == 'pw':
                        out.append(f"pw|{project}|{path}|{full}")
                    else:
                        out.append(f"stack|{path}|{full}")
        walk(suite.get('suites', []), out)


out = []
for path in sys.argv[2:]:
    try:
        with open(path) as handle:
            data = json.load(handle)
    except Exception:
        continue
    walk(data.get('suites', []), out)
seen = set()
for item in out:
    if item not in seen:
        seen.add(item)
        print(item)
PY
}

# Проверка перезапуска: печатает ожидаемые идентичности, которых нет среди
# действительно прошедших в новом JSON-отчёте. Пустой вывод — все прошли.
missing_rerun_ids() {
    local kind=$1 prefix=$2 expected=$3 f files=() slot
    [[ -s "$expected" ]] || return 0
    [[ -f "$prefix.json" ]] && files+=("$prefix.json")
    for slot in 1 2 3 4 5 6 7 8; do
        [[ -f "$prefix-$slot.json" ]] && files+=("$prefix-$slot.json")
    done
    python3 - "$kind" "$expected" "${files[@]}" <<'PY'
import json
import sys

kind, expected_path = sys.argv[1], sys.argv[2]
expected = [line.rstrip('\n') for line in open(expected_path) if line.strip()]
passed = set()


def walk(suites):
    for suite in suites or []:
        for spec in suite.get('specs', []) or []:
            path = spec.get('file', '')
            if spec.get('line'):
                path = f"{path}:{spec['line']}"
            spec_title = spec.get('title', '')
            for test in spec.get('tests', []) or []:
                statuses = [r.get('status') for r in test.get('results', []) or []]
                if 'passed' not in statuses or test.get('status') not in ('expected', 'flaky'):
                    continue
                title = test.get('title', '')
                full = f"{spec_title} › {title}" if spec_title and title else (spec_title or title)
                if kind == 'pw':
                    project = test.get('projectName') or spec.get('projectName') or ''
                    passed.add(f"pw|{project}|{path}|{full}")
                else:
                    passed.add(f"stack|{path}|{full}")
        walk(suite.get('suites', []))


for path in sys.argv[3:]:
    try:
        with open(path) as handle:
            walk(json.load(handle).get('suites', []))
    except Exception:
        continue
for item in expected:
    if item not in passed:
        print(item)
PY
}

deploy_failures() {
    local dir=$1 f rc name
    [[ -d "$dir" ]] || return 0
    for f in "$dir"/*.rc; do
        [[ -f "$f" ]] || continue
        read -r rc _ <"$f" || continue
        [[ "$rc" == 0 ]] && continue
        name=${f##*/}
        printf 'deploy|%s\n' "${name%.rc}"
    done
}

# --- состояние прогона ---------------------------------------------------------
declare -a state_lines=() check_ids=() deploy_ids=() pw_ids=() stack_ids=() all_failures=() flaky_ids=()
state_add() { state_lines+=("$1"$'\t'"$2"); }
write_state() {
    local tmp="$log_dir/state.tsv.$$"
    printf '%s\n' "${state_lines[@]}" >"$tmp"
    mv -f "$tmp" "$log_dir/state.tsv"
}
step_failure_kind() {
    local status=$1
    shift
    if [[ "$status" == 0 || "$status" == skipped ]]; then
        printf 'ok\n'
    elif (($#)); then
        printf 'tests\n'
    else
        printf 'other\n'
    fi
}

# --- npm ci: один раз, дальше check-web.sh видит свежий node_modules ---
if [[ "$npm_action" != skip ]]; then
    printf '\n==> npm ci\n'
    s=$SECONDS
    npm_status=0
    run_gate_step npm "$step_npm" full >"$log_dir/npm-ci.log" 2>&1 || npm_status=$?
    npm_s=$((SECONDS - s))
    printf '    npm ci: %ds, код %s\n' "$npm_s" "$npm_status"
else
    printf '\n==> npm ci: переиспользован прошедший шаг\n'
fi

# --- Rust, речь и версия без веб-половины ---
if [[ "$check_action" != skip ]]; then
    printf '\n==> scripts/check.sh --all --no-web (%s)\n' "$check_action"
    s=$SECONDS
    check_status=0
    run_gate_step check "$step_check" "$check_action" "$check_sel" >"$log_dir/check.log" 2>&1 || check_status=$?
    check_s=$((SECONDS - s))
    printf '    check.sh --all --no-web: %ds, код %s\n' "$check_s" "$check_status"
    mapfile -t check_ids < <(rust_failures)
else
    printf '\n==> scripts/check.sh --all --no-web: переиспользован прошедший шаг\n'
fi

# --- наборы scripts/deploy: отдельным шагом, не вместе с веб-половиной ---
printf '\n==> scripts/deploy/tests/run.sh\n'
if [[ "$deploy_action" == skip ]]; then
    printf '    deploy tests: переиспользован прошедший шаг (%s)\n' "$deploy_exit"
elif ! deploy_needed; then
    printf '    deploy tests: skipped (no changes since the last release tag)\n'
else
    s=$SECONDS
    deploy_status=0
    mkdir -p "$log_dir/deploy"
    if [[ "$deploy_action" == tests ]]; then
        RUN_LOG_DIR="$PWD/$log_dir/deploy" run_gate_step deploy "$step_deploy" tests "$deploy_sel" \
            >"$log_dir/deploy.log" 2>&1 || deploy_status=$?
    else
        RUN_LOG_DIR="$PWD/$log_dir/deploy" run_gate_step deploy "$step_deploy" full \
            >"$log_dir/deploy.log" 2>&1 || deploy_status=$?
    fi
    deploy_exit=$deploy_status
    deploy_s=$((SECONDS - s))
    printf '    deploy tests: %ds, код %s\n' "$deploy_s" "$deploy_exit"
    mapfile -t deploy_ids < <(deploy_failures "$log_dir/deploy")
fi

# --- вся веб-половина и сквозные сценарии: один прогон ---
web_stack_missing=0
if [[ "$web_action" != skip ]]; then
    printf '\n==> scripts/check-web.sh --stack (%s)\n' "$web_action"
    s=$SECONDS
    web_status=0
    if [[ "$web_action" == tests ]]; then
        printf '%s\n' "${prev_pw[@]}" >"$log_dir/pw-expected.txt"
        printf '%s\n' "${prev_stack[@]}" >"$log_dir/stack-expected.txt"
    fi
    run_gate_step web "$step_web" "$web_action" "$web_sel" >"$log_dir/web.log" 2>&1 || web_status=$?
    web_s=$((SECONDS - s))
    printf '    check-web.sh --stack: %ds, код %s\n' "$web_s" "$web_status"
    mapfile -t pw_ids < <(pw_failures "$CANTERLOT_PW_JSON_REPORT")
    mapfile -t stack_ids < <(pw_failures "$CANTERLOT_STACK_JSON_REPORT" stack)
    # Без отчёта стека перезапуск одних упавших UI-тестов дал бы аттестацию без сквозных
    # сценариев: такое падение не «тесты», и перезапуск повторит весь веб.
    if [[ "$web_action" == full && "$web_status" != 0 ]] && ! report_present "$CANTERLOT_STACK_JSON_REPORT"; then
        web_stack_missing=1
        printf 'гейт: сквозные сценарии не запускались, перезапуск повторит весь веб\n' >&2
    fi
    if [[ "$web_action" == tests ]]; then
        mapfile -t pw_missing < <(missing_rerun_ids pw "$CANTERLOT_PW_JSON_REPORT" "$log_dir/pw-expected.txt")
        mapfile -t stack_missing < <(missing_rerun_ids stack "$CANTERLOT_STACK_JSON_REPORT" "$log_dir/stack-expected.txt")
        if ((${#pw_missing[@]} + ${#stack_missing[@]})); then
            printf 'гейт: перезапуск не подтвердил прохождение тестов (провалены, пропущены или не запущены):\n' >&2
            printf '  %s\n' "${pw_missing[@]}" "${stack_missing[@]}" >&2
            for id in "${pw_missing[@]}"; do pw_ids+=("$id"); done
            for id in "${stack_missing[@]}"; do stack_ids+=("$id"); done
            web_status=1
        fi
    fi
else
    printf '\n==> scripts/check-web.sh --stack: переиспользован прошедший шаг\n'
fi

# --- состояние и отчёт о нестабильных тестах ---
all_failures=("${check_ids[@]}" "${deploy_ids[@]}" "${pw_ids[@]}" "${stack_ids[@]}")
if [[ -n "$prev_started" ]]; then
    state_add started "$prev_started"
    state_add rerun_of "$prev_started"
else
    state_add started "$started_at"
fi
state_add schema 1
state_add sha "$sha"
state_add seams "$seams_active"
state_add finished "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
state_add completed 1
mode_name=full
((rerun)) && mode_name=rerun
state_add mode "$mode_name"
state_add npm_ci_exit "$npm_status"
state_add npm_ci_duration_s "$npm_s"
state_add npm_ci_failed "$(step_failure_kind "$npm_status")"
state_add check_exit "$check_status"
state_add check_duration_s "$check_s"
state_add check_failed "$(step_failure_kind "$check_status" "${check_ids[@]}")"
for id in "${check_ids[@]}"; do state_add rust_fail "$id"; done
state_add deploy_exit "$deploy_exit"
state_add deploy_duration_s "$deploy_s"
state_add deploy_failed "$(step_failure_kind "$deploy_exit" "${deploy_ids[@]}")"
for id in "${deploy_ids[@]}"; do state_add deploy_fail "$id"; done
state_add web_exit "$web_status"
state_add web_duration_s "$web_s"
web_kind=$(step_failure_kind "$web_status" "${pw_ids[@]}" "${stack_ids[@]}")
((web_stack_missing)) && [[ "$web_kind" == tests ]] && web_kind=other
state_add web_failed "$web_kind"
for id in "${pw_ids[@]}"; do state_add pw_fail "$id"; done
for id in "${stack_ids[@]}"; do state_add stack_fail "$id"; done
for id in "${done_ids[@]}" "${to_rerun[@]}"; do state_add rerun_done "$id"; done
for id in "${prev_flaky[@]}"; do
    state_add flaky "$id"
    in_array "$id" "${flaky_ids[@]}" || flaky_ids+=("$id")
done
for id in "${to_rerun[@]}"; do
    if in_array "$id" "${all_failures[@]}"; then continue; fi
    state_add flaky "$id"
    in_array "$id" "${flaky_ids[@]}" || flaky_ids+=("$id")
done
write_state

# Сводка в формате прежнего result.txt: коды и времена, затем nextest,
# Playwright и первые упавшие тесты.
mapfile -t rust_summary < <(grep -E 'Summary \[' "$log_dir/check.log" || true)
mapfile -t playwright_summary < <(grep -E '^\s+[0-9]+ (passed|failed|flaky|skipped|did not run)' "$log_dir/web.log" || true)
mapfile -t failed_rust < <(grep -E '^ +FAIL \[' "$log_dir/check.log" | head || true)
mapfile -t failed_web < <(grep -E '^\s+[0-9]+\) ' "$log_dir/web.log" | sort -u | head || true)

{
    printf '%s npm=%s check=%s %ss deploy=%s %ss web=%s %ss\n' \
        "$short" "$npm_status" "$check_status" "$check_s" "$deploy_exit" "$deploy_s" "$web_status" "$web_s"
    ((${#rust_summary[@]})) && printf '%s\n' "${rust_summary[@]}"
    ((${#playwright_summary[@]})) && printf '%s\n' "${playwright_summary[@]}"
    ((${#failed_rust[@]})) && printf '%s\n' "${failed_rust[@]}"
    ((${#failed_web[@]})) && printf '%s\n' "${failed_web[@]}"
    true
} >"$log_dir/result.txt"
cat "$log_dir/result.txt"
total_s=$((SECONDS - started))
printf 'гейт %s за %ds\n' "$short" "$total_s"

if ((npm_status != 0 || check_status != 0 || web_status != 0)) \
    || [[ "$deploy_exit" != 0 && "$deploy_exit" != skipped ]]; then
    printf 'гейт: провал, аттестация не пишется; логи в %s\n' "$log_dir" >&2
    exit 1
fi

if ((rerun)); then
    printf '\n==> отчёт о нестабильных тестах\n'
    if ((${#flaky_ids[@]})); then
        printf '  %s\n' "${flaky_ids[@]}"
    else
        printf '  нет\n'
    fi
    printf 'Каждый нестабильный тест должен получить строку в .superpowers/carry/flaky-tests.md (скрипт файл не меняет)\n'
fi

if ((!attest)); then
    printf 'гейт: успех без аттестации (--no-attest), логи в %s\n' "$log_dir"
    exit 0
fi

# Аттестация честна только для того же чистого коммита, что был в начале.
head_now=$(git rev-parse HEAD)
if [[ "$head_now" != "$sha" ]]; then
    printf 'гейт: HEAD сместился с %s на %s за прогон; аттестация не пишется\n' "$sha" "$head_now" >&2
    exit 1
fi
if ! clean_tree; then
    echo 'гейт: рабочая копия стала грязной за прогон; аттестация не пишется' >&2
    git status --short >&2 || true
    exit 1
fi

attestation=".superpowers/gate/$sha.ok"
tmp="$attestation.tmp.$$"
{
    printf 'sha=%s\n' "$sha"
    printf 'started=%s\n' "$started_at"
    printf 'finished=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'duration_s=%s\n' "$total_s"
    printf 'npm_ci_exit=%s\n' "$npm_status"
    printf 'npm_ci_duration_s=%s\n' "$npm_s"
    printf 'check_exit=%s\n' "$check_status"
    printf 'check_duration_s=%s\n' "$check_s"
    printf 'deploy_exit=%s\n' "$deploy_exit"
    printf 'deploy_duration_s=%s\n' "$deploy_s"
    printf 'web_exit=%s\n' "$web_status"
    printf 'web_duration_s=%s\n' "$web_s"
    printf 'logs=%s\n' "$log_dir"
    if ((seams_active)); then printf 'test_seams=1\n'; fi
    if ((rerun)); then
        printf 'rerun_of=%s\n' "$prev_started"
        for id in "${flaky_ids[@]}"; do printf 'flaky=%s\n' "$id"; done
    fi
    while IFS= read -r line; do
        printf 'summary=%s\n' "$line"
    done <"$log_dir/result.txt"
} >"$tmp"
mv -f "$tmp" "$attestation"
printf 'гейт: аттестация записана в %s\n' "$attestation"
