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
# shellcheck disable=SC2016 # удалённый скрипт раскрывает свои позиционные аргументы
RESERVE_SCRIPT='b="$1"; shift; for n in "$@"; do : >"${b}/.reserved.${n}"; done'
# shellcheck disable=SC2016,SC2034 # используется в cutover (печатаемая команда)
UNRESERVE_SCRIPT='b="$1"; shift; for n in "$@"; do rm -f "${b}/.reserved.${n}"; done'
reserve_releases() {
    local kind="$1"; shift
    case "${kind}" in
        hub) hub_locked "${RESERVE_SCRIPT}" "${HUB_BASE}" "$@" ;;
        web) web_locked "${RESERVE_SCRIPT}" "${WEB_SITES_BASE}" "$@" ;;
        worker) worker_locked "${RESERVE_SCRIPT}" "${WORKER_BASE}" "$@" ;;
        *) die "unknown reservation kind: ${kind}" ;;
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

# Короткий sha (12 знаков) из полного или короткого.
short_sha() { printf '%s' "$1" | cut -c1-12; }

# Токен подтверждения боевого прогона: dry-run печатает его, боевой прогон
# принимает только точное значение за текущие UTC-сутки. Это не секрет, а
# осознанный барьер между «посмотрел план» и «остановил прод»: случайный запуск
# без свежего dry-run отказывает. Токен детерминирован, поэтому его можно
# пересчитать в тестах.
CONFIRM_LEN="${CONFIRM_LEN:-16}"
confirm_token() {
    local id="$1" day="${2:-$(date -u +%Y-%m-%d)}"
    printf '%s:%s' "${id}" "${day}" | sha256sum | cut -c1-"${CONFIRM_LEN}"
}

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
        sleep 1
        waited=$((waited + 1))
    done
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
        new_cli="$(session_cli_pid "$(printf '%s' "${line}" | grep -o ' sup=[0-9]*' | cut -d= -f2 | head -1)" 2>/dev/null || true)"
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

# Ни один управляющий процесс воркера не должен жить. Признак — basename exe ИЛИ
# argv[0] (бинарь мог быть удалён из релиза после запуска: тогда /proc/pid/exe
# кончается на " (deleted)", а argv[0] сохраняет имя). Тот же бинарь запускает
# супервизор и mcp-relay сессий: их argv[1] — точная подкоманда, и только эти две
# подкоманды можно пропустить. Любая другая (включая run) — живой воркер, даже
# если в её аргументах встречается слово supervise.
# Возврат 0 и описание на stdout — найден; 1 — нет. WORKER_PROC_GUARD_ROOT —
# опциональный скоуп только для hermetic-тестов (на машине разработчика рядом
# работает настоящий воркер); в проде не задан, скан идёт по всему /proc.
find_control_worker() {
    local proc_dir proc_pid exe_target exe_base argv0_base proc_argv
    for proc_dir in /proc/[0-9]*; do
        proc_pid="${proc_dir#/proc/}"
        exe_target="$(readlink "${proc_dir}/exe" 2>/dev/null || true)"
        case "${exe_target}" in
            *" (deleted)") exe_target="${exe_target% (deleted)}" ;;
        esac
        proc_argv=()
        [ -r "${proc_dir}/cmdline" ] || continue
        mapfile -d '' -t proc_argv <"${proc_dir}/cmdline" || true
        [ "${#proc_argv[@]}" -gt 0 ] || continue
        exe_base="${exe_target##*/}"
        argv0_base="${proc_argv[0]##*/}"
        if [ "${exe_base}" != "canterlot-worker" ] && [ "${argv0_base}" != "canterlot-worker" ]; then
            continue
        fi
        if [ -n "${WORKER_PROC_GUARD_ROOT:-}" ]; then
            case "${exe_target}" in
                "${WORKER_PROC_GUARD_ROOT}"/*) ;;
                *)
                    case "${proc_argv[0]}" in
                        "${WORKER_PROC_GUARD_ROOT}"/*) ;;
                        *) continue ;;
                    esac ;;
            esac
        fi
        case "${proc_argv[1]:-}" in
            supervise|mcp-relay) continue ;;
        esac
        printf 'worker process %s still runs %s (argv[1]=%s)' "${proc_pid}" "${exe_target:-${proc_argv[0]}}" "${proc_argv[1]:-none}"
        return 0
    done
    return 1
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
        mapfile -d '' -t argv <"${d}/cmdline" 2>/dev/null || true
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
soft() { printf '[%s] SOFT-FAIL: %s (recorded, continuing)\n' "$(date -u +%H:%M:%S)" "$*" >&2; SOFT_FAILURES=$((SOFT_FAILURES + 1)); }
check_summary() {
    log "hard failures: ${HARD_FAILURES}, soft failures: ${SOFT_FAILURES}"
    [ "${HARD_FAILURES}" -eq 0 ]
}

# Чтение курсора журнала (sj/ack, cj/ack): первые 8 байт LE u64, дальше контрольная сумма.
read_ack() { od -An -tu8 -N8 "$1" 2>/dev/null | tr -d ' \n'; }

# Снимок exec id живых сопровождений из /proc по argv (set -e безопасно: пусто = пусто).
proc_supervisor_ids() {
    local root="${1:-}" d pid cmdline rest dir
    for d in /proc/[0-9]*; do
        pid="${d#/proc/}"
        is_zombie "${pid}" && continue
        # shellcheck disable=SC2002
        cmdline="$(cat "${d}/cmdline" 2>/dev/null | tr '\0' ' ')"
        [ -n "${cmdline}" ] || continue
        case "${cmdline}" in
            *canterlot-worker*supervise*--exec-dir*) ;;
            *) continue ;;
        esac
        rest="${cmdline##*--exec-dir }"
        dir="${rest%% *}"
        if [ -n "${root}" ]; then
            case "${dir}" in
                "${root}"/*) ;;
                *) continue ;;
            esac
        fi
        printf '%s\n' "${dir##*/}"
    done | sort -u
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
        is_zombie "${cand}" && continue
        [ -z "$(proc_relay_exec_id "${cand}")" ] || continue
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
    local pid="$1" argv=() prev="" arg
    [ -r "/proc/${pid}/cmdline" ] || return 0
    mapfile -d '' -t argv <"/proc/${pid}/cmdline" || return 0
    [ "${#argv[@]}" -ge 2 ] || return 0
    [ "${argv[1]}" = "mcp-relay" ] || return 0
    for arg in "${argv[@]:2}"; do
        if [ "${prev}" = "--socket" ]; then
            case "${arg}" in
                */executions/*/mcp.sock)
                    arg="${arg#*/executions/}"
                    printf '%s\n' "${arg%%/*}"
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
        [ "$(proc_relay_exec_id "${cand}")" = "${exec_id}" ] || continue
        is_zombie "${cand}" && continue
        printf '%s\n' "${cand}"
    done
}

# Значения роли из строки снимка (sup=..., cli=..., relay=pid:tick,pid:tick),
# по одному "pid:tick" в строке; none/none:? не выводятся.
snapshot_role_values() {
    local line="$1" role="$2" token
    token="$(printf '%s' "${line}" | grep -o " ${role}=[^ ]*" | head -1 | cut -d= -f2 || true)"
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
