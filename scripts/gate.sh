#!/usr/bin/env bash
# Полный гейт перед выпуском. Заменяет частный /tmp/gate/run.sh и гоняет веб
# ровно один раз:
#   1. npm ci в web/ — один раз на весь прогон;
#   2. scripts/check.sh --all --no-web — версия, fmt, clippy, nextest, речь;
#   3. scripts/check-web.sh — вся веб-половина (статические проверки, vitest,
#      сборка PWA и vitest.build). Браузерные тесты удалены 22.11.2026.
#
#   scripts/gate.sh                  полный гейт с аттестацией .superpowers/gate/<full sha>.ok;
#                                    по умолчанию --runner prefer: первый доступный раннер,
#                                    иначе эта VM
#   scripts/gate.sh --rerun-failed   перезапустить только то, что упало на этом же коммите
#   scripts/gate.sh --no-attest      гейт без аттестации (нужен, если рабочая копия грязная)
#   scripts/gate.sh --runner prefer|local|auto|<машина>
#                                    prefer (по умолчанию) — первый доступный статический
#                                    раннер из конфига воркера, иначе эта VM; local — на этой
#                                    VM; auto или имя машины — на раннере через
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
#   CANTERLOT_WORKER_CONFIG конфиг воркера для prefer (по умолчанию
#                           $HOME/.config/canterlot/worker.toml);
#   CANTERLOT_GATE_RUNNER_PROBE
#                           команда пробы доступности раннера: ей передаётся ssh-алиас,
#                           код 0 означает доступен. Пусто — настоящий ssh; шов только
#                           для тестов, гейт всё равно идёт через настоящего воркера.
#
# Аттестация пишется только при полном успехе, чистой рабочей копии и неизменном HEAD:
# release.sh ship принимает только аттестацию ровно того коммита, который выкатывает.
#
# Гейт на раннере (--runner prefer|auto|<машина>). Нужна чистая рабочая копия: на раннер
# уходит ровно полный sha HEAD. prefer читает [[runners.ssh]] из конфига воркера и идёт
# на первого доступного статического раннера (проба ssh -o BatchMode=yes -o ConnectTimeout=5
# <ssh> true, через шов CANTERLOT_GATE_RUNNER_PROBE); облако prefer не выбирает — оно стоит
# денег. Нет конфига, записей или доступных раннеров — обычный гейт на этой VM. auto и явное
# имя машины сохраняют прежний смысл. Метка ядра kernel-6.12 больше не требуется. Скрипт зовёт
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
# --rerun-failed с явным --runner auto|<машина> не поддерживается. С prefer перезапуск идёт
# локально, если на этом коммите есть локальное состояние; без него — полный гейт через prefer.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
original_args=("$@")

usage() {
    echo 'Использование: scripts/gate.sh [--rerun-failed] [--no-attest] [--runner prefer|local|auto|<машина>]'
}

attest=1
rerun=0
runner=prefer
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
if [[ "$runner" != local && "$runner" != prefer && ! "$runner" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
    printf 'гейт: --runner: ожидается prefer, local, auto или имя машины, а не «%s»\n' "$runner" >&2
    exit 2
fi
if [[ "$runner" != local && "$runner" != prefer ]] && ((rerun)); then
    printf 'гейт: --rerun-failed не поддерживается вместе с --runner auto|<машина> (v1): гейт на раннере идёт целиком; уберите --rerun-failed или --runner\n' >&2
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
        --lease gate --timeout-minutes 240 --expected-minutes 45 \
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

# --- выбор места для --runner prefer -------------------------------------------------
# prefer смотрит статических раннеров из конфига воркера в порядке записей и идёт на
# первого доступного. Облако не рассматривается: оно стоит денег. Проба идёт через шов
# CANTERLOT_GATE_RUNNER_PROBE, чтобы тесты обходились без сети; в обычном прогоне шов пуст.
worker_config="${CANTERLOT_WORKER_CONFIG:-$HOME/.config/canterlot/worker.toml}"
runner_probe="${CANTERLOT_GATE_RUNNER_PROBE:-}"

runner_reachable() {
    local ssh=$1
    if [[ -n "$runner_probe" ]]; then
        bash -c "$runner_probe" gate-probe "$ssh" </dev/null >/dev/null 2>&1
    else
        # -n: без него ssh съест stdin цикла, и следующие раннеры из конфига не проверятся.
        ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$ssh" true >/dev/null 2>&1
    fi
}

choose_prefer_runner() {
    local cfg="$worker_config" entries="" machine ssh
    if [[ -f "$cfg" ]]; then
        # Как в scripts/deploy/verify.sh: tomllib, только имена machine и ssh, без секретов.
        if ! entries=$(python3 -I - "$cfg" <<'PY'
import sys, tomllib
try:
    cfg = tomllib.load(open(sys.argv[1], "rb"))
except (OSError, tomllib.TOMLDecodeError) as exc:
    print(f"cannot parse worker config: {type(exc).__name__}", file=sys.stderr)
    sys.exit(1)
for entry in cfg.get("runners", {}).get("ssh", []) or []:
    machine, alias = entry.get("machine", ""), entry.get("ssh", "")
    if machine and alias:
        print(f"{machine}\t{alias}")
PY
        ); then
            printf 'гейт: не удалось прочитать %s — гейт идёт на этой машине\n' "$cfg" >&2
            printf '%s\n' local
            return 0
        fi
    fi
    while IFS=$'\t' read -r machine ssh; do
        [[ -n "$machine" && -n "$ssh" ]] || continue
        if runner_reachable "$ssh"; then
            printf 'гейт: раннер %s доступен — гейт идёт на нём\n' "$machine" >&2
            printf '%s\n' "$machine"
            return 0
        fi
    done <<<"$entries"
    printf 'гейт: раннеров нет или они недоступны — гейт идёт на этой машине\n' >&2
    printf '%s\n' local
}

if [[ "$runner" == prefer ]]; then
    # Состояние предыдущего прогона лежит только локально: результаты раннера — в
    # .superpowers/gate/<short>/runner/. Есть состояние — перезапускаем на этой машине;
    # нет — повторяем полный гейт через prefer, чтобы обычная пара вызовов работала и
    # после первого прогона на раннере.
    prefer_sha=$(git rev-parse HEAD)
    prefer_short=${prefer_sha:0:8}
    if ((rerun)) && [[ -f ".superpowers/gate/$prefer_short/state.tsv" ]]; then
        printf 'гейт: есть локальное состояние этого коммита — --rerun-failed идёт на этой машине\n'
        runner=local
    else
        if ((rerun)); then
            printf 'гейт: локального состояния этого коммита нет — --rerun-failed запускает полный гейт через prefer\n'
            rerun=0
        fi
        runner=$(choose_prefer_runner)
    fi
fi

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
declare -a done_ids=() prev_flaky=() prev_rust=() prev_deploy=() to_rerun=()
declare -A flaky_pass=()
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
    # Третье поле строки flaky — проход, в котором тест упал (pass-<n>); в состоянии,
    # записанном до появления логов проходов, его нет.
    while IFS=$'\t' read -r id pass_name; do
        [[ -z "$id" || -z "${pass_name:-}" ]] || flaky_pass["$id"]=$pass_name
    done < <(awk -F '\t' '$1=="flaky"{print $2 "\t" $3}' "$prev")
    mapfile -t prev_rust < <(state_values "$prev" rust_fail)
    mapfile -t prev_deploy < <(state_values "$prev" deploy_fail)
    npm_status=$(state_value "$prev" npm_ci_exit); npm_s=$(state_value "$prev" npm_ci_duration_s)
    check_status=$(state_value "$prev" check_exit); check_s=$(state_value "$prev" check_duration_s)
    deploy_exit=$(state_value "$prev" deploy_exit); deploy_s=$(state_value "$prev" deploy_duration_s)
    web_status=$(state_value "$prev" web_exit); web_s=$(state_value "$prev" web_duration_s)
else
    npm_status=0 npm_s=0 check_status=0 check_s=0 deploy_exit=skipped deploy_s=0 web_status=0 web_s=0
fi

# Что перезапускаем: skip — прошедший шаг берём из состояния, full — весь шаг,
# tests — только упавшие тесты Rust и наборов выката.
npm_action=full check_action=full deploy_action=full web_action=full
check_sel="" deploy_sel=""
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
    else
        web_action=full
    fi

    if [[ "$check_action" == tests ]]; then to_rerun+=("${prev_rust[@]}"); fi
    if [[ "$deploy_action" == tests ]]; then to_rerun+=("${prev_deploy[@]}"); fi
    for id in "${to_rerun[@]}"; do
        if in_array "$id" "${done_ids[@]}"; then
            die "тест $id уже перезапускался на этом коммите: повторный перезапуск запрещён"
        fi
    done
fi

# --- логи предыдущего прохода ----------------------------------------------------
# Следующий проход пишет в те же файлы. Пока он ничего не записал, копия логов шагов,
# result.txt, failures.txt и state.tsv предыдущего прохода уходит в pass-<n>/ (n = 1, 2, ...).
# Копия, а не перенос: шаг, который проход пропускает, оставляет свой лог на верхнем уровне
# (из него собирается result.txt), а читатели вроде release.sh и аттестации смотрят только
# на верхний уровень, где всегда лежит последний проход.
prev_pass=""
archive_previous_pass() {
    local n=1 f
    while [[ -e "$log_dir/pass-$n" ]]; do n=$((n + 1)); done
    for f in npm-ci.log check.log deploy.log web.log result.txt failures.txt state.tsv deploy; do
        [[ -e "$log_dir/$f" ]] || continue
        [[ -n "$prev_pass" ]] || { prev_pass=pass-$n; mkdir -p "$log_dir/$prev_pass"; }
        cp -a "$log_dir/$f" "$log_dir/$prev_pass/$f"
    done
    rm -f "$log_dir/failures.txt"
}
archive_previous_pass

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
# Куда шаги кладут machine-readable отчёты: JUnit nextest — фиксированный путь.
junit="target/nextest/default/junit.xml"

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
            web) bash scripts/check-web.sh ;;
        esac
    fi
}

# --- разбор отчётов в идентичности упавших тестов ------------------------------
# Идентичности канонические: rust|binary|test и deploy|suite. Те же строки идут
# в состояние и в flaky= аттестации при перезапуске.
rust_failures() {
    # Упавшие тесты берутся из JUnit и из check.log. Одного JUnit мало: check.sh --all после
    # воркспейса гоняет canterlot-speech с тем же профилем, и тот перезаписывает junit.xml
    # (упавшие тесты воркспейса тогда видны только в логе).
    python3 -I - "$junit" "$log_dir/check.log" <<'PY'
import os
import re
import sys
import xml.etree.ElementTree as ET

junit, log = sys.argv[1:3]
seen = []


def add(ident):
    if ident not in seen:
        seen.append(ident)


if os.path.isfile(junit):
    try:
        root = ET.parse(junit).getroot()
    except Exception:
        root = None
    if root is not None:
        for suite in root.iter('testsuite'):
            binary = suite.get('name', '')
            for case in suite.iter('testcase'):
                if any(child.tag in ('failure', 'error') for child in case):
                    add(f"rust|{binary}|{case.get('name', '')}")

# Строка итога nextest: «FAIL [   0.028s] (4385/5330) <binary id> <тест>». TRY n — попытка
# с повтором, это не итог.
final = re.compile(
    r'^\s+(?:FAIL|LEAK-FAIL|TIMEOUT|EXEC-FAIL|ABORT|SIG[A-Z0-9]+) \[\s*[\d.]+s\]\s+'
    r'(?:\(\s*\d+/\s*\d+\)\s+)?(\S+) (\S.*?)\s*$')
if os.path.isfile(log):
    with open(log, errors='replace') as fh:
        for line in fh:
            m = final.match(line.rstrip('\n'))
            if m:
                add(f"rust|{m.group(1)}|{m.group(2)}")
for ident in seen:
    print(ident)
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

# --- причины падений ------------------------------------------------------------
# failure_reasons <check.log> <каталог deploy> <тестов не больше> <строк на тест> <id>...
# Для каждого теста печатает «FAIL <binary> <тест>» и строки паники из его вывода в check.log
# (блок `--- STDERR:` / `--- STDOUT:` важнее встроенного вывода под строкой FAIL; при отсутствии
# паники — первые строки вывода). Набор выката берёт строки FAIL из deploy/<набор>.log.
failure_reasons() {
    python3 -I - "$@" <<'PY'
import os
import re
import sys

log, deploy_dir, max_tests, max_lines = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
ids = sys.argv[5:]
lines = []
if os.path.isfile(log):
    with open(log, errors='replace') as fh:
        lines = [x.rstrip('\n') for x in fh]

# Граница вывода: строка статуса nextest («FAIL [ 0.028s] (1/2) bin test», «Summary [ 1s] ...»),
# заголовок итогового блока или линейка.
status = re.compile(r'^\s*(?:TRY \d+ )?[A-Z][A-Za-z-]*(?: \d+(?:/\d+)?)? \[\s*[\d.]+s\]\s*(?:\(\s*\d+/\s*\d+\)\s*)?(.*)$')
block = re.compile(r'^--- (STDOUT|STDERR):\s+(.*?)\s+---\s*$')
rule = re.compile(r'^\s*-{8,}\s*$')

sections = []  # (ключ «binary тест», итоговый ли блок, строки)
cur = None
for line in lines:
    m_status, m_block = status.match(line), block.match(line)
    if m_status or m_block or rule.match(line):
        cur = None
        if m_block:
            cur = (' '.join(m_block.group(2).split()), True, [])
        elif m_status and not re.match(r'^\s*(?:TRY \d+ )?(?:PASS|SKIP|Summary|SLOW|FLAKY|Starting|LEAK)\b', line):
            cur = (' '.join(m_status.group(1).split()), False, [])
        if cur:
            sections.append(cur)
    elif cur is not None:
        cur[2].append(line)


def panic_lines(body):
    out, grab = [], False
    for line in body:
        text = line.strip()
        if 'panicked at' in line:
            grab = True
        elif not text or text.startswith(('note:', 'stack backtrace')):
            grab = False
        if grab:
            out.append(line)
    return out


def reasons(key):
    mine = [sec for sec in sections if sec[0] == key]
    for want_block in (True, False):
        picked = []
        for sec in mine:
            if sec[1] == want_block:
                for line in panic_lines(sec[2]):
                    if line.strip() and line not in picked:
                        picked.append(line)
        if picked:
            return picked
    for sec in mine:
        body = [x for x in sec[2] if x.strip()]
        if body:
            return body
    return None


def dedent(body):
    pad = min(len(x) - len(x.lstrip()) for x in body)
    return [x[pad:] for x in body]


def emit(title, body):
    print(f'  {title}')
    if not body:
        print('    (вывода этого теста в логе нет)')
        return
    body = dedent(body)
    for line in body[:max_lines]:
        print(f'    {line}')
    if len(body) > max_lines:
        print(f'    ... ещё строк: {len(body) - max_lines}')


for ident in ids[:max_tests]:
    kind, _, rest = ident.partition('|')
    if kind == 'deploy':
        path = os.path.join(deploy_dir, rest + '.log')
        body = []
        if os.path.isfile(path):
            with open(path, errors='replace') as fh:
                text = [x.rstrip('\n') for x in fh if x.strip()]
            body = [x for x in text if x.startswith('FAIL')] or text[-max_lines:]
        emit(f'FAIL набор выката {rest}', body)
    else:
        binary, _, name = rest.partition('|')
        emit(f'FAIL {binary} {name}', reasons(f'{binary} {name}'))
if len(ids) > max_tests:
    print(f'  ... и ещё {len(ids) - max_tests} упавших тестов (см. {log})')
PY
}

# --- состояние прогона ---------------------------------------------------------
declare -a state_lines=() check_ids=() deploy_ids=() all_failures=() flaky_ids=()
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
    # Чужой отчёт прежнего прогона не должен сойти за отчёт этого (например, если упал fmt).
    rm -f "$junit"
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

# --- вся веб-половина: один прогон ---
if [[ "$web_action" != skip ]]; then
    printf '\n==> scripts/check-web.sh (%s)\n' "$web_action"
    s=$SECONDS
    web_status=0
    run_gate_step web "$step_web" "$web_action" >"$log_dir/web.log" 2>&1 || web_status=$?
    web_s=$((SECONDS - s))
    printf '    check-web.sh: %ds, код %s\n' "$web_s" "$web_status"
else
    printf '\n==> scripts/check-web.sh: переиспользован прошедший шаг\n'
fi

# --- состояние и отчёт о нестабильных тестах ---
all_failures=("${check_ids[@]}" "${deploy_ids[@]}")
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
state_add web_failed "$(step_failure_kind "$web_status")"
for id in "${done_ids[@]}" "${to_rerun[@]}"; do state_add rerun_done "$id"; done
for id in "${prev_flaky[@]}"; do
    state_add flaky "$id${flaky_pass[$id]:+$'\t'${flaky_pass[$id]}}"
    in_array "$id" "${flaky_ids[@]}" || flaky_ids+=("$id")
done
for id in "${to_rerun[@]}"; do
    if in_array "$id" "${all_failures[@]}"; then continue; fi
    # Тест упал в предыдущем проходе, чьи логи только что ушли в $prev_pass.
    [[ -n "${flaky_pass[$id]:-}" || -z "$prev_pass" ]] || flaky_pass["$id"]=$prev_pass
    state_add flaky "$id${flaky_pass[$id]:+$'\t'${flaky_pass[$id]}}"
    in_array "$id" "${flaky_ids[@]}" || flaky_ids+=("$id")
done
write_state

# Сводка в формате прежнего result.txt: коды и времена, затем nextest
# и первые упавшие тесты.
mapfile -t rust_summary < <(grep -E 'Summary \[' "$log_dir/check.log" || true)
mapfile -t failed_rust < <(grep -E '^ +FAIL \[' "$log_dir/check.log" | head || true)

{
    printf '%s npm=%s check=%s %ss deploy=%s %ss web=%s %ss\n' \
        "$short" "$npm_status" "$check_status" "$check_s" "$deploy_exit" "$deploy_s" "$web_status" "$web_s"
    ((${#rust_summary[@]})) && printf '%s\n' "${rust_summary[@]}"
    ((${#failed_rust[@]})) && printf '%s\n' "${failed_rust[@]}"
    true
} >"$log_dir/result.txt"
cat "$log_dir/result.txt"
if ((${#all_failures[@]})); then
    {
        printf '\n==> причины падений (%s)\n' "$log_dir/check.log"
        failure_reasons "$log_dir/check.log" "$log_dir/deploy" 5 12 "${all_failures[@]}"
    } | tee "$log_dir/failures.txt"
fi
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
        for id in "${flaky_ids[@]}"; do
            printf '  %s\n' "$id"
            if [[ -n "${flaky_pass[$id]:-}" ]]; then
                printf '    лог прохода, где тест упал: %s\n' "$log_dir/${flaky_pass[$id]}/check.log"
                failure_reasons "$log_dir/${flaky_pass[$id]}/check.log" "$log_dir/${flaky_pass[$id]}/deploy" 1 12 "$id" | tail -n +2
            else
                printf '    лог прохода, где тест упал, не записан (состояние старого формата)\n'
            fi
        done
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
