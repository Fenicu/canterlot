#!/usr/bin/env bash
# Общие помощники выката: логи, SSH без чтения stdin, проверки только чтением.
# Подключается через: source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
set -euo pipefail
# Детерминированный порядок сортировки везде (урок: sort зависит от локали).
export LC_ALL=C

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091 # deploy.conf lives next to lib.sh, path is dynamic
source "${DEPLOY_DIR}/deploy.conf"

# Журнал и ошибки — по-английски; комментарии — по-русски.
log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
die() { printf '[%s] FATAL: %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; exit 1; }

# Все вызовы ssh только с -n либо с явным конвейером (урок: ssh ел stdin).
# Все вызовы docker только с </dev/null (урок: docker без stdin виснет в скрипте).
ssh_hub() { ssh -n -o BatchMode=yes -o ConnectTimeout=10 "${HUB_SSH}" "$@" </dev/null; }
ssh_web() { ssh -n -o BatchMode=yes -o ConnectTimeout=10 "${WEB_SSH}" "$@" </dev/null; }
ssh_pmx() { ssh -n -o BatchMode=yes -o ConnectTimeout=10 "${PROXMOX_SSH}" "$@" </dev/null; }
# Приём tar-конвейера: stdin — это поток tar (явный конвейер, не -n).
# Вызывающий строит конвейер сам: tar -C dir -cf - . | recv_to HOST 'tar -C dest -xf -'
recv_to() { ssh -o BatchMode=yes -o ConnectTimeout=30 "$1" "$2"; }

# Безопасная цитата для удалённого shell: каждый аргумент, попадающий в строку
# ssh-команды, обязан пройти через shq (printf %q). Имена релизов дополнительно
# валидируются (is_release_id), но цитата нужна и для путей/имён юнитов.
shq() {
    local s="$1"
    s="${s//\'/\'\\\'\'}"
    printf "'%s'" "${s}"
}

# Один advisory-lock на хост, в корне выката: flip/start/retention на этом хосте
# не могут пересечься с проверкой и удалением. Держится одной удалённой командой;
# критический участок передаётся скриптом, а значения — позиционными аргументами
# (никакой интерполяции значений в shell-текст).
HOST_LOCK_WAIT="${HOST_LOCK_WAIT:-120}"
_quote_args() {
    local out="" arg
    for arg in "$@"; do printf -v out '%s %q' "${out}" "${arg}"; done
    printf '%s' "${out}"
}
# Текст команды «взять lock и выполнить script с позиционными аргументами»:
# одна и та же форма для исполнения скриптом и для печати ручных команд оператору.
locked_cmd_text() {
    local lock="$1" script="$2"; shift 2
    printf 'flock -o -w %s %s bash -c %s --%s' "${HOST_LOCK_WAIT}" "$(shq "${lock}")" "$(shq "${script}")" "$(_quote_args "$@")"
}
# Текст удалённой команды без lock: bash -c <script> -- <args>. Только чтения.
script_cmd_text() {
    local script="$1"; shift
    printf 'bash -c %s --%s' "$(shq "${script}")" "$(_quote_args "$@")"
}
hub_locked() {
    local script="$1"; shift
    ssh_hub "$(locked_cmd_text "${HUB_BASE}/.deploy.lock" "${script}" "$@")"
}
web_locked() {
    local script="$1"; shift
    ssh_web "$(locked_cmd_text "${WEB_SITES_BASE}/.deploy.lock" "${script}" "$@")"
}
worker_locked() {
    local script="$1"; shift
    flock -o -w "${HOST_LOCK_WAIT}" "${WORKER_BASE}/.deploy.lock" bash -c "${script}" -- "$@"
}

# Flip хаба: цель перепроверяется ВНУТРИ lock (каталог мог исчезнуть после ранней
# проверки), и только потом ссылка заменяется атомарно. $1 корень, $2 релиз,
# $3 путь current. Печатает целевую ссылку после flip.
# shellcheck disable=SC2016,SC2034 # удалённый скрипт раскрывает свои позиционные аргументы; константа используется в cutover/rollback-db
HUB_FLIP_SCRIPT='cd "$1" && test -x "releases/$2/bin/canterlot-hub" && test -f "releases/$2/RELEASE" && test -f "releases/$2/SHA256SUMS" && ln -s "releases/$2" current.new && mv -T current.new current && readlink "$3"'
# Печатаемый ручной текст ssh-команды: host + одна цитированная удалённая команда.
ssh_cmd_text() { printf 'ssh %s %s' "$1" "$(shq "$2")"; }

# Резервирование релизов на время операции (cutover/rollback-db): файл
# <корень выката>/.reserved.<release> под тем же lock. retain не удаляет
# зарезервированный релиз, так что проверка цели и flip не расходятся с чисткой.
# Резервы снимаются явно по завершении операции (reservations_release).
# Маркер хранит, кто и когда его поставил: run=<id> pid=<pid> started=<epoch>.
# pid — процесс операции на машине воркера (все прогоны идут там), поэтому retain
# отличает живой прогон от брошенного. Старые маркеры без содержимого оцениваются
# по mtime файла.
# shellcheck disable=SC2016 # удалённый скрипт раскрывает свои позиционные аргументы
RESERVE_SCRIPT='b="$1"; run="$2"; pid="$3"; started="$4"; shift 4; for n in "$@"; do printf "run=%s pid=%s started=%s\n" "$run" "$pid" "$started" >"${b}/.reserved.${n}"; done'
# shellcheck disable=SC2016,SC2034 # используется в cutover (печатаемая команда)
UNRESERVE_SCRIPT='b="$1"; shift; for n in "$@"; do rm -f "${b}/.reserved.${n}"; done'
reserve_releases() {
    local kind="$1"; shift
    if [ -z "${DEPLOY_RUN_ID:-}" ]; then
        DEPLOY_RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
    fi
    if [ -z "${DEPLOY_RUN_PID:-}" ]; then
        DEPLOY_RUN_PID="$$"
    fi
    if [ -z "${DEPLOY_RUN_STARTED:-}" ]; then
        DEPLOY_RUN_STARTED="$(date -u +%s)"
    fi
    case "${kind}" in
        hub) hub_locked "${RESERVE_SCRIPT}" "${HUB_BASE}" "${DEPLOY_RUN_ID}" "${DEPLOY_RUN_PID}" "${DEPLOY_RUN_STARTED}" "$@" ;;
        web) web_locked "${RESERVE_SCRIPT}" "${WEB_SITES_BASE}" "${DEPLOY_RUN_ID}" "${DEPLOY_RUN_PID}" "${DEPLOY_RUN_STARTED}" "$@" ;;
        worker) worker_locked "${RESERVE_SCRIPT}" "${WORKER_BASE}" "${DEPLOY_RUN_ID}" "${DEPLOY_RUN_PID}" "${DEPLOY_RUN_STARTED}" "$@" ;;
        *) die "unknown reservation kind: ${kind}" ;;
    esac
}
# Снять устаревший резерв под тем же lock: только если содержимое маркера не
# изменилось с момента чтения. Иначе прогон успел перезаписать его своим —
# не трогаем и отказываем кодом 6 (это «занято», а не ошибка).
# shellcheck disable=SC2016,SC2034 # используется в retain (снятие устаревшего резерва)
CLEAR_RESERVATION_SCRIPT='b="$1"; n="$2"; want="$3"; f="${b}/.reserved.${n}"; [ -e "$f" ] || exit 0; [ "$(cat "$f")" = "$want" ] || exit 6; rm -f "$f"'
# Чтение резервов хоста (только чтение): строки "release|mtime|content". content —
# одна строка, которую пишет RESERVE_SCRIPT; у старых прогонов она пустая.
# shellcheck disable=SC2016 # удалённый скрипт раскрывает свои позиционные аргументы
RESERVATIONS_SCRIPT='b="$1"; for f in "$b"/.reserved.*; do [ -f "$f" ] || continue; n="${f##*/.reserved.}"; m="$(stat -c %Y "$f" 2>/dev/null || echo 0)"; c="$(tr -d "\n" <"$f" 2>/dev/null || true)"; printf "%s|%s|%s\n" "$n" "$m" "$c"; done'
reservation_lines() {
    case "$1" in
        hub) ssh_hub "$(script_cmd_text "${RESERVATIONS_SCRIPT}" "${HUB_BASE}")" ;;
        web) ssh_web "$(script_cmd_text "${RESERVATIONS_SCRIPT}" "${WEB_SITES_BASE}")" ;;
        worker) bash -c "${RESERVATIONS_SCRIPT}" -- "${WORKER_BASE}" ;;
        *) die "unknown reservation kind: $1" ;;
    esac
}
unreserve_releases() {
    local kind="$1"; shift
    case "${kind}" in
        hub) hub_locked "${UNRESERVE_SCRIPT}" "${HUB_BASE}" "$@" ;;
        web) web_locked "${UNRESERVE_SCRIPT}" "${WEB_SITES_BASE}" "$@" ;;
        worker) worker_locked "${UNRESERVE_SCRIPT}" "${WORKER_BASE}" "$@" ;;
        *) die "unknown reservation kind: ${kind}" ;;
    esac
}

# Строка $1 есть среди строк $2 (точное совпадение). Без конвейера в grep -q:
# ранний выход grep под pipefail давал SIGPIPE слева (урок 1.19.0).
in_lines() {
    case $'\n'"$2"$'\n' in
        *$'\n'"$1"$'\n'*) return 0 ;;
    esac
    return 1
}

# Короткий sha (12 знаков) из полного или короткого.
short_sha() { printf '%s' "$1" | cut -c1-12; }

# Режим записи. real — боевой прогон; rehearse — репетиция: каждая запись идёт
# через run_write и только печатается, а все чтения, ожидания и разборы
# выполняются по-настоящему. Единственная точка выхода записи (урок 1.16-1.19:
# пять выкатов подряд падали на пути записи, который dry-run не исполнял).
# Правило tests/pipe-lint.py: ни одна пишущая команда в скриптах выката не
# обходит run_write.
DEPLOY_MODE="${DEPLOY_MODE:-real}"
# Счётчик напечатанных записей репетиции — в файле: run_write зовут и из $(…).
REHEARSE_COUNT_FILE="${REHEARSE_COUNT_FILE:-}"
_rehearse_note() {
    local label="$1" n=0
    shift
    if [ -n "${REHEARSE_COUNT_FILE}" ]; then
        printf '%s\n' "${label}" >>"${REHEARSE_COUNT_FILE}"
        n="$(wc -l <"${REHEARSE_COUNT_FILE}" 2>/dev/null || echo 0)"
    fi
    printf '[%s] WOULD-WRITE #%s (%s):%s\n' "$(date -u +%H:%M:%S)" "${n}" "${label}" "$(_quote_args "$@")" >&2
}
run_write() {
    local label="$1"
    shift
    case "${DEPLOY_MODE}" in
        real) "$@" ;;
        rehearse) _rehearse_note "${label}" "$@" ;;
        *) die "unknown DEPLOY_MODE=${DEPLOY_MODE}" ;;
    esac
}
# То же для записи, которой данные приходят в stdin (конвейер printf/tar | …):
# в репетиции stdin дочитывается до конца, чтобы производитель слева не умер
# от SIGPIPE (pipefail превратил бы это в 141).
run_write_pipe() {
    local label="$1"
    shift
    case "${DEPLOY_MODE}" in
        real) "$@" ;;
        rehearse) cat >/dev/null; _rehearse_note "${label}" "$@" ;;
        *) die "unknown DEPLOY_MODE=${DEPLOY_MODE}" ;;
    esac
}
is_rehearse() { [ "${DEPLOY_MODE}" = "rehearse" ]; }

# Токен подтверждения боевого прогона: печатают dry-run и репетиция, боевой
# прогон принимает его CONFIRM_TTL секунд (решение владельца: 2 часа) и только
# для того же релиза и того же плана. Формат <epoch>-<план>-<проверка>: план —
# первые 12 знаков sha256 канонического текста плана (plan_text), проверка
# связывает epoch, план и id релиза. Это не секрет, а осознанный барьер между
# «посмотрел план» и «остановил прод»: запуск без свежего dry-run или с другим
# планом (сдвинулся развёрнутый релиз, изменился dist, конфигурация или сам
# toolkit) отказывает с понятной причиной.
CONFIRM_TTL="${CONFIRM_TTL:-7200}"
confirm_token() {
    local id="$1" plan="$2" epoch="$3" check
    check="$(printf '%s:%s:%s' "${id}" "${plan}" "${epoch}" | sha256sum | cut -c1-8)"
    printf '%s-%s-%s' "${epoch}" "${plan}" "${check}"
}
# Проверка токена: печатает причину отказа и возвращает 1; 0 — токен годен.
confirm_token_check() {
    local token="$1" id="$2" plan="$3" now="${4:-}" epoch tplan age issued
    [ -n "${now}" ] || now="$(date -u +%s)"
    if ! [[ "${token}" =~ ^([0-9]{9,11})-([0-9a-f]{12})-([0-9a-f]{8})$ ]]; then
        printf 'malformed --confirm token (expected <epoch>-<plan>-<check> as printed by --dry-run or --rehearse)'
        return 1
    fi
    epoch="${BASH_REMATCH[1]}"
    tplan="${BASH_REMATCH[2]}"
    if [ "$(confirm_token "${id}" "${tplan}" "${epoch}")" != "${token}" ]; then
        printf 'the --confirm token was not issued for release %s (or was altered)' "${id}"
        return 1
    fi
    issued="$(date -u -d "@${epoch}" '+%Y-%m-%d %H:%M:%S UTC' 2>/dev/null || echo "${epoch}")"
    age=$((now - epoch))
    if [ "${age}" -lt -300 ]; then
        printf 'the --confirm token is dated in the future (%s): check the clock' "${issued}"
        return 1
    fi
    if [ "${age}" -gt "${CONFIRM_TTL}" ]; then
        printf 'the --confirm token expired: issued %s, %s min ago; a token is valid for %s min after its dry-run or rehearsal: run one again' \
            "${issued}" "$((age / 60))" "$((CONFIRM_TTL / 60))"
        return 1
    fi
    if [ "${tplan}" != "${plan}" ]; then
        printf 'the plan changed since the token was issued (token plan %s, current plan %s: deployed releases, dist, config or toolkit differ): run --dry-run or --rehearse again' \
            "${tplan}" "${plan}"
        return 1
    fi
    return 0
}
# Хэш файла для плана: 12 знаков sha256 или missing.
_plan_file_hash() {
    if [ -f "$1" ]; then sha256sum <"$1" | cut -c1-12; else printf 'missing'; fi
}
# Канонический текст плана выката, общий у dry-run, репетиции и боевого прогона:
# зависит только от цели, развёрнутой базы, содержимого dist, конфигурации выката
# и текста самого toolkit. $1 цель, $2 каталог dist/<цель>, $3..$5 развёрнутые
# hub web worker.
plan_text() {
    local sha="$1" dist_dir="$2" b_hub="$3" b_web="$4" b_worker="$5" f toolkit
    toolkit="$(for f in cutover.sh lib.sh verify.sh retain.sh preflight.sh target-check.sh deploy.conf; do
        printf '%s %s\n' "$(_plan_file_hash "${DEPLOY_DIR}/${f}")" "${f}"
    done | sha256sum | cut -c1-12)"
    printf '%s\n' \
        "plan v3 target=${sha}" \
        "baseline hub=${b_hub} web=${b_web} worker=${b_worker}" \
        "dist SHA256SUMS=$(_plan_file_hash "${dist_dir}/SHA256SUMS") RELEASE=$(_plan_file_hash "${dist_dir}/RELEASE") EXPECT=$(_plan_file_hash "${dist_dir}/EXPECT") csp=$(_plan_file_hash "${dist_dir}/csp.txt")" \
        "hosts hub=${HUB_SSH} web=${WEB_SSH} proxmox=${PROXMOX_SSH} node=${PROXMOX_NODE} ct=${HUB_CTID} ip=${HUB_IP}" \
        "paths hub=${HUB_BASE} web=${WEB_SITES_BASE} env=${WEB_ENV} worker=${WORKER_BASE} backups=${HUB_BACKUP_DIR} units=${WORKER_UNIT},${HUB_UNIT},${WEB_CONTAINER},${SPEECH_UNIT}" \
        "toolkit=${toolkit} retain=${RETAIN_COUNT}" \
        "steps:" \
        "  1 preflight snapshot (local state file) and target check (staged trees, dist, DB not ahead); reserve ${sha} and the previous releases on every host" \
        "  2 fresh vzdump of CT ${HUB_CTID} while hub and worker run (notes pre-${sha}-<ts>); the control worker is exactly the unit MainPID" \
        "  3 read gates and the session inventory again (vzdump takes up to a minute)" \
        "  4 stop worker ${WORKER_UNIT} (sessions keep running); prove no control worker is left" \
        "  5 quiesced pg_dump canterlot-pre-${sha}-<ts>.dump, verified readable" \
        "  6 hub flip ${b_hub} -> ${sha} under the host lock, one restart of ${HUB_UNIT}; readyz, migrations, exe, NRestarts" \
        "  7 worker flip ${b_worker} -> ${sha}, start; guarded MainPID/exe wait; state=live wait; NRestarts; downtime printed" \
        "  8 PWA (soft, after the worker is live, never stops it): canterlot.env.${sha} from dist csp.txt, build flip ${b_web} -> ${sha}, one recreate of ${WEB_CONTAINER}" \
        "  9 speech: restart ${SPEECH_UNIT} (bounded to ${SPEECH_RESTART_TIMEOUT} s) and ask health when installed, enabled and configured; stop and disable it when ${sha} has no canterlot-speech; every speech failure is soft" \
        " 10 merge old _app/immutable chunks of the previous web releases into ${sha} (no clobber), verify dist files intact" \
        " 11 maintenance check (sockets, exec dirs, leftovers, env record); verify.sh twice incl. loopback Caddy probe and harness catalog" \
        " 12 geoip: canterlot-hub geoip update on the hub when [auth.geoip].path (canterlot-hub geoip path) is missing or older than ${GEOIP_MAX_AGE_DAYS} days (soft)" \
        " 13 retention: keep ${RETAIN_COUNT} per component + every live release; literal paths re-checked right before deletion"
}
plan_hash() { plan_text "$@" | sha256sum | cut -c1-12; }

# Последнее состояние сессии воркера в журнале юнита после метки since.
# Журнал печатает голое state="live" (метка времени — префикс journald), поэтому
# ищем именно поле state=, а не time="…" после него (урок 1.16.0). Возврат:
# 0 — последнее состояние live; 1 — иное/нет строк; 2 — журнал не читается.
# Никаких `[ … ] && break` под set -e: только явные if.
live_from_journal() {
    local since="$1" max_wait="${2:-60}" waited=0 wlog last
    while :; do
        if wlog="$(journalctl --user -u "${WORKER_UNIT}" --since "${since}" --no-pager 2>/dev/null)"; then
            last="$(printf '%s\n' "${wlog}" | grep -o 'state="[a-z_]*"' | tail -1 | sed 's/state="//; s/"//' || true)"
            if [ "${last}" = "live" ]; then
                return 0
            fi
        else
            last="unreadable"
        fi
        if [ "${waited}" -ge "${max_wait}" ]; then
            printf '%s' "${last:-none}"
            return 1
        fi
        sleep "${LIVE_POLL:-1}"
        waited=$((waited + 1))
    done
}

# --- ожидание процесса воркера после старта (урок 1.18.1) ---
# Пока юнит в activating, MainPID бывает пустым или 0, а readlink /proc/<pid>/exe
# падает с ENOENT на переходе exec. Каждое чтение здесь защищено: функция никогда
# не роняет вызывающего под set -e, а только возвращает пустое значение.
# Репетиция умеет подмешать эти состояния (START_WAIT_FAULT_DIR, только в
# DEPLOY_MODE=rehearse): файлы pid-empty / pid-zero / exe-enoent в каталоге
# содержат число оставшихся поддельных ответов. Счётчик — в файлах, потому что
# функции вызываются из $(…).
_take_fault() {
    local f="${START_WAIT_FAULT_DIR:-}/$1" n
    is_rehearse || return 1
    [ -n "${START_WAIT_FAULT_DIR:-}" ] && [ -f "${f}" ] || return 1
    n="$(cat "${f}" 2>/dev/null || echo 0)"
    case "${n}" in '' | *[!0-9]*) return 1 ;; esac
    [ "${n}" -gt 0 ] || return 1
    printf '%s\n' "$((n - 1))" >"${f}"
    printf '1\n' >>"${START_WAIT_FAULT_DIR}/calls-$1"
    return 0
}
# MainPID юнита воркера: число без мусора или пусто.
unit_main_pid() {
    local v=""
    if _take_fault pid-empty; then return 0; fi
    if _take_fault pid-zero; then printf '0'; return 0; fi
    v="$(systemctl --user show "${WORKER_UNIT}" -p MainPID --value 2>/dev/null)" || v=""
    case "${v}" in '' | *[!0-9]*) v="" ;; esac
    printf '%s' "${v}"
}
# Цель /proc/<pid>/exe или пусто (процесс исчез, переход exec, нет прав).
proc_exe() {
    local v=""
    if _take_fault exe-enoent; then return 0; fi
    v="$(readlink "/proc/$1/exe" 2>/dev/null)" || v=""
    printf '%s' "${v}"
}
# Ждать, пока у юнита появится MainPID с читаемым exe. $1 — число опросов
# (по START_POLL секунд). Ставит WAIT_PID и WAIT_EXE; 0 — найдено, 1 — нет.
START_POLL="${START_POLL:-0.5}"
# shellcheck disable=SC2034 # WAIT_PID/WAIT_EXE читает вызывающий
wait_worker_exe() {
    local max="${1:-60}" i=0 pid="" exe=""
    WAIT_PID=""
    WAIT_EXE=""
    while [ "${i}" -lt "${max}" ]; do
        pid="$(unit_main_pid)" || pid=""
        if [ -n "${pid}" ] && [ "${pid}" != "0" ]; then
            exe="$(proc_exe "${pid}")" || exe=""
            if [ -n "${exe}" ]; then
                WAIT_PID="${pid}"
                WAIT_EXE="${exe}"
                return 0
            fi
        fi
        sleep "${START_POLL}"
        i=$((i + 1))
    done
    WAIT_PID="${pid}"
    return 1
}

# Инвариант живой сессии: каждый ЗАПИСАННЫЙ процесс (sup/cli/relay) обязан быть
# жив и иметь тот же стартовый тик. Множество НЕ обязано совпадать: новые
# короткоживущие дети (вызовы инструментов, второй CLI-процесс на миг) допустимы
# и только печатаются. Строка worker=... в снимке — отдельное множество и здесь
# не проверяется (урок 1.17.0: равенство 15 строк падало на транзиентном ребёнке).
snapshot_drift() {
    local file="$1" label="$2" bad=0 line exec_id role old old_pid old_tick new_tick new_cli new_pid
    [ -f "${file}" ] || { printf 'snapshot file missing: %s\n' "${file}" >&2; return 1; }
    while IFS= read -r line; do
        case "${line}" in
            worker\ * | "") continue ;;
        esac
        exec_id="${line%% *}"
        for role in sup cli relay; do
            for old in $(snapshot_role_values "${line}" "${role}"); do
                old_pid="${old%%:*}"
                old_tick="${old##*:}"
                if ! proc_is_live "${old_pid}"; then
                    printf 'recorded session process gone (%s): %s %s %s\n' "${label}" "${exec_id}" "${role}" "${old_pid}" >&2
                    bad=1
                    continue
                fi
                new_tick="$(proc_start_tick "${old_pid}")"
                if [ "${new_tick}" != "${old_tick}" ]; then
                    printf 'recorded session tick changed (%s): %s %s %s (%s -> %s)\n' \
                        "${label}" "${exec_id}" "${role}" "${old_pid}" "${old_tick}" "${new_tick:-gone}" >&2
                    bad=1
                fi
            done
        done
        # Новые дети — только к сведению. Записанный CLI остаётся главным; если
        # рядом появился второй ребёнок (например, инструмент), это не дрейф.
        new_cli="$(session_cli_pid "$(printf '%s' "${line}" | grep -o ' sup=[0-9]*' | cut -d= -f2 | sed -n 1p)" 2>/dev/null || true)"
        if [ -n "${new_cli}" ]; then
            new_pid="${new_cli%% *}"
            case "${line}" in
                *"cli=${new_pid}:"*) ;;
                *) printf 'new transient CLI child (%s): %s pid %s (allowed)\n' "${label}" "${exec_id}" "${new_pid}" >&2 ;;
            esac
        fi
    done <"${file}"
    return "${bad}"
}

# Сравнение SHA-идентификаторов разной длины: релизные каталоги именуются
# 12 символами, а version.json сборки может содержать короткую форму того же
# коммита. Совпадением считается равенство или префиксность (минимум 7 символов,
# чтобы не матчить пустое/мусор).
sha_match() {
    local a="$1" b="$2" n
    [ -n "${a}" ] && [ -n "${b}" ] || return 1
    n=${#a}; [ "${#b}" -lt "${n}" ] && n="${#b}"
    [ "${n}" -ge 7 ] || return 1
    [ "${a:0:${n}}" = "${b:0:${n}}" ]
}
# Release id: ровно 12 или 40 hex-символов. Всё остальное (включая имена,
# пришедшие с удалённого хоста) — громкий отказ: имя релиза никогда не должно
# попадать в shell-текст без такой проверки.
is_release_id() {
    [[ "${1}" =~ ^[0-9a-f]{12}$ || "${1}" =~ ^[0-9a-f]{40}$ ]]
}
require_release_id() {
    is_release_id "$1" || die "invalid release id: '${1}' (expected 12 or 40 hex characters)"
}

# Текущий задеплоенный релиз компонента по ссылке current/build.
# Пустой ответ (обрыв ssh, битая ссылка) — громкая смерть, а не пустая строка,
# которую вызывающий прочитал бы как «всё хорошо».
deployed_sha() {
    local out
    case "$1" in
        hub) out="$(ssh_hub "readlink '${HUB_CURRENT}'" </dev/null | sed 's|.*/||')" ;;
        web) out="$(ssh_web "readlink '${WEB_BUILD_LINK}'" </dev/null | sed 's|.*/||')" ;;
        worker) out="$(readlink "${WORKER_CURRENT}" | sed 's|.*/||')" ;;
        *) die "unknown component: $1" ;;
    esac
    [ -n "${out}" ] || die "cannot read current release for $1"
    # Имя пришло с хоста (или из файловой системы): до shell-текста оно доходит
    # только после проверки формата.
    require_release_id "${out}"
    printf '%s' "${out}"
}

# Ни один управляющий процесс прод-воркера не должен жить. Кандидат — процесс, у
# которого basename exe ИЛИ argv[0] равен canterlot-worker (бинарь мог быть удалён
# из релиза после запуска: тогда /proc/pid/exe кончается на " (deleted)", а argv[0]
# сохраняет имя). Тот же бинарь запускает супервизор и mcp-relay сессий, а также
# клиент аренды тяжёлых задач (`lease run` вокруг сборок и проверок): их argv[1] —
# точная подкоманда, и только эти три можно пропустить. Клиент аренды переживает
# остановку воркера: 30 с ждёт его и продолжает команду без аренды.
#
# Прод-воркер отличается от тестовых (стек-тесты делегатов запускают
# <worktree>/target/debug/canterlot-worker run против одноразового хаба, выкат
# 1.23.2) путём бинаря и конфигом. Кандидат считается прод-воркером, если:
#   - exe (без " (deleted)") лежит под корнем установки WORKER_BASE: current и
#     releases/<sha>, включая удалённый после старта релиз;
#   - или абсолютный argv[0] лежит под WORKER_BASE;
#   - или это `run` с --config, указывающим на прод-конфиг WORKER_CONFIG, откуда
#     бы ни был запущен бинарь (копия бинаря на прод-конфиге — тот же воркер);
#   - или классифицировать нельзя: exe не читается, а argv[0] не абсолютный.
# Остальные кандидаты (worktree, target/, /tmp) не мешают выкату; их pid и путь
# бинаря (без прочих аргументов) печатаются одной строкой в stderr.
# Возврат 0 и описание на stdout — найден (все найденные через "; "); 1 — нет.
# Тесты направляют WORKER_BASE и
# WORKER_CONFIG в фейковый корень, поэтому настоящий прод-воркер рядом им не виден.
path_under() {
    local path="$1" root="$2"
    [ -n "${path}" ] && [ -n "${root}" ] || return 1
    case "${path}" in "${root}"/*) return 0 ;; esac
    return 1
}
_worker_root_match() {
    local path="$1" roots="$2" root
    for root in ${roots}; do
        path_under "${path}" "${root}" && return 0
    done
    return 1
}
# Значение --config у `run` (форма "--config X" и "--config=X"), относительный путь
# разрешается от cwd процесса. Печатает нормализованный путь или ничего.
_worker_run_config() {
    local pid="$1"; shift
    local prev="" arg cfg="" cwd
    for arg in "$@"; do
        case "${arg}" in
            --config=*) cfg="${arg#--config=}" ;;
            *) if [ "${prev}" = "--config" ]; then cfg="${arg}"; fi ;;
        esac
        prev="${arg}"
    done
    [ -n "${cfg}" ] || return 0
    case "${cfg}" in
        /*) ;;
        *) cwd="$(readlink "/proc/${pid}/cwd" 2>/dev/null)" || return 0
           cfg="${cwd}/${cfg}" ;;
    esac
    readlink -m "${cfg}"
}
find_control_worker() {
    local proc_dir proc_pid exe_target exe_base argv0_base proc_argv
    local roots base_real conf_real cfg found="" ignored="" why
    base_real="$(readlink -m "${WORKER_BASE}")"
    roots="${WORKER_BASE}"
    [ "${base_real}" = "${WORKER_BASE}" ] || roots="${roots} ${base_real}"
    conf_real="$(readlink -m "${WORKER_CONFIG}")"
    for proc_dir in /proc/[0-9]*; do
        proc_pid="${proc_dir#/proc/}"
        exe_target="$(readlink "${proc_dir}/exe" 2>/dev/null || true)"
        case "${exe_target}" in
            *" (deleted)") exe_target="${exe_target% (deleted)}" ;;
        esac
        proc_argv=()
        [ -r "${proc_dir}/cmdline" ] || continue
        mapfile -d '' -t proc_argv <"${proc_dir}/cmdline" 2>/dev/null || true
        [ "${#proc_argv[@]}" -gt 0 ] || continue
        exe_base="${exe_target##*/}"
        argv0_base="${proc_argv[0]##*/}"
        if [ "${exe_base}" != "canterlot-worker" ] && [ "${argv0_base}" != "canterlot-worker" ]; then
            continue
        fi
        case "${proc_argv[1]:-}" in
            supervise|mcp-relay|lease) continue ;;
        esac
        why=""
        if _worker_root_match "${exe_target}" "${roots}"; then
            why="exe"
        elif case "${proc_argv[0]}" in /*) true ;; *) false ;; esac \
            && { _worker_root_match "${proc_argv[0]}" "${roots}" \
                || _worker_root_match "$(readlink -m "${proc_argv[0]}")" "${roots}"; }; then
            why="argv0"
        elif [ "${proc_argv[1]:-}" = "run" ] \
            && cfg="$(_worker_run_config "${proc_pid}" "${proc_argv[@]:2}")" \
            && [ -n "${cfg}" ] && [ "${cfg}" = "${conf_real}" ]; then
            why="config"
        elif [ -z "${exe_target}" ]; then
            case "${proc_argv[0]}" in /*) ;; *) why="unclassifiable" ;; esac
        fi
        if [ -z "${why}" ]; then
            ignored="${ignored:+${ignored}, }${proc_pid} ${exe_target:-${proc_argv[0]}}"
            continue
        fi
        found="${found:+${found}; }worker process ${proc_pid} still runs ${exe_target:-${proc_argv[0]}} (argv[1]=${proc_argv[1]:-none}, matched by ${why})"
    done
    if [ -n "${ignored}" ]; then
        log "control-worker scan: ignoring non-production canterlot-worker processes outside ${WORKER_BASE} and not on ${WORKER_CONFIG}: ${ignored}" >&2
    fi
    [ -n "${found}" ] || return 1
    printf '%s' "${found}"
}

# Версия PWA внутри контейнера Caddy: голый sha без JSON-обёртки.
# Пустой ответ (контейнер молчит, файла нет) — пустая строка, не UNKNOWN-ловушка.
container_pwa_sha() {
    ssh_web "docker exec caddy cat '${WEB_CONTAINER_VERSION_JSON}'" 2>/dev/null \
        | sed -n 's/.*"version"[ ]*:[ ]*"\([0-9a-zA-Z]\+\)".*/\1/p'
}

# Зомби — не живой процесс: сопровождение-зомби нельзя считать ни живым,
# ни мёртвым (его /proc ещё виден, тики заморожены). Пропускаем везде.
is_zombie() {
    local statline state
    { read -r statline <"/proc/$1/stat"; } 2>/dev/null || return 0
    state="${statline##*) }"
    state="${state%% *}"
    [ "${state}" = "Z" ]
}

# Живой процесс: /proc есть и это не зомби. Зомби держит замороженный тик
# и без этой проверки проходил бы сверку тиков мёртвым (урок: sleep-супервизоры
# в тестах не reap'ят детей; в проде залипший супервизор — тоже сигнал).
proc_is_live() { [ -d "/proc/$1" ] && ! is_zombie "$1"; }

# PIDs и стартовые тики процессов: только /proc, без pgrep -f (урок: pgrep -f ловил сам себя).
# Печатает строки "pid starttime comm". Перечитывание через cat: процесс может
# умереть между glob и чтением, ошибка чтения глушится и процесс пропускается.
proc_ticks() {
    local pattern="$1" d pid tick
    local -a argv=()
    for d in /proc/[0-9]*; do
        pid="${d#/proc/}"
        is_zombie "${pid}" && continue
        # NUL-разделённый cmdline читается в массив без cat|tr: на машине с сотнями
        # процессов это единственный заметный источник fork'ов в горячем пути.
        argv=()
        mapfile -d '' -t argv 2>/dev/null <"${d}/cmdline" || true
        [ "${#argv[@]}" -gt 0 ] || continue
        # Встроенное сравнение ([[ ]]), без fork: внешний grep видел бы образец
        # в собственной командной строке и ловил сам себя.
        [[ " ${argv[*]} " == *"${pattern}"* ]] || continue
        tick="$(proc_start_tick "${pid}")"
        # Печатаются только argv[0] и подкоманда argv[1]: остальные аргументы (пути,
        # сокеты, идентификаторы) в вывод не попадают, сопоставление выше шло по полной строке.
        printf '%s %s %s\n' "${pid}" "${tick:-?}" "${argv[0]}${argv[1]:+ ${argv[1]}}"
    done
}

# Проверка отсутствия процесса по шаблону на удалённом хосте через ps.
# Первый символ образца — в скобках (урок: pgrep -f и /proc-скан ловили сам сканер:
# его командная строка содержит образец буквально, а "[v]zdump" — нет).
# Сам ps выполняется без конвейера: провал осмотра — это код 2, а не пустой
# вывод, который читался бы как «процесса нет». Фильтрация — локальный grep,
# и только её «ничего не нашлось» глушится.
# Возврат 0 — процесса нет, 1 — процесс есть, 2 — сам осмотр не удался.
assert_no_remote_proc() {
    local host="$1" pattern="$2" label="$3"
    local re out
    re="[${pattern:0:1}]${pattern:1}"
    if ! out="$(ssh -n -o BatchMode=yes -o ConnectTimeout=10 "${host}" "ps -eo pid=,args=" </dev/null)"; then
        printf 'cannot inspect %s on %s (ssh/ps failed)\n' "${label}" "${host}" >&2
        return 2
    fi
    out="$(printf '%s' "${out}" | grep -v 'sshd:' | grep -- "${re}" || true)"
    if [ -n "${out}" ]; then
        # Только pid и argv[0]: командные строки чужих процессов не печатаются.
        printf 'found %s:\n%s\n' "${label}" "$(printf '%s\n' "${out}" | awk '{print $1, $2}')" >&2
        return 1
    fi
    return 0
}

# Проверка дампа перед flip хаба: TOC читается (статус pg_restore проверяется
# отдельно, без конвейера) и весь дамп распаковывается в /dev/null.
# Возвращает 0 только если обе проверки прошли; 1 — иначе.
verify_dump() {
    local dump="$1" toc
    if ! toc="$(ssh_hub "runuser -u postgres -- pg_restore --list '${dump}'")"; then
        printf 'pg_restore --list failed for %s\n' "${dump}" >&2
        return 1
    fi
    # Без конвейера: grep -q на большом TOC закрывает pipe раньше, и pipefail
    # превращал бы успешную проверку в SIGPIPE (141).
    case "${toc}" in
        *_sqlx_migrations*) ;;
        *)
            printf 'dump TOC has no _sqlx_migrations: %s\n' "${dump}" >&2
            return 1 ;;
    esac
    if ! ssh_hub "runuser -u postgres -- pg_restore -f /dev/null '${dump}'"; then
        printf 'full pg_restore read failed for %s\n' "${dump}" >&2
        return 1
    fi
    return 0
}

# Нода CT по кластерному списку ресурсов (vmid -> node): имя из конфигурации ничему
# не доверено. Печатает имя ноды. Возврат 0 — найдено; 1 — CT нет в списке;
# 2 — сам запрос или разбор провалился.
discover_ct_node() {
    local out
    out="$(ssh_pmx "pvesh get /cluster/resources --type vm --output-format json")" || return 2
    printf '%s' "${out}" | python3 -I -c '
import json, sys
vmid = int(sys.argv[1])
try:
    rows = json.load(sys.stdin)
except Exception:
    sys.exit(2)
nodes = sorted({r.get("node") for r in rows if r.get("vmid") == vmid and r.get("node")})
if len(nodes) != 1:
    sys.exit(1)
print(nodes[0])
' "${HUB_CTID}"
}

# Проверка ноды CT перед запросом блокировки. Печатает причину в stderr и возвращает
# 1, если нода не найдена или не совпадает с PROXMOX_NODE; при успехе печатает имя ноды.
check_ct_node() {
    local found rc=0
    found="$(discover_ct_node)" || rc=$?
    if [ "${rc}" -ne 0 ] || [ -z "${found}" ]; then
        printf 'cannot discover the node of CT %s (pvesh get /cluster/resources failed or CT not listed, rc=%s)\n' "${HUB_CTID}" "${rc}" >&2
        return 1
    fi
    if [ "${found}" != "${PROXMOX_NODE}" ]; then
        printf 'PROXMOX_NODE=%s but CT %s runs on node %s: fix PROXMOX_NODE (deploy.conf) before trusting any lock query\n' "${PROXMOX_NODE}" "${HUB_CTID}" "${found}" >&2
        return 1
    fi
    printf 'CT %s runs on node %s\n' "${HUB_CTID}" "${found}"
}

# Счётчик жёстких и мягких сбоев для cutover: жёсткие откатывают, мягкие пишутся и идут дальше.
HARD_FAILURES=0
SOFT_FAILURES=0
hard() { printf '[%s] HARD-FAIL: %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; HARD_FAILURES=$((HARD_FAILURES + 1)); }
soft() {
    printf '[%s] SOFT-FAIL: %s (recorded, continuing)\n' "$(date -u +%H:%M:%S)" "$*" >&2
    SOFT_FAILURES=$((SOFT_FAILURES + 1))
    soft_record "${PHASE:-${DEPLOY_SOFT_KIND:-other}}"
}
# Файл мягких ошибок прогона cutover (DEPLOY_SOFT_LOG): строка — вид шага; его
# наследуют verify.sh и retain.sh, так что их ошибки тоже попадают в результат.
soft_record() {
    if [ -n "${DEPLOY_SOFT_LOG:-}" ]; then printf '%s\n' "$1" >>"${DEPLOY_SOFT_LOG}" 2>/dev/null || true; fi
}
check_summary() {
    log "hard failures: ${HARD_FAILURES}, soft failures: ${SOFT_FAILURES}"
    [ "${HARD_FAILURES}" -eq 0 ]
}

# GeoIP на хосте хаба: возраст файла базы в секундах или missing. Только чтение.
# shellcheck disable=SC2016 # удалённый скрипт раскрывает свои позиционные аргументы
GEOIP_AGE_SCRIPT='if [ -f "$1" ]; then echo $(( $(date +%s) - $(stat -c %Y "$1") )); else echo missing; fi'
geoip_age() { ssh_hub "$(script_cmd_text "${GEOIP_AGE_SCRIPT}" "$1")"; }
# Путь базы спрашивается у хаба ($1 — его бинарь): тот же [auth.geoip].path, что откроет
# хаб, поэтому выкат и хаб не смотрят в разные файлы. HUB_GEOIP_PATH подменяет ответ (тесты).
geoip_path() {
    local path
    if [ -n "${HUB_GEOIP_PATH:-}" ]; then printf '%s' "${HUB_GEOIP_PATH}"; return 0; fi
    path="$(ssh_hub "runuser -u canterlot -- $(shq "$1") geoip path --config $(shq "${HUB_CONFIG}")")" || return 1
    case "${path}" in
        *$'\n'* | '') return 1 ;;
        /*) printf '%s' "${path}" ;;
        *) return 1 ;;
    esac
}
# Решение без записи: печатает причину; 0 — обновить, 1 — свежая, 2 — не прочитать.
# $1 — бинарь хаба (по умолчанию текущий релиз).
geoip_due() {
    local bin="${1:-${HUB_CURRENT}/bin/canterlot-hub}" path age limit=$((GEOIP_MAX_AGE_DAYS * 86400))
    path="$(geoip_path "${bin}" 2>/dev/null)" || { printf 'cannot ask %s for [auth.geoip].path' "${bin}"; return 2; }
    age="$(geoip_age "${path}" 2>/dev/null)" || { printf 'cannot read %s on the hub' "${path}"; return 2; }
    case "${age}" in
        missing) printf '%s is missing' "${path}"; return 0 ;;
        '' | *[!0-9-]*) printf 'unexpected age of %s: %s' "${path}" "${age}"; return 2 ;;
    esac
    if [ "${age}" -gt "${limit}" ]; then
        printf '%s is %s days old (more than %s)' "${path}" "$((age / 86400))" "${GEOIP_MAX_AGE_DAYS}"
        return 0
    fi
    printf '%s is %s days old' "${path}" "$((age / 86400))"
    return 1
}
# Обновление базы после выката хаба. Любая неудача мягкая: места входа просто пусты.
# $1 — бинарь хаба (по умолчанию текущий релиз).
geoip_refresh() {
    local bin="${1:-${HUB_CURRENT}/bin/canterlot-hub}" why rc=0
    why="$(geoip_due "${bin}")" || rc=$?
    case "${rc}" in
        1) log "geoip: ${why}, no update"; return 0 ;;
        2) soft "geoip: ${why}; sign-in locations stay empty"; return 0 ;;
    esac
    log "geoip: ${why}: running canterlot-hub geoip update"
    if run_write "geoip update" ssh_hub "runuser -u canterlot -- $(shq "${bin}") geoip update --config $(shq "${HUB_CONFIG}")"; then
        log "geoip: update done"
    else
        soft "geoip update failed; sign-in locations stay empty until a later run"
    fi
}

# --- служба распознавания речи (диктовка) ---
# Её сбой никогда не останавливает выкат и откат. Поэтому отдельный счётчик, а не
# soft(): мягкая ошибка в dry-run и репетиции отнимает токен боевого прогона.
SPEECH_ISSUES=0
# shellcheck disable=SC2034 # итог читают cutover, verify и rollback-db
SPEECH_SUMMARY="not checked"
speech_soft() {
    printf '[%s] SPEECH-SOFT: %s (dictation only, the release goes on)\n' "$(date -u +%H:%M:%S)" "$*" >&2
    SPEECH_ISSUES=$((SPEECH_ISSUES + 1))
    soft_record speech
}
# Виды WARN без базы, которые ждут: сбои движка службы (`engine_*`) и сбои
# диктовки в журнале хаба (`speech_engine_failed`, `speech_protocol_error`).
SPEECH_WARN_KINDS_SOFT="${SPEECH_WARN_KINDS_SOFT:-engine_load_failed engine_failed engine_exited speech_engine_failed speech_protocol_error}"
speech_warn_kind_soft() {
    local k
    for k in ${SPEECH_WARN_KINDS_SOFT}; do
        if [ "${k}" = "$1" ]; then return 0; fi
    done
    return 1
}
# Состояние на хосте хаба одним чтением: "load= active= enabled= config= binary=".
# $1 юнит, $2 speech.toml, $3 бинарь релиза.
# shellcheck disable=SC2016 # удалённый скрипт раскрывает свои позиционные аргументы
SPEECH_STATE_SCRIPT='s="$(systemctl show "$1" -p LoadState,ActiveState,UnitFileState 2>/dev/null)" || s=""; l="$(printf "%s\n" "$s" | sed -n "s/^LoadState=//p")"; a="$(printf "%s\n" "$s" | sed -n "s/^ActiveState=//p")"; e="$(printf "%s\n" "$s" | sed -n "s/^UnitFileState=//p")"; c=absent; if [ -f "$2" ]; then c=present; fi; b=absent; if [ -x "$3" ]; then b=present; fi; printf "load=%s active=%s enabled=%s config=%s binary=%s\n" "${l:-unknown}" "${a:-unknown}" "${e:-unknown}" "$c" "$b"'
speech_state() {
    ssh_hub "$(script_cmd_text "${SPEECH_STATE_SCRIPT}" "${SPEECH_UNIT}" "${SPEECH_CONFIG}" "$1")"
}
speech_field() { printf '%s\n' "$2" | tr ' ' '\n' | sed -n "s/^$1=//p"; }
# Что делать со службой для релиза хаба $1 (только чтение). Ставит SPEECH_ACTION:
# restart, disable (юнит есть, бинаря в релизе нет), skip (диктовка недоступна или
# выключена руками), unknown (состояние не прочитать); SPEECH_REASON — пояснение,
# SPEECH_STATE — строка состояния.
speech_plan() {
    local rel="$1" load enabled
    SPEECH_BIN="${HUB_RELEASES}/${rel}/bin/canterlot-speech"
    if ! SPEECH_STATE="$(speech_state "${SPEECH_BIN}" 2>/dev/null)" || [ -z "${SPEECH_STATE}" ]; then
        SPEECH_STATE="unreadable"
        SPEECH_ACTION=unknown
        SPEECH_REASON="cannot read the state of ${SPEECH_UNIT} on the hub"
        return 0
    fi
    load="$(speech_field load "${SPEECH_STATE}")"
    enabled="$(speech_field enabled "${SPEECH_STATE}")"
    if [ "${load}" != "loaded" ]; then
        SPEECH_ACTION=skip
        SPEECH_REASON="dictation unavailable: unit ${SPEECH_UNIT} is not installed"
    elif [ "$(speech_field binary "${SPEECH_STATE}")" != "present" ]; then
        SPEECH_ACTION=disable
        SPEECH_REASON="dictation unavailable: release ${rel} has no bin/canterlot-speech"
    elif [ "$(speech_field config "${SPEECH_STATE}")" != "present" ]; then
        SPEECH_ACTION=skip
        SPEECH_REASON="dictation unavailable: no ${SPEECH_CONFIG}"
    elif [ "${enabled}" != "enabled" ]; then
        SPEECH_ACTION=skip
        SPEECH_REASON="dictation switched off by hand: ${SPEECH_UNIT} is ${enabled}"
    else
        SPEECH_ACTION=restart
        SPEECH_REASON="${SPEECH_UNIT} installed and enabled, ${SPEECH_CONFIG} present, release ${rel} has the binary"
    fi
}
# Ответ health от имени пользователя хаба: так проверяется и доступ к сокету.
# Печатает вывод команды (JSON или строку speech_unavailable), код — её код.
# $2 — предел локального ssh, секунды (0 или пусто — без него).
speech_health() {
    local bin="$1" limit="${2:-0}" remote
    remote="runuser -u $(shq "${SPEECH_CLIENT_USER}") -- timeout 10 $(shq "${bin}") health --socket $(shq "${SPEECH_SOCKET}") 2>&1"
    if [ "${limit}" -gt 0 ]; then
        timeout "${limit}" ssh -n -o BatchMode=yes -o ConnectTimeout=10 "${HUB_SSH}" "${remote}" </dev/null
    else
        ssh_hub "${remote}"
    fi
}
# Ждать ok от health не дольше SPEECH_HEALTH_WAIT секунд по часам. Каждая попытка
# ограничена остатком бюджета (минимум 1 с), а вывод пишется в файл: иначе потомок
# медленной команды держал бы канал и ожидание выходило за бюджет. Ставит
# SPEECH_HEALTH_OUT.
speech_wait_health() {
    local bin="$1" deadline remaining out
    out="$(mktemp "${TMPDIR:-/tmp}/canterlot-speech-health.XXXXXX")" || return 1
    deadline=$(( $(date +%s) + SPEECH_HEALTH_WAIT ))
    while :; do
        remaining=$(( deadline - $(date +%s) ))
        [ "${remaining}" -ge 1 ] || remaining=1
        : >"${out}"
        if speech_health "${bin}" "${remaining}" >"${out}" 2>&1; then
            SPEECH_HEALTH_OUT="$(cat "${out}")" || SPEECH_HEALTH_OUT=""
            rm -f "${out}" # lint: local-temp
            return 0
        fi
        SPEECH_HEALTH_OUT="$(cat "${out}" 2>/dev/null)" || SPEECH_HEALTH_OUT=""
        if [ "$(date +%s)" -ge "${deadline}" ]; then
            rm -f "${out}" # lint: local-temp
            return 1
        fi
        sleep 1
    done
}
# Привести службу к релизу хаба $1 после его flip (cutover, откат). Всё мягкое:
# ничего не возвращает с ошибкой и не выходит. Ставит SPEECH_SUMMARY.
# shellcheck disable=SC2034 # SPEECH_SUMMARY читает вызывающий
speech_sync() {
    local rel="$1" rc=0 after
    speech_plan "${rel}"
    log "speech: ${SPEECH_UNIT}: ${SPEECH_STATE}"
    case "${SPEECH_ACTION}" in
        unknown)
            speech_soft "${SPEECH_REASON}"
            SPEECH_SUMMARY="unknown (${SPEECH_REASON})"
            return 0 ;;
        skip)
            log "speech: ${SPEECH_REASON}; ${SPEECH_UNIT} not touched"
            SPEECH_SUMMARY="${SPEECH_REASON}"
            return 0 ;;
        disable)
            log "speech: ${SPEECH_REASON}; stopping and disabling ${SPEECH_UNIT} so it is not left failing"
            if ! run_write "stop and disable ${SPEECH_UNIT}" ssh_hub "timeout ${SPEECH_RESTART_TIMEOUT} systemctl stop $(shq "${SPEECH_UNIT}"); rc=\$?; systemctl disable $(shq "${SPEECH_UNIT}") && exit \"\${rc}\""; then
                speech_soft "could not stop and disable ${SPEECH_UNIT}"
            elif ! is_rehearse; then
                after="$(speech_state "${SPEECH_BIN}" 2>/dev/null)" || after="unreadable"
                case "$(speech_field active "${after}")/$(speech_field enabled "${after}")" in
                    active/* | activating/* | */enabled) speech_soft "${SPEECH_UNIT} still running or enabled after stop+disable: ${after}" ;;
                    *) log "speech: ${SPEECH_UNIT} stopped and disabled (${after})" ;;
                esac
            fi
            SPEECH_SUMMARY="${SPEECH_REASON}; unit stopped and disabled"
            return 0 ;;
    esac
    log "speech: ${SPEECH_REASON}; restarting ${SPEECH_UNIT} (bounded to ${SPEECH_RESTART_TIMEOUT} s)"
    run_write "restart ${SPEECH_UNIT}" ssh_hub "timeout ${SPEECH_RESTART_TIMEOUT} systemctl restart $(shq "${SPEECH_UNIT}")" || rc=$?
    case "${rc}" in
        0) ;;
        124)
            speech_soft "${SPEECH_UNIT} restart timed out after ${SPEECH_RESTART_TIMEOUT} s"
            SPEECH_SUMMARY="restart timed out after ${SPEECH_RESTART_TIMEOUT} s"
            return 0 ;;
        *)
            speech_soft "${SPEECH_UNIT} restart failed (exit ${rc})"
            SPEECH_SUMMARY="restart failed (exit ${rc})"
            return 0 ;;
    esac
    if speech_wait_health "${SPEECH_BIN}"; then
        log "speech: health ok: ${SPEECH_HEALTH_OUT}"
        SPEECH_SUMMARY="ok"
    else
        speech_soft "${SPEECH_UNIT} health failed after ${SPEECH_HEALTH_WAIT} s: ${SPEECH_HEALTH_OUT##*$'\n'}"
        SPEECH_SUMMARY="health failing"
    fi
}

# Чтение курсора журнала (sj/ack, cj/ack): первые 8 байт LE u64, дальше контрольная сумма.
read_ack() { od -An -tu8 -N8 "$1" 2>/dev/null | tr -d ' \n'; }

# Снимок exec id живых сопровождений из /proc по точному argv: argv[0] с
# basename canterlot-worker, argv[1] ровно supervise, значение --exec-dir.
# $1 — каталог executions прод-воркера (пусто — без ограничения).
# Подстрока по склеенной командной строке ловила бы обёртки (bash -c "… supervise
# --exec-dir …") — тот же класс, что самосовпадение сканера (урок 1.16.0).
# Без fork на процесс: mapfile и встроенные сравнения.
proc_supervisor_ids() {
    local root="${1:-}" d dir prev arg
    local -a argv=()
    for d in /proc/[0-9]*; do
        argv=()
        mapfile -d '' -t argv 2>/dev/null <"${d}/cmdline" || continue
        [ "${#argv[@]}" -ge 3 ] || continue
        [ "${argv[0]##*/}" = "canterlot-worker" ] || continue
        [ "${argv[1]}" = "supervise" ] || continue
        if is_zombie "${d#/proc/}"; then continue; fi
        dir=""
        prev=""
        for arg in "${argv[@]:2}"; do
            case "${arg}" in
                --exec-dir=*) dir="${arg#--exec-dir=}" ;;
                *) if [ "${prev}" = "--exec-dir" ]; then dir="${arg}"; fi ;;
            esac
            prev="${arg}"
        done
        [ -n "${dir}" ] || continue
        if [ -n "${root}" ]; then
            # Только прямой каталог исполнения <root>/<exec-id>: сопровождения
            # тестовых воркеров из worktree под WORKER_DATA (worktrees/<id>/…/
            # executions/<id>) — не прод-сессии.
            case "${dir}" in
                "${root}"/*/*) continue ;;
                "${root}"/*) ;;
                *) continue ;;
            esac
        fi
        printf '%s\n' "${dir##*/}"
    done | sort -u
}

# Состояния исполнения хаба (executions.rs legal_transition), при которых живой
# супервизор законен, хотя строка ещё (или уже) не running:
#   created, starting — воркер поднял супервизор, строка дойдёт до running через
#                       секунды (провал прогона 1.24.0: делегат стартовал за миг до preflight);
#   stopping          — сессия останавливается, супервизор выходит следом;
#   owner_lost        — воркер не увидел процесс в отчёте; при живом супервизоре
#                       владение возвращается (owner_reclaimed) или его решает владелец.
# Терминальные stopped/failed и отсутствие строки при живом супервизоре — утечка.
SUPERVISOR_TRANSIENT_STATES="created starting stopping owner_lost"
SUPERVISOR_SETTLE_WAIT="${SUPERVISOR_SETTLE_WAIT:-60}"
SUPERVISOR_SETTLE_POLL="${SUPERVISOR_SETTLE_POLL:-5}"

# Строки "id state" хаба для перечисленных id (по строке на найденную строку).
# Только UUID попадают в SQL: имя каталога в /proc не цитируется в запрос.
# Использует hub_psql вызывающего скрипта (его провал — провал вызывающего).
hub_exec_states() {
    local id list=""
    for id in "$@"; do
        [[ "${id}" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || continue
        list+="${list:+,}'${id}'"
    done
    [ -n "${list}" ] || return 0
    hub_psql "select id::text || ' ' || state from executions where id in (${list}) order by 1"
}

# Сверка супервизоров /proc со строками хаба с коротким ожиданием переходных
# строк. Ставит:
#   SETTLED_LIVE    — id строк running (последнее чтение);
#   SETTLED_PROC    — exec id живых супервизоров (последнее чтение);
#   SETTLED_EXTRA   — "id state" супервизоров без строки running; state — состояние
#                     хаба или none, если строки нет.
# Пока среди лишних есть переходные (или строка стала running между чтениями),
# ждём до SUPERVISOR_SETTLE_WAIT секунд, опрашивая каждые SUPERVISOR_SETTLE_POLL;
# каждое чтение заново берёт и хаб, и /proc. Решение (hard/soft) — у вызывающего.
# Провал запроса к хабу — код 1 (вызывающий обязан умереть, а не считать пустоту).
# $1 — каталог executions прод-воркера.
settle_supervisor_rows() {
    local root="$1" start="${SECONDS}" id state states pending waited_log=0 row_id row_state
    local -a extra=()
    while :; do
        SETTLED_LIVE="$(hub_psql "select id::text from executions where state='running' order by 1")" || return 1
        SETTLED_PROC="$(proc_supervisor_ids "${root}")" || return 1
        SETTLED_EXTRA=""
        pending=""
        extra=()
        for id in ${SETTLED_PROC}; do
            in_lines "${id}" "${SETTLED_LIVE}" || extra+=("${id}")
        done
        if [ "${#extra[@]}" -gt 0 ]; then
            states="$(hub_exec_states "${extra[@]}")" || return 1
            for id in "${extra[@]}"; do
                state="none"
                while read -r row_id row_state; do
                    if [ "${row_id}" = "${id}" ]; then state="${row_state:-none}"; fi
                done <<<"${states}"
                SETTLED_EXTRA+="${id} ${state}"$'\n'
                case " ${SUPERVISOR_TRANSIENT_STATES} running " in
                    *" ${state} "*) pending+="${pending:+, }${id} ${state}" ;;
                esac
            done
        fi
        [ -n "${pending}" ] || break
        if [ $((SECONDS - start)) -ge "${SUPERVISOR_SETTLE_WAIT}" ]; then
            log "supervisor rows still transitional after ${SUPERVISOR_SETTLE_WAIT} s: ${pending}"
            waited_log=0
            break
        fi
        [ "${waited_log}" = "1" ] || log "waiting up to ${SUPERVISOR_SETTLE_WAIT} s for transitional supervisor rows: ${pending}"
        waited_log=1
        sleep "${SUPERVISOR_SETTLE_POLL}"
    done
    [ "${waited_log}" = "0" ] || log "supervisor rows settled after $((SECONDS - start)) s"
    SETTLED_EXTRA="${SETTLED_EXTRA%$'\n'}"
}

# Состояние хаба — переходное (живой супервизор при нём законен).
is_transient_exec_state() {
    case " ${SUPERVISOR_TRANSIENT_STATES} " in *" $1 "*) return 0 ;; esac
    return 1
}

# Все потомки PID (дети, внуки, ...): ps один раз, без pgrep -f.
# Нужно, чтобы релей находился даже не прямым ребёнком сопровождения.
proc_descendants() {
    local root="$1"
    ps -eo pid=,ppid= 2>/dev/null | awk -v root="${root}" '
        { children[$2] = children[$2] " " $1 }
        END {
            head = 1; tail = 1; queue[1] = root
            while (head <= tail) {
                p = queue[head++]
                n = split(children[p], c, " ")
                for (i = 1; i <= n; i++) {
                    if (c[i] == "") continue
                    print c[i]
                    queue[++tail] = c[i]
                }
            }
        }'
}

# Стартовый тик процесса (поле 22 stat); пусто, если процесса нет.
# После "pid (comm) " поля начинаются с state (поле 3), поэтому starttime — индекс 19.
proc_start_tick() {
    local statline rest
    local -a fields=()
    { read -r statline <"/proc/$1/stat"; } 2>/dev/null || return 0
    rest="${statline##*) }"
    read -r -a fields <<<"${rest}"
    printf '%s' "${fields[19]:-}"
}

# CLI сессии — точная роль, а не любой потомок супервизора: прямой ребёнок
# сопровождения, который не релей (точная подкоманда), не сопровождение и не
# ссылается на каталог исполнения. Из нескольких кандидатов берётся самый
# старший по стартовому тику: CLI запускается первым и живёт всю сессию, а вызовы
# инструментов приходят позже и живут секунды. Печатает "pid tick" либо ничего
# (ничего — это "CLI нет", вызывающий решает, что это значит).
session_cli_pid() {
    local sup_pid="$1" raw cand tick cmd best="" best_tick=""
    raw="$(ps -o pid= --ppid "${sup_pid}" 2>/dev/null || true)"
    for cand in ${raw}; do
        if is_zombie "${cand}"; then continue; fi
        _relay_exec_id "${cand}"
        [ -z "${RELAY_ID}" ] || continue
        cmd="$(tr '\0' ' ' <"/proc/${cand}/cmdline" 2>/dev/null | cut -c1-200)"
        case "${cmd}" in
            *supervise*|*mcp-relay*|*executions/*) continue ;;
        esac
        tick="$(proc_start_tick "${cand}")"
        [ -n "${tick}" ] || continue
        if [ -z "${best}" ] || [ "${tick}" -lt "${best_tick}" ]; then
            best="${cand}"; best_tick="${tick}"
        fi
    done
    [ -z "${best}" ] || printf '%s %s\n' "${best}" "${best_tick}"
}

# Точная идентификация релея: argv[1] ровно "mcp-relay" и есть --socket
# с путём <...>/executions/<exec-id>/mcp.sock. Печатает exec id или ничего.
# Слово mcp-relay в произвольных аргументах (или executions/ в пути CLI) не
# считается релеем, а argv[1]=run — не считается ни при каких аргументах.
proc_relay_exec_id() {
    _relay_exec_id "$1"
    [ -z "${RELAY_ID}" ] || printf '%s\n' "${RELAY_ID}"
}
# То же без подоболочки: результат в RELAY_ID (пусто — не релей). Нужен сканам
# всей таблицы процессов: $(…) на каждый процесс стоил сотни fork'ов.
_relay_exec_id() {
    local pid="$1" prev="" arg
    local -a argv=()
    RELAY_ID=""
    mapfile -d '' -t argv 2>/dev/null <"/proc/${pid}/cmdline" || return 0
    [ "${#argv[@]}" -ge 2 ] || return 0
    [ "${argv[1]}" = "mcp-relay" ] || return 0
    for arg in "${argv[@]:2}"; do
        if [ "${prev}" = "--socket" ]; then
            case "${arg}" in
                */executions/*/mcp.sock)
                    arg="${arg#*/executions/}"
                    RELAY_ID="${arg%%/*}"
                    return 0 ;;
            esac
        fi
        prev="${arg}"
    done
    return 0
}

# Все живые настоящие релеи исполнения (по всей таблице процессов, точной
# подкомандой и exec id), по одному pid в строке. Приманки со словом mcp-relay в
# произвольных аргументах не считаются.
relay_pids_for_exec() {
    local exec_id="$1" d cand
    for d in /proc/[0-9]*; do
        cand="${d#/proc/}"
        _relay_exec_id "${cand}"
        [ "${RELAY_ID}" = "${exec_id}" ] || continue
        if is_zombie "${cand}"; then continue; fi
        printf '%s\n' "${cand}"
    done
}

# Значения роли из строки снимка (sup=..., cli=..., relay=pid:tick,pid:tick),
# по одному "pid:tick" в строке; none/none:? не выводятся.
snapshot_role_values() {
    local line="$1" role="$2" token
    token="$(printf '%s' "${line}" | grep -o " ${role}=[^ ]*" | sed -n 1p | cut -d= -f2 || true)"
    printf '%s' "${token}" | tr ',' '\n' | grep -vE '^(none|none:\?|)$' || true
}

# Ссылка link вида <base>/<rel> ведёт ровно на rel: сравнение нормализованное
# (урок: plain readlink отдавал относительную цель, и каждый flip считался провалом).
link_points_to() {
    local link="$1" base="$2" rel="$3"
    local target
    target="$(readlink "${link}")" || return 1
    [ "${target}" = "${rel}" ] || return 1
    [ "$(readlink -m "${base}/${target}")" = "${base}/${rel}" ] || return 1
    return 0
}

# --- помощники состава релиза: общие у stage.sh и target-check.sh ---
# Ожидаемый состав корня stagedir веба берётся из собранного dist этого релиза, а не из
# зашитого списка (сборка меняется между релизами: service-worker.js убран в 1.0.0).
# Печатает имена корня в C-порядке через пробел (с хвостовым пробелом, как собирает
# удалённый ls). Громкий отказ, если каталог не похож на содержимое build/: нет
# 200.html или _app/version.json либо лишний уровень web/ (урок: tar целого web).
web_top_entries() {
    local dir="$1"
    [ -d "${dir}" ] || { printf 'no dist web dir: %s\n' "${dir}" >&2; return 1; }
    if [ -e "${dir}/web" ]; then
        printf 'dist web has a nested web/ level: %s/web\n' "${dir}" >&2
        return 1
    fi
    if [ ! -f "${dir}/200.html" ] || [ ! -f "${dir}/_app/version.json" ]; then
        printf 'dist web lacks 200.html or _app/version.json: %s\n' "${dir}" >&2
        return 1
    fi
    find "${dir}" -mindepth 1 -maxdepth 1 -printf '%f\n' | LC_ALL=C sort | tr '\n' ' '
}

# Для следующих функций вызывающий задаёт DIST (каталог dist/<sha>).
# Полные пофайловые хэши дерева. Сравнение идёт по именам (sort -k2), а не по
# хэшам: иначе другой набор файлов мог случайно дать ту же строку.
sums_sorted() { LC_ALL=C sort -k2; }
tree_sums_local() {
    ( cd "$1" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum ) | sums_sorted
}
tree_sums_remote() {
    ssh -n -o BatchMode=yes -o ConnectTimeout=10 "$1" \
        "cd $(shq "$2") && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum" \
        </dev/null | sums_sorted
}
# Ожидаемый состав релиза: dist-файлы + RELEASE (и SHA256SUMS у хаба).
# Так проверка идёт и против dist/SHA256SUMS, и против RELEASE, и по каждому файлу.
hub_expected_sums() {
    {
        ( cd "${DIST}" && find ./hub -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum ) \
            | sed 's|  \./hub/|  ./|'
        ( cd "${DIST}" && sha256sum ./RELEASE ./SHA256SUMS )
    } | sums_sorted
}
web_expected_sums() {
    ( cd "${DIST}/web" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum ) | sums_sorted
}
worker_expected_sums() {
    {
        ( cd "${DIST}/worker" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum )
        ( cd "${DIST}" && sha256sum ./RELEASE )
    } | sums_sorted
}

# CSP из csp.txt обязан быть выведен из ФИНАЛЬНОГО PWA-билда в том же dist (урок 1.5.0:
# env со старым хэшем скрипта, залитый раньше финальной сборки, ломал страницу). Хэши
# встроенных скриптов 200.html считаются так же, как в web/scripts/print-csp.mjs, и
# множество должно совпасть с хэшами script-src в csp.txt. Печатаются только числа.
# Вызывающий задаёт DIST. Код 0 — совпало, 1 — нет (причина в stdout).
check_csp_matches_build() {
    python3 -I - "${DIST}/web/200.html" "${DIST}/csp.txt" <<'PY'
import base64, hashlib, re, sys
try:
    shell = open(sys.argv[1], encoding="utf-8").read()
    csp = open(sys.argv[2], encoding="utf-8").read()
except OSError as exc:
    print(f"cannot read {exc.filename}"); sys.exit(1)
built = {"sha256-" + base64.b64encode(hashlib.sha256(m.encode()).digest()).decode()
         for m in re.findall(r"<script\b(?![^>]*\bsrc=)[^>]*>([\s\S]*?)</script>", shell)}
declared = set(re.findall(r"'(sha256-[^']*)'", csp))
if built != declared:
    print(f"csp.txt script hashes ({len(declared)}) differ from the dist web/200.html inline scripts ({len(built)}): "
          f"{len(declared - built)} declared-only, {len(built - declared)} build-only")
    sys.exit(1)
print(f"csp.txt script hashes equal the dist web/200.html inline scripts ({len(built)})")
PY
}

# --- обслуживание: сокеты, каталоги исполнений, остатки, запись env ---
# Раньше проверялось руками в журнале выката. Каждая живая сессия обязана иметь
# свой каталог исполнения и сокет; на хостах не должно остаться current.new,
# build.new и каталогов .stage-*; запись canterlot.env.<web> обязана быть 0600
# и совпадать с живым env. Нарушения — мягкие ошибки (soft): записать и идти
# дальше; dry-run считает их провалом. Лишние каталоги/сокеты без живой строки
# только печатаются. $1 — живые id (строки), $2 — релиз PWA.
# shellcheck disable=SC2016 # удалённые скрипты раскрывают свои позиционные аргументы
LEFTOVER_SCRIPT='cd "$1" || exit 2; shift; for n in "$@" releases/.stage-*; do if [ -e "$n" ] || [ -L "$n" ]; then echo "$n"; fi; done'
# shellcheck disable=SC2016
ENV_RECORD_SCRIPT='rec="$1"; live="$2"; if [ ! -f "$rec" ]; then echo missing; exit 0; fi; mode="$(stat -c %a "$rec")"; if cmp -s "$rec" "$live"; then echo "mode=$mode equal"; else echo "mode=$mode differs"; fi'
maintenance_check() {
    local live_ids="$1" web_sha="$2" id n_live=0 out leftovers name
    for id in ${live_ids}; do
        n_live=$((n_live + 1))
        if [ -d "${WORKER_DATA}/executions/${id}" ]; then :; else soft "maintenance: live execution ${id} has no exec dir in ${WORKER_DATA}/executions"; fi
        if [ -S "${WORKER_SOCKET_DIR}/${id}.sock" ]; then :; else soft "maintenance: live execution ${id} has no socket ${WORKER_SOCKET_DIR}/${id}.sock"; fi
    done
    for name in "${WORKER_DATA}"/executions/*/; do
        [ -d "${name}" ] || continue
        name="${name%/}"; name="${name##*/}"
        case " ${live_ids//$'\n'/ } " in
            *" ${name} "*) ;;
            *) log "maintenance note: exec dir ${name} has no live execution row (left as is)" ;;
        esac
    done
    log "maintenance: ${n_live} live executions, each checked for its exec dir and socket"
    leftovers=""
    if out="$(ssh_hub "$(locked_free_cmd "${LEFTOVER_SCRIPT}" "${HUB_BASE}" current.new)")"; then
        leftovers+="${out:+hub: ${out//$'\n'/ } }"
    else
        soft "maintenance: cannot list leftovers on the hub"
    fi
    if out="$(ssh_web "$(locked_free_cmd "${LEFTOVER_SCRIPT}" "${WEB_SITES_BASE}" build.new)")"; then
        leftovers+="${out:+web: ${out//$'\n'/ } }"
    else
        soft "maintenance: cannot list leftovers on web"
    fi
    if out="$(bash -c "${LEFTOVER_SCRIPT}" -- "${WORKER_BASE}" current.new)"; then
        leftovers+="${out:+worker: ${out//$'\n'/ } }"
    else
        soft "maintenance: cannot list leftovers on the worker"
    fi
    if [ -n "${leftovers}" ]; then
        soft "maintenance: leftovers on hosts: ${leftovers}"
    else
        log "maintenance: no current.new/build.new/.stage-* on any host"
    fi
    if out="$(ssh_web "$(locked_free_cmd "${ENV_RECORD_SCRIPT}" "${WEB_ENV}.${web_sha}" "${WEB_ENV}")")"; then
        case "${out}" in
            "mode=600 equal") log "maintenance: ${WEB_ENV##*/}.${web_sha} is 0600 and equal to the live env" ;;
            *) soft "maintenance: env record ${WEB_ENV##*/}.${web_sha}: ${out}" ;;
        esac
    else
        soft "maintenance: cannot read the env record on web"
    fi
}
# Текст «bash -c script -- args» без lock: та же цитата, что у locked_cmd_text.
locked_free_cmd() {
    local script="$1"
    shift
    printf 'bash -c %s --%s' "$(shq "${script}")" "$(_quote_args "$@")"
}

# --- слияние старых _app/immutable чанков (открытые вкладки PWA) ---
# Вкладка, открытая на прошлом релизе, догружает свои чанки по старым именам.
# После старта воркера и ДО любой чистки чанки предыдущих релизов веба
# копируются в новый релиз без перезаписи (cp -rn). Проверка — отдельным
# чтением: ни одного пропущенного относительного пути из каждого источника, и
# каждый файл dist нового релиза байт в байт на месте (sha256sum -c).
# shellcheck disable=SC2016,SC2034 # используется в cutover
WEB_MERGE_SCRIPT='rel="$1"; new="$2"; shift 2; test -d "$rel/$new/_app/immutable" || exit 3; for old in "$@"; do src="$rel/$old/_app/immutable"; if [ -d "$src" ]; then cp -rn "$src/." "$rel/$new/_app/immutable/" || true; fi; done'
# shellcheck disable=SC2016
WEB_MERGE_COUNT_SCRIPT='rel="$1"; new="$2"; shift 2; for old in "$@"; do src="$rel/$old/_app/immutable"; t=0; m=0; if [ -d "$src" ]; then for f in $(cd "$src" && find . -type f); do t=$((t + 1)); if [ ! -e "$rel/$new/_app/immutable/$f" ]; then m=$((m + 1)); fi; done; fi; echo "$old $t $m"; done'
# Источники слияния: до RETAIN_COUNT самых свежих релизов веба, кроме цели.
web_merge_sources() {
    local target="$1" raw dir name n=0 out=""
    if ! raw="$(ssh_web "ls -1dt $(shq "${WEB_RELEASES}")/*/")"; then
        return 1
    fi
    for dir in ${raw}; do
        dir="${dir%/}"
        name="${dir##*/}"
        is_release_id "${name}" || continue
        [ "${name}" != "${target}" ] || continue
        [ "${n}" -lt "${RETAIN_COUNT}" ] || break
        out+="${name} "
        n=$((n + 1))
    done
    printf '%s' "${out% }"
}
# Пропуски по источникам: строки "<релиз> <файлов> <пропущено>".
web_merge_counts() {
    local target="$1"
    shift
    ssh_web "$(locked_free_cmd "${WEB_MERGE_COUNT_SCRIPT}" "${WEB_RELEASES}" "${target}" "$@")"
}
# Каждый файл dist/<sha>/web на месте в релизе веба. Вызывающий задаёт DIST.
web_dist_files_intact() {
    local target="$1"
    web_expected_sums | ssh -o BatchMode=yes -o ConnectTimeout=10 "${WEB_SSH}" \
        "cd $(shq "${WEB_RELEASES}/${target}") && sha256sum --quiet --strict -c -"
}
