#!/usr/bin/env bash
# Выпуск релиза одной командой после гейта (docs/deploy/pilot.md §11.7).
#
#   scripts/release.sh prepare X.Y.Z
#       бамп версии во всех местах (Cargo.toml, web/package.json, docs/api/openapi.json,
#       бейдж и «Текущая версия» в README.md, cargo update -w --offline), проверка
#       scripts/check-version.sh, коммит «Выпустить версию X.Y.Z». Запись X.Y.Z в
#       CHANGES.rst пишется заранее руками; из грязного дерева допускается только
#       изменённый CHANGES.rst, он входит в коммит. Ничего не пушит.
#   scripts/release.sh ship X.Y.Z [--continue]
#       тег, push main + тег одной атомарной операцией, сборка, проверка dist, stage,
#       репетиция, боевой прогон по её токену, check-proxy без входа, публикация,
#       раздел журнала выката. Можно запускать отсоединённо: ход — в
#       .superpowers/deploy/release-X.Y.Z.log, фазы — в release-X.Y.Z.state, итог
#       каждой попытки — в release-X.Y.Z.attempt-N.result, release-X.Y.Z.result —
#       ссылка на последнюю (формат — pilot.md §11.7). Повторный запуск без
#       --continue только печатает сохранённое состояние; --continue продолжает с
#       первой незавершённой фазы и никогда не повторяет боевой прогон.
#
# Коды выхода: 0 — выпуск опубликован; 1 — остановка (причина в файле итога);
# 2 — отказ предусловий или аргументов; 3 — уже идёт другой выпуск или есть
# сохранённое состояние (нужен --continue); 4 — не записался итог, журнал или
# состояние (исход выпуска — в том, что записалось, и в логе; --continue допишет).
#
# Окружение: CANTERLOT_RELEASE_DIR (каталог логов, состояния и журнала, по умолчанию
# .superpowers/deploy), CANTERLOT_GATE_DIR (аттестации гейта, .superpowers/gate),
# CANTERLOT_DIST_ROOT (dist, .superpowers/deploy/dist), RELEASE_RETRY_DELAY (пауза
# перед повтором репетиции, 120 с), RELEASE_MIN_FREE_MB (свободное место, 10240 МБ).
# Секреты не читаются и не печатаются; конфиги и site.env не меняются.
# shellcheck disable=SC2016 # обратные кавычки в printf — разметка Markdown журнала
set -euo pipefail
export LC_ALL=C

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"

usage() {
    cat >&2 <<'EOF'
Использование:
  scripts/release.sh prepare X.Y.Z
  scripts/release.sh ship X.Y.Z [--continue]
EOF
    exit 2
}

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }
log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
one_line() { printf '%s' "$1" | tr '\n\r' '  '; }
# Значение ключа из файла ключ=значение (файл результата cutover, состояние).
kv() { [[ -f $1 ]] || return 0; sed -n "s/^$2=//p" "$1" | sed -n 1p; }

# Значения примера из deploy.conf, site.env.example и build-release.sh.
EXAMPLE_DENYLIST=(example.com 192.0.2. 198.51.100. 203.0.113.)
TRANSIENT_CODES=" session_starting vzdump_busy "
TOKEN_CODES=" token_expired token_plan_changed "

head_version() {
    python3 -I -c 'import sys, tomllib
print(tomllib.load(open(sys.argv[1], "rb"))["workspace"]["package"]["version"])' "$repo/Cargo.toml"
}

# --- prepare -------------------------------------------------------------------

prepare() {
    local ver=$1 line path bad=() files
    [[ $ver =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
    # Грязное дерево: допустим только изменённый (не новый, не удалённый) CHANGES.rst.
    while IFS= read -r line; do
        [[ -n $line ]] || continue
        path=${line:3}
        case "${line:0:2}" in
            ' M' | 'M ' | 'MM') [[ $path == CHANGES.rst ]] && continue ;;
        esac
        bad+=("$line")
    done < <(git status --porcelain --untracked-files=all)
    if ((${#bad[@]})); then
        printf 'prepare: the tree has changes besides CHANGES.rst:\n' >&2
        printf '  %s\n' "${bad[@]}" >&2
        exit 2
    fi
    if ! grep -qE "^${ver//./\\.} — " CHANGES.rst || ! grep -qx ".. _v${ver}:" CHANGES.rst; then
        printf 'prepare: CHANGES.rst has no entry for %s (".. _v%s:" and "%s — DD.MM.YYYY"): write it first\n' \
            "$ver" "$ver" "$ver" >&2
        exit 2
    fi
    local cur
    cur=$(head_version)
    [[ $cur != "$ver" ]] || { printf 'prepare: Cargo.toml is already at %s\n' "$ver" >&2; exit 2; }
    log "prepare $ver (was $cur)"
    python3 -I - "$repo" "$ver" <<'PY'
import json, re, sys
from pathlib import Path

root, ver = Path(sys.argv[1]), sys.argv[2]

def sub_once(path, pattern, repl, flags=0):
    text = path.read_text(encoding="utf-8")
    new, n = re.subn(pattern, repl, text, count=1, flags=flags)
    if n != 1:
        sys.exit(f"prepare: version spot not found in {path.relative_to(root)}: {pattern}")
    path.write_text(new, encoding="utf-8")

sub_once(root / "Cargo.toml",
         r'(^\[workspace\.package\][^\[]*?^version = ")[^"]*(")', rf"\g<1>{ver}\g<2>", re.M | re.S)
sub_once(root / "web/package.json", r'^(  "version": ")[^"]*(")', rf"\g<1>{ver}\g<2>", re.M)
sub_once(root / "docs/api/openapi.json", r'("info": \{.*?"version": ")[^"]*(")', rf"\g<1>{ver}\g<2>", re.S)
sub_once(root / "README.md",
         r"(img\.shields\.io/badge/(?:%D0%B2%D0%B5%D1%80%D1%81%D0%B8%D1%8F|версия)-)\d+\.\d+\.\d+(-)",
         rf"\g<1>{ver}\g<2>")
sub_once(root / "README.md", r"(Текущая версия — )\d+\.\d+\.\d+(\.)", rf"\g<1>{ver}\g<2>")
if json.loads((root / "docs/api/openapi.json").read_text(encoding="utf-8"))["info"]["version"] != ver:
    sys.exit("prepare: docs/api/openapi.json info.version did not change")
if json.loads((root / "web/package.json").read_text(encoding="utf-8"))["version"] != ver:
    sys.exit("prepare: web/package.json version did not change")
PY
    files=(Cargo.toml web/package.json docs/api/openapi.json README.md)
    [[ -f Cargo.lock ]] && files+=(Cargo.lock)
    if ! cargo update -w --offline || ! scripts/check-version.sh; then
        # Файлы были чистыми до бампа: возврат к HEAD теряет только сам бамп.
        git checkout -- "${files[@]}"
        printf 'prepare: cargo update or check-version failed; the bump was reverted, CHANGES.rst kept\n' >&2
        exit 1
    fi
    git add -- "${files[@]}" CHANGES.rst
    git commit -q -m "Выпустить версию $ver"
    log "committed $(git rev-parse --short=12 HEAD) «Выпустить версию $ver»; nothing pushed"
    log "next: scripts/gate.sh, then scripts/release.sh ship $ver"
}

# --- ship: файлы и состояние -----------------------------------------------------

PHASES=(tag push build dist stage rehearsal execute proxy publish journal)
VER=""
TAG=""
SHA=""
SHA12=""
CONTINUE=0
STATE=""
RESULT=""
ATTEMPT=""
ATTEMPT_RESULT=""
LOG=""
JOURNAL=""
STATE_DIR=""
DIST_ROOT=""
SITE_ORIGIN_EFF=""
CURRENT_PHASE="preconditions"
FINISHED=0
RUN_STARTED=0
TEE_PID=""
SIGNALED=""
STARTED_AT=""
# Итоги фаз этого запуска (и сохранённые в состоянии).
REHEARSAL_RESULT=""
REHEARSAL_LOG=""
REHEARSAL_RETRIED=no
REHEARSAL_CODES=""
TOKEN=""
CUTOVER_RESULT=""
CUTOVER_LOG=""
CUTOVER_RC=""
PROXY_CHECK=not_run
PUBLISHED=no

state_get() { kv "$STATE" "$1"; }
# Запись ключа на месте (или в конец), атомарно через временный файл рядом.
state_set() {
    local key=$1 value tmp
    value=$(one_line "$2")
    tmp="$STATE.tmp.$$"
    if [[ -f $STATE ]] && grep -q "^$key=" "$STATE"; then
        awk -v k="$key" -v v="$value" 'index($0, k "=") == 1 { print k "=" v; next } { print }' "$STATE" >"$tmp" || return 1
    else
        { [[ -f $STATE ]] && cat "$STATE"; printf '%s=%s\n' "$key" "$value"; } >"$tmp" || return 1
    fi
    mv -f "$tmp" "$STATE" || return 1
}
phase_status() { state_get "phase_$1" | awk '{print $1}'; }
phase_set() {
    state_set "phase_$1" "$2 $(ts)" || return 1
    log "phase $1: $2"
}

# Свободный счётчик для имён логов повторяемых фаз (rehearsal-X.Y.Z-N.log).
next_seq() {
    local n
    n=$(state_get "$1")
    n=$((${n:-0} + 1))
    state_set "$1" "$n"
    printf '%s' "$n"
}

# Файл результата cutover полный: format=1 в начале, известный статус и
# последний ключ log= на месте (обрезанный файл — как отсутствующий).
cutover_result_complete() {
    [[ -f $1 ]] || return 1
    [[ $(head -1 "$1") == format=1 ]] || return 1
    grep -q '^log=' "$1" || return 1
    case "$(kv "$1" status)" in
        pass | fail | done | stop_and_report | failed_before_flip | refused) return 0 ;;
    esac
    return 1
}

# Итог пишется в файл своей попытки (прошлые не перезаписываются), затем
# release-X.Y.Z.result атомарно переводится на него.
write_result() {
    local status=$1 code=$2 reason=$3 cr=${CUTOVER_RESULT} pending tmp link
    pending="check-proxy --positive (sign-in, orchestrator); owner checklist (authenticated checks)"
    if [[ -n $cr && -f $cr && $(kv "$cr" speech_soft_failures) =~ ^[1-9] ]]; then
        pending+="; speech soft failures $(kv "$cr" speech_soft_failures) (dictation, see SPEECH-SOFT in the cutover log)"
    fi
    if [[ -n $cr && -f $cr && $(kv "$cr" status) == stop_and_report ]]; then
        pending="STOP-AND-REPORT: paste the STOP block into the deploy log and call the owner; $pending"
    fi
    [[ -n $ATTEMPT_RESULT ]] || return 1
    tmp="$ATTEMPT_RESULT.tmp.$$"
    {
        printf '%s=%s\n' \
            format 1 \
            attempt "$ATTEMPT" \
            version "$VER" \
            tag "$TAG" \
            sha "$SHA" \
            status "$status" \
            exit_code "$code" \
            reason "$(one_line "$reason")" \
            phase "$CURRENT_PHASE" \
            continued "$([[ $CONTINUE == 1 ]] && echo yes || echo no)" \
            started_at "$STARTED_AT" \
            finished_at "$(ts)" \
            rehearsal_retried "$REHEARSAL_RETRIED" \
            rehearsal_fail_codes "$REHEARSAL_CODES" \
            cutover_status "$([[ -n $cr ]] && kv "$cr" status)" \
            soft_failures "$([[ -n $cr ]] && kv "$cr" soft_failures)" \
            soft_by_kind "$([[ -n $cr ]] && kv "$cr" soft_by_kind)" \
            speech_soft_failures "$([[ -n $cr ]] && kv "$cr" speech_soft_failures)" \
            worker_downtime_s "$([[ -n $cr ]] && kv "$cr" worker_downtime_s)" \
            hub_interruption_s "$([[ -n $cr ]] && kv "$cr" hub_interruption_s)" \
            dump "$([[ -n $cr ]] && kv "$cr" dump)" \
            vzdump_archive "$([[ -n $cr ]] && kv "$cr" vzdump_archive)" \
            stop_block "$([[ -n $cr ]] && kv "$cr" stop_block)" \
            proxy_check "$PROXY_CHECK" \
            published "$PUBLISHED" \
            pending "$pending" \
            log "$LOG" \
            state "$([[ -f $STATE ]] && echo "$STATE")" \
            journal "$([[ -f $STATE ]] && echo "$JOURNAL")" \
            rehearsal_result "$REHEARSAL_RESULT" \
            rehearsal_log "$REHEARSAL_LOG" \
            cutover_result "$cr" \
            cutover_log "$CUTOVER_LOG"
    } >"$tmp" || return 1
    mv -f "$tmp" "$ATTEMPT_RESULT" || return 1
    link="$RESULT.tmp.$$"
    ln -sfn "${ATTEMPT_RESULT##*/}" "$link" || return 1
    mv -fT "$link" "$RESULT" || return 1
}

# Номер попытки: первый свободный release-X.Y.Z.attempt-N.result, созданный
# эксклюзивно (две одновременные попытки не получат один номер).
new_attempt() {
    local n=1
    while ! (set -C; : >"$STATE_DIR/release-$VER.attempt-$n.result") 2>/dev/null; do
        n=$((n + 1))
        ((n < 10000)) || return 1
    done
    ATTEMPT=$n
    ATTEMPT_RESULT="$STATE_DIR/release-$VER.attempt-$n.result"
}

# Короткая запись попытки, не дошедшей до старта (отказ, блокировка, сигнал).
journal_brief() {
    printf '\n### release.sh ship %s%s attempt %s: %s (exit %s), %s UTC\n\n%s. Result `%s`, log `%s`.\n' \
        "$VER" "$([[ $CONTINUE == 1 ]] && echo ' --continue')" "$ATTEMPT" "$1" "$2" "$(date -u '+%d.%m, %H:%M')" \
        "$(one_line "$3")" "$ATTEMPT_RESULT" "$LOG" >>"$JOURNAL"
}
# Итог такой попытки: свой файл итога и запись в журнале; сбой записи — код 4.
record_attempt() {
    local status=$1 code=$2 reason=$3 failed=""
    FINISHED=1
    set +e
    journal_brief "$status" "$code" "$reason" || failed+=" journal ($JOURNAL)"
    write_result "$status" "$code" "$reason${failed:+; REPORTING FAILED:$failed}" || failed+=" result (${ATTEMPT_RESULT:-$RESULT})"
    FINISH_CODE=$code
    if [[ -n $failed ]]; then
        log "REPORTING FAILED:$failed for attempt ${ATTEMPT:-?} ($status)"
        FINISH_CODE=4
    fi
}

# Раздел журнала выката: на каждом пути после старта (итог, фазы, логи, хвосты).
append_journal() {
    local status=$1 reason=$2 cr=${CUTOVER_RESULT} p st n
    {
        printf '\n## Update to %s (`%s`) by scripts/release.sh: %s, %s UTC\n\n' \
            "$VER" "$SHA12" "$status" "$(date -u '+%d.%m, %H:%M')"
        printf 'Release: `%s` (`%s`, tag `%s`); dist `%s`. Driver: `scripts/release.sh ship %s%s`, started %s.\n\n' \
            "$SHA12" "$SHA" "$TAG" "$DIST_ROOT/$SHA12" "$VER" "$([[ $CONTINUE == 1 ]] && echo ' --continue')" "$STARTED_AT"
        printf '**Result: %s: %s.**\n\n### Phases\n' "$status" "$reason"
        for p in "${PHASES[@]}"; do
            st=$(state_get "phase_$p")
            printf -- '- %s: %s\n' "$p" "${st:-not started}"
        done
        if [[ -n $cr && -f $cr ]]; then
            printf '\n### Cutover\n'
            printf -- '- result `%s`: status %s, soft failures %s (%s), speech soft failures %s, worker down %s s, hub interruption %s s\n' \
                "$cr" "$(kv "$cr" status)" "$(kv "$cr" soft_failures)" "$(kv "$cr" soft_by_kind)" \
                "$(kv "$cr" speech_soft_failures)" "$(kv "$cr" worker_downtime_s)" "$(kv "$cr" hub_interruption_s)"
            [[ -z $(kv "$cr" dump) ]] || printf -- '- restore points: dump `%s`, vzdump `%s` notes `%s`\n' \
                "$(kv "$cr" dump)" "$(kv "$cr" vzdump_archive)" "$(kv "$cr" vzdump_notes)"
            if [[ -n $(kv "$cr" stop_block) && $(kv "$cr" status) == stop_and_report ]]; then
                printf -- '- STOP block: `%s` (paste it here, call the owner)\n' "$(kv "$cr" stop_block)"
            elif [[ -n $(kv "$cr" stop_block) ]]; then
                printf -- '- recovery report before the hub flip: `%s` (paste it here)\n' "$(kv "$cr" stop_block)"
            fi
            [[ -z $(kv "$cr" recovery) ]] || printf -- '- recovery before the hub flip: %s\n' "$(kv "$cr" recovery)"
        fi
        printf '\n### Logs\n'
        printf -- '- release: `%s`; state `%s`; result `%s`\n' "$LOG" "$STATE" "$RESULT"
        for p in build stage proxy publish; do
            st=$(state_get "log_$p")
            [[ -z $st ]] || printf -- '- %s: `%s`\n' "$p" "$st"
        done
        # Каждая репетиция и каждый боевой прогон этого выпуска: лог и файл результата.
        for ((n = 1; n <= $(state_get rehearsals || true) + 0; n++)); do
            printf -- '- rehearsal #%s: `%s`, result `%s`\n' "$n" "$STATE_DIR/rehearsal-$VER-$n.log" "$STATE_DIR/rehearsal-$VER-$n.result"
        done
        for ((n = 1; n <= $(state_get executes || true) + 0; n++)); do
            printf -- '- cutover #%s: `%s`, result `%s`\n' "$n" "$STATE_DIR/cutover-$VER-$n.log" "$STATE_DIR/cutover-$VER-$n.result"
        done
        printf '\n### Deviations\n'
        if [[ $REHEARSAL_RETRIED == yes ]]; then
            printf -- '- the rehearsal was repeated once after transient failure codes `%s`\n' "$(state_get retried_codes)"
        fi
        if [[ -n $cr && -f $cr && $(kv "$cr" soft_failures) =~ ^[1-9] ]]; then
            printf -- '- soft failures %s (%s): publication withheld, see SOFT-FAIL lines in the cutover log\n' \
                "$(kv "$cr" soft_failures)" "$(kv "$cr" soft_by_kind)"
        fi
        [[ $status == "done" || $status == stopped_before_publish ]] || printf -- '- stopped: %s\n' "$reason"
        printf '\n### Left for the owner\n'
        if [[ -n $cr && -f $cr && $(kv "$cr" status) == stop_and_report ]]; then
            printf -- '- STOP-AND-REPORT: do nothing beyond the printed block; call the owner (block `%s`)\n' "$(kv "$cr" stop_block)"
        fi
        printf -- '- pending: `EXPECTED_LOGIN=<owner login> scripts/check-proxy.sh %s --positive` (orchestrator; sends one sign-in push)\n' \
            "${SITE_ORIGIN_EFF:-<origin>}"
        if [[ -n $cr && -f $cr && $(kv "$cr" status) == "done" ]]; then
            printf -- '- authenticated checks: the owner checklist at the end of `%s`\n' "$CUTOVER_LOG"
        fi
        if [[ -n $cr && -f $cr && $(kv "$cr" speech_soft_failures) =~ ^[1-9] ]]; then
            printf -- '- pending: speech soft failures %s (dictation never blocks the release; see SPEECH-SOFT in the cutover log)\n' \
                "$(kv "$cr" speech_soft_failures)"
        fi
    } >>"$JOURNAL"
}

print_owner_checklist() {
    if [[ -n $CUTOVER_LOG && -f $CUTOVER_LOG ]] && grep -q '=== owner checklist' "$CUTOVER_LOG"; then
        sed -n '/=== owner checklist/,/paste the result of each line/p' "$CUTOVER_LOG"
    else
        printf '=== owner checklist ===\n  [ ] EXPECTED_LOGIN=<owner login> scripts/check-proxy.sh %s --positive (orchestrator)\n' \
            "${SITE_ORIGIN_EFF:-<origin>}"
    fi
}

# Итог любого пути после старта: раздел журнала, состояние, файл итога, сводка.
# Каждая запись проверяется: сбой любой из них — итог reporting_failed и код 4
# (исход выпуска — в причине и в ключе published), фаза journal отмечается
# только после записанного раздела, так что --continue допишет только отчёт.
FINISH_CODE=""
finish() {
    finish_record "$@"
    exit "$FINISH_CODE"
}
finish_record() {
    local status=$1 code=$2 reason=$3 failed="" outcome=$1
    FINISHED=1
    set +e
    if [[ -f $STATE ]]; then
        if append_journal "$status" "$reason"; then
            if [[ $status == "done" ]] && ! phase_set journal "done"; then failed+=" state ($STATE)"; fi
        else
            failed+=" journal ($JOURNAL)"
        fi
        state_set last_status "$status" || [[ $failed == *"state ("* ]] || failed+=" state ($STATE)"
    fi
    if [[ -n $failed ]]; then
        reason="release $outcome: $reason; REPORTING FAILED:$failed"
        status=reporting_failed
        code=4
    fi
    if ! write_result "$status" "$code" "$reason"; then
        failed+=" result ($RESULT)"
        code=4
    fi
    FINISH_CODE=$code
    if [[ $outcome == "done" || $outcome == stopped_before_publish || $outcome == publish_failed ]]; then
        print_owner_checklist
    fi
    if [[ -n $CUTOVER_RESULT && $(kv "$CUTOVER_RESULT" stop_block) != "" ]]; then
        log "STOP block: $(kv "$CUTOVER_RESULT" stop_block)"
    fi
    if [[ -n $failed ]]; then
        log "release REPORTING FAILED:$failed; the release itself: $outcome (published: $PUBLISHED): $3; fix the destination, then scripts/release.sh ship $VER --continue redoes the reporting only"
    elif [[ $status == "done" ]]; then
        log "release PASS: $VER ($SHA12) deployed and published; result $RESULT"
    else
        log "release STOP ($status): $reason; result $RESULT"
    fi
}

# Любой выход ship: неожиданный после старта — итог error; затем дождаться,
# пока tee допишет лог.
on_exit() {
    local rc=$?
    set +e
    if [[ $FINISHED == 0 && $RUN_STARTED == 1 ]]; then
        finish_record error 1 "release.sh stopped unexpectedly (exit $rc) in phase $CURRENT_PHASE; see $LOG"
        rc=$FINISH_CODE
    elif [[ $FINISHED == 0 && -n $SIGNALED ]]; then
        record_attempt interrupted 1 "release.sh got SIG$SIGNALED before the run started; nothing ran"
        rc=$FINISH_CODE
    elif [[ $FINISHED == 0 && -n $ATTEMPT ]]; then
        record_attempt error 1 "release.sh stopped unexpectedly (exit $rc) before the run started; see $LOG"
        rc=$FINISH_CODE
    fi
    exec 1>&- 2>&-
    [[ -z $TEE_PID ]] || wait "$TEE_PID" 2>/dev/null
    exit "$rc"
}

stop_if_signaled() {
    [[ -z $SIGNALED ]] || finish interrupted 1 "release.sh got SIG$SIGNALED in phase $CURRENT_PHASE; see the state, resume with --continue"
}

# Тот же коммит, ветка main, чистое дерево — перед каждой фазой.
check_pin() {
    local head branch st=failed_before_cutover
    # После боевого прогона прод уже на релизе: остановка — до публикации.
    [[ $(phase_status execute) != "done" ]] || st=stopped_before_publish
    head=$(git rev-parse HEAD)
    [[ $head == "$SHA" ]] || finish "$st" 1 "HEAD moved from the pinned $SHA to $head"
    branch=$(git symbolic-ref -q --short HEAD || true)
    [[ $branch == main ]] || finish "$st" 1 "the checkout left branch main (now ${branch:-detached})"
    [[ -z $(git status --porcelain --untracked-files=normal) ]] || finish "$st" 1 "the tree became dirty"
}

# --- ship: предусловия ---------------------------------------------------------

refuse() {
    printf '[%s] REFUSED: %s\n' "$(date -u +%H:%M:%S)" "$*" >&2
    # Отказ — своя попытка: итог прошлой остановки и состояние фаз не трогаются.
    record_attempt refused 2 "$*"
    exit "$FINISH_CODE"
}

site_values() {
    # Действующие значения так, как их видят scripts/deploy и build-release.sh.
    bash -c 'source "$1/scripts/deploy/deploy.conf"
        printf "%s=%s\n" SITE_ORIGIN "${SITE_ORIGIN}" WSS_ORIGIN "${WSS_ORIGIN}" SITE_CODES "${SITE_CODES}" HUB_IP "${HUB_IP}" \
            CANTERLOT_SITE_ORIGIN "${CANTERLOT_SITE_ORIGIN:-https://hub.example.com}" \
            CANTERLOT_WSS_ORIGIN "${CANTERLOT_WSS_ORIGIN:-wss://hub.example.com}"' _ "$repo"
}

preconditions() {
    local branch head ver att key value bad=() deny dir free
    [[ $(git rev-parse --absolute-git-dir) == "$(cd "$(git rev-parse --git-common-dir)" && pwd)" ]] \
        || refuse "run from the main checkout, not from a linked worktree"
    branch=$(git symbolic-ref -q --short HEAD || true)
    [[ $branch == main ]] || refuse "the checkout is on ${branch:-a detached HEAD}, not on main"
    [[ -z $(git status --porcelain --untracked-files=normal) ]] || refuse "the tree is dirty (git status --porcelain is not empty)"
    head=$(git rev-parse HEAD)
    if [[ -n $SHA && $head != "$SHA" ]]; then
        refuse "HEAD $head is not the pinned release commit $SHA"
    fi
    ver=$(head_version) || refuse "cannot read [workspace.package] version from Cargo.toml"
    [[ $ver == "$VER" ]] || refuse "HEAD is version $ver, not $VER: run scripts/release.sh prepare $VER first"
    scripts/check-version.sh >/dev/null || refuse "scripts/check-version.sh failed"
    att="$GATE_DIR/$head.ok"
    [[ -s $att ]] || refuse "no gate attestation for HEAD $head ($att): run scripts/gate.sh"
    [[ $(kv "$att" sha) == "$head" ]] || refuse "the gate attestation $att was written for another commit"
    while IFS='=' read -r key value; do
        if [[ $key == *_exit && $value != 0 ]]; then refuse "the gate attestation $att records $key=$value"; fi
    done <"$att"
    while IFS='=' read -r key value; do
        if [[ $key == SITE_ORIGIN ]]; then SITE_ORIGIN_EFF=$value; fi
        for deny in "${EXAMPLE_DENYLIST[@]}"; do
            if [[ $value == *"$deny"* ]]; then bad+=("$key"); fi
        done
    done < <(site_values)
    # Только имена: значения стенда в лог не печатаются.
    ((${#bad[@]} == 0)) || refuse "effective site values are the neutral example (${bad[*]}): fill .superpowers/site.env"
    [[ $SITE_ORIGIN_EFF =~ ^https://[^/[:space:]]+$ ]] || refuse "effective SITE_ORIGIN is not an https origin"
    for dir in "$repo" "$HOME"; do
        free=$(df -Pk "$dir" | awk 'NR == 2 {print int($4 / 1024)}')
        ((free >= MIN_FREE_MB)) || refuse "only $free MB free on the file system of $dir (need $MIN_FREE_MB MB)"
    done
    log "preconditions OK: main checkout on main, clean, version $VER, gate attestation for $head, site values set, disk space"
}

report_state() {
    log "a release state exists: $STATE"
    sed 's/^/  /' "$STATE"
    [[ ! -f $RESULT ]] || log "last result: status $(kv "$RESULT" status): $(kv "$RESULT" reason)"
    if [[ -n $(phase_status execute) ]]; then
        log "the real cutover already started; --continue only resumes check-proxy/publish/journal after a done cutover"
    else
        log "resume with: scripts/release.sh ship $VER --continue"
    fi
}

# --- ship: фазы ----------------------------------------------------------------

phase_tag() {
    if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
        [[ $(git rev-parse "$TAG^{commit}") == "$SHA" ]] \
            || finish failed_before_cutover 1 "tag $TAG already exists at another commit"
        log "tag $TAG already at $SHA12"
    else
        git tag -s "$TAG" -m "Версия $VER" || finish failed_before_cutover 1 "git tag -s $TAG failed"
    fi
    phase_set tag "done"
}

remote_at_sha() {
    local refs
    refs=$(git ls-remote origin "refs/heads/main" "refs/tags/$TAG" "refs/tags/$TAG^{}") || return 1
    grep -qx "$SHA	refs/heads/main" <<<"$refs" && grep -qx "$SHA	refs/tags/$TAG^{}" <<<"$refs"
}

phase_push() {
    if remote_at_sha; then
        log "origin already has main and $TAG at $SHA12"
    else
        git push --atomic origin "refs/heads/main:refs/heads/main" "refs/tags/$TAG:refs/tags/$TAG" \
            || finish failed_before_cutover 1 "git push --atomic of main and $TAG failed"
        remote_at_sha || finish failed_before_cutover 1 "origin does not show main and $TAG at $SHA12 after the push"
    fi
    phase_set push "done"
}

dist_ok() {
    local d="$DIST_ROOT/$SHA12"
    [[ -f $d/SHA256SUMS && -f $d/RELEASE && -f $d/EXPECT ]] || return 1
    (cd "$d" && sha256sum --quiet -c SHA256SUMS) || return 1
    grep -qx "source=$SHA" "$d/RELEASE" || return 1
    grep -qx 'format=1' "$d/EXPECT"
}

phase_build() {
    local blog="$STATE_DIR/build-$VER.log"
    if [[ -e $DIST_ROOT/$SHA12 ]]; then
        dist_ok || finish failed_before_cutover 1 "$DIST_ROOT/$SHA12 exists but is not a complete build of $SHA12: remove that directory by hand, then --continue"
        log "dist $DIST_ROOT/$SHA12 already built from $SHA12"
    else
        state_set log_build "$blog"
        log "build: scripts/build-release.sh $TAG --local (log $blog)"
        CANTERLOT_DIST_ROOT="$DIST_ROOT" scripts/build-release.sh "$TAG" --local >"$blog" 2>&1 \
            || finish failed_before_cutover 1 "scripts/build-release.sh failed, see $blog"
    fi
    phase_set build "done"
}

phase_dist() {
    local d="$DIST_ROOT/$SHA12"
    dist_ok || finish failed_before_cutover 1 "dist check failed for $d (SHA256SUMS, RELEASE source=, EXPECT format=1)"
    log "dist: SHA256SUMS OK, $(find "$d" -type f | wc -l) files, source=$SHA, $(grep -m1 '^capabilities=' "$d/EXPECT" || echo 'no capabilities line')"
    phase_set dist "done"
}

phase_stage() {
    local slog="$STATE_DIR/stage-$VER.log"
    state_set log_stage "$slog"
    scripts/deploy/stage.sh "$SHA12" --source "$repo" --dist "$DIST_ROOT" >"$slog" 2>&1 \
        || finish failed_before_cutover 1 "scripts/deploy/stage.sh failed, see $slog"
    grep -q "staged $SHA12 everywhere" "$slog" || finish failed_before_cutover 1 "stage.sh did not report 'staged $SHA12 everywhere', see $slog"
    phase_set stage "done"
}

# Отказ репетиции только из переходных кодов (и без сигнала) — повторяемый.
rehearsal_transient() {
    local r=$1 code
    cutover_result_complete "$r" || return 1
    [[ -z $(kv "$r" signal) ]] || return 1
    [[ $(kv "$r" exit_code) =~ ^[01]$ ]] || return 1
    [[ -n $(kv "$r" fail_codes) ]] || return 1
    for code in $(kv "$r" fail_codes | tr ',' ' '); do
        [[ $TRANSIENT_CODES == *" $code "* ]] || return 1
    done
}

run_rehearsal() {
    local n
    n=$(next_seq rehearsals)
    REHEARSAL_RESULT="$STATE_DIR/rehearsal-$VER-$n.result"
    REHEARSAL_LOG="$STATE_DIR/rehearsal-$VER-$n.log"
    state_set rehearsal_result "$REHEARSAL_RESULT"
    state_set log_rehearsal "$REHEARSAL_LOG"
    log "rehearsal #$n: scripts/deploy/cutover.sh $SHA12 --rehearse (log $REHEARSAL_LOG)"
    REHEARSAL_RC=0
    scripts/deploy/cutover.sh "$SHA12" --rehearse --source "$repo" --dist "$DIST_ROOT" \
        --result "$REHEARSAL_RESULT" >"$REHEARSAL_LOG" 2>&1 || REHEARSAL_RC=$?
    state_set rehearsal_exit "$REHEARSAL_RC"
}

rehearsal_passed() {
    local r=$REHEARSAL_RESULT
    [[ $REHEARSAL_RC == 0 ]] && cutover_result_complete "$r" && [[ $(kv "$r" status) == pass ]] \
        && [[ $(kv "$r" exit_code) == 0 && -z $(kv "$r" signal) ]] && [[ $(kv "$r" soft_failures) == 0 ]] \
        && [[ -n $(kv "$r" token) ]]
}

phase_rehearsal() {
    local retried=0
    phase_set rehearsal started
    run_rehearsal
    stop_if_signaled
    if ! rehearsal_passed; then
        REHEARSAL_CODES=$(kv "$REHEARSAL_RESULT" fail_codes)
        if rehearsal_transient "$REHEARSAL_RESULT"; then
            retried=1
            REHEARSAL_RETRIED=yes
            state_set retried_codes "$REHEARSAL_CODES"
            log "rehearsal failed only with transient codes ($REHEARSAL_CODES): one fresh rehearsal in $RETRY_DELAY s"
            sleep "$RETRY_DELAY"
            stop_if_signaled
            run_rehearsal
            stop_if_signaled
        fi
    fi
    if ! rehearsal_passed; then
        phase_set rehearsal failed
        REHEARSAL_CODES=$(kv "$REHEARSAL_RESULT" fail_codes)
        local why="rehearsal failed"
        if ! cutover_result_complete "$REHEARSAL_RESULT"; then
            why+=" without a complete result file (exit $REHEARSAL_RC)"
        else
            why+=": status $(kv "$REHEARSAL_RESULT" status), codes ${REHEARSAL_CODES:-none}, soft failures $(kv "$REHEARSAL_RESULT" soft_failures)"
            [[ -z $(kv "$REHEARSAL_RESULT" signal) ]] || why+=", signal $(kv "$REHEARSAL_RESULT" signal)"
        fi
        ((retried == 0)) || why+=" (after one retry)"
        finish failed_before_cutover 1 "$why; see $REHEARSAL_LOG"
    fi
    TOKEN=$(kv "$REHEARSAL_RESULT" token)
    log "rehearsal PASS: plan $(kv "$REHEARSAL_RESULT" plan), token valid until $(kv "$REHEARSAL_RESULT" token_valid_until)"
    phase_set rehearsal "done"
}

# Токен отвергнут до любой записи: срок или изменённый план.
token_rejected_before_writes() {
    local r=$1 code
    cutover_result_complete "$r" || return 1
    [[ $(kv "$r" status) == refused && $(kv "$r" phase) == token && -z $(kv "$r" signal) ]] || return 1
    [[ $(kv "$r" vzdump_done) == no && $(kv "$r" worker_stopped) == no ]] || return 1
    [[ -n $(kv "$r" fail_codes) ]] || return 1
    for code in $(kv "$r" fail_codes | tr ',' ' '); do
        [[ $TOKEN_CODES == *" $code "* ]] || return 1
    done
}

run_execute() {
    local n rc=0
    n=$(next_seq executes)
    CUTOVER_RESULT="$STATE_DIR/cutover-$VER-$n.result"
    CUTOVER_LOG="$STATE_DIR/cutover-$VER-$n.log"
    state_set cutover_result "$CUTOVER_RESULT"
    state_set log_cutover "$CUTOVER_LOG"
    # Фаза отмечается начатой ДО запуска: после этого --continue её не повторит.
    phase_set execute started
    log "real cutover: scripts/deploy/cutover.sh --execute $SHA12 --confirm <rehearsal token> (log $CUTOVER_LOG)"
    scripts/deploy/cutover.sh --execute "$SHA12" --confirm "$TOKEN" --source "$repo" --dist "$DIST_ROOT" \
        --result "$CUTOVER_RESULT" >"$CUTOVER_LOG" 2>&1 || rc=$?
    CUTOVER_RC=$rc
    state_set execute_exit "$rc"
    log "cutover exited $rc"
}

# Боевой прогон завершён чисто, только если согласны все признаки: статус done,
# exit_code=0 в файле, настоящий код выхода процесса 0 и пустой signal.
# Иначе — пусто (согласовано) или описание расхождения.
cutover_inconsistency() {
    local r=$1 rc=$2
    [[ $(kv "$r" status) == "done" && $(kv "$r" exit_code) == 0 && $rc == 0 && -z $(kv "$r" signal) ]] && return 0
    printf 'status %s, exit_code %s, process exit %s, signal %s' \
        "$(kv "$r" status)" "$(kv "$r" exit_code)" "${rc:-unknown}" "$(kv "$r" signal)"
}

phase_execute() {
    local r status token_retry=0
    while :; do
        run_execute
        r=$CUTOVER_RESULT
        if ((token_retry == 0)) && token_rejected_before_writes "$r"; then
            token_retry=1
            log "the token was rejected before any write ($(kv "$r" fail_codes)): one fresh rehearsal, then the real run"
            phase_set execute "refused_token"
            phase_rehearsal
            check_pin
            continue
        fi
        break
    done
    if ! cutover_result_complete "$r"; then
        phase_set execute unknown
        finish cutover_unknown 1 "the real cutover left no complete result file ($r): read $CUTOVER_LOG; the cutover is never re-run"
    fi
    status=$(kv "$r" status)
    case "$status" in
        "done")
            local bad
            bad=$(cutover_inconsistency "$r" "$CUTOVER_RC")
            if [[ -n $bad ]]; then
                phase_set execute inconsistent
                finish stopped_before_publish 1 "the real cutover result is inconsistent ($bad): production may be on $SHA12, nothing published; read $CUTOVER_LOG and the STOP block, if any"
            fi
            phase_set execute "done" ;;
        stop_and_report)
            phase_set execute stop_and_report
            finish stop_and_report 1 "cutover STOP-AND-REPORT after the hub flip (block $(kv "$r" stop_block)); nothing published" ;;
        failed_before_flip)
            phase_set execute failed_before_flip
            finish failed_before_flip 1 "cutover failed before the hub flip (recovery $(kv "$r" recovery), codes $(kv "$r" fail_codes)); nothing published" ;;
        refused)
            phase_set execute refused
            finish failed_before_cutover 1 "the real cutover refused before any write (codes $(kv "$r" fail_codes)); production unchanged, nothing published" ;;
        *)
            phase_set execute unknown
            finish cutover_unknown 1 "unexpected cutover status '$status' in $r" ;;
    esac
}

phase_proxy() {
    local plog="$STATE_DIR/check-proxy-$VER.log"
    state_set log_proxy "$plog"
    # Только проверки без сеанса: --positive шлёт владельцу push о входе.
    if scripts/check-proxy.sh "$SITE_ORIGIN_EFF" >"$plog" 2>&1; then
        PROXY_CHECK=pass
        phase_set proxy "done"
    else
        PROXY_CHECK=fail
        phase_set proxy failed
    fi
}

phase_publish() {
    local r=$CUTOVER_RESULT plog="$STATE_DIR/publish-$VER.log"
    if [[ $(kv "$r" soft_failures) != 0 ]]; then
        phase_set publish withheld
        finish stopped_before_publish 1 "cutover done with soft failures $(kv "$r" soft_failures) ($(kv "$r" soft_by_kind)); publication withheld until reviewed (then scripts/publish-release.sh $TAG by hand)"
    fi
    if [[ $PROXY_CHECK != pass ]]; then
        phase_set publish withheld
        finish stopped_before_publish 1 "check-proxy.sh failed after the cutover, see $(state_get log_proxy); publication withheld"
    fi
    state_set log_publish "$plog"
    phase_set publish started
    if ! scripts/publish-release.sh "$TAG" >"$plog" 2>&1; then
        phase_set publish failed
        finish publish_failed 1 "scripts/publish-release.sh failed (publication may be partial), see $plog; it is re-entrant: --continue"
    fi
    PUBLISHED=yes
    phase_set publish "done"
}

# После начала боевого прогона --continue только дочитывает его итог.
resume_after_execute() {
    local r
    r=$(state_get cutover_result)
    CUTOVER_RESULT=$r
    CUTOVER_LOG=$(state_get log_cutover)
    REHEARSAL_RESULT=$(state_get rehearsal_result)
    REHEARSAL_LOG=$(state_get log_rehearsal)
    if ! cutover_result_complete "$r" || [[ $(kv "$r" status) != "done" ]] \
        || [[ -n $(cutover_inconsistency "$r" "$(state_get execute_exit)") ]]; then
        log "the real cutover already ran (phase execute: $(state_get phase_execute)); it is never re-run"
        local why
        if cutover_result_complete "$r"; then
            why="cutover status $(kv "$r" status), process exit $(state_get execute_exit); $(cutover_inconsistency "$r" "$(state_get execute_exit)"); STOP block: $(kv "$r" stop_block)"
        else
            why="no complete cutover result ($r): read $CUTOVER_LOG and check whether cutover.sh still runs"
        fi
        log "$why"
        record_attempt continue_refused 1 "the real cutover already ran and is not done cleanly ($why); it is never re-run, nothing published"
        exit "$FINISH_CODE"
    fi
    [[ $(phase_status execute) == "done" ]] || phase_set execute "done"
    log "the real cutover is done ($r): resuming check-proxy, publication and the journal"
}

ship() {
    local p
    VER=$1
    TAG="v$VER"
    STATE_DIR=${CANTERLOT_RELEASE_DIR:-$repo/.superpowers/deploy}
    GATE_DIR=${CANTERLOT_GATE_DIR:-$repo/.superpowers/gate}
    DIST_ROOT=${CANTERLOT_DIST_ROOT:-$repo/.superpowers/deploy/dist}
    RETRY_DELAY=${RELEASE_RETRY_DELAY:-120}
    MIN_FREE_MB=${RELEASE_MIN_FREE_MB:-10240}
    mkdir -p "$STATE_DIR"
    STATE="$STATE_DIR/release-$VER.state"
    RESULT="$STATE_DIR/release-$VER.result"
    LOG="$STATE_DIR/release-$VER.log"
    JOURNAL="$STATE_DIR/pilot-deploy-log.md"
    # Ход — и в терминал, и в лог; отвалившийся терминал не останавливает запись в лог.
    exec > >(tee -a --output-error=warn "$LOG") 2>&1
    TEE_PID=$!
    trap on_exit EXIT
    # До старта сигнал — выход с итогом interrupted; после старта — флаг SIGNALED.
    trap 'SIGNALED=INT; exit 130' INT
    trap 'SIGNALED=TERM; exit 143' TERM
    # Отвалившийся терминал выпуск не останавливает (запуск в фоне).
    trap '' HUP
    new_attempt || { log "cannot create an attempt result in $STATE_DIR"; exit 4; }
    log "release.sh ship $VER$([[ $CONTINUE == 1 ]] && echo ' --continue') attempt $ATTEMPT (pid $$, log $LOG, result $ATTEMPT_RESULT)"
    # Выпуск один на чекаут. Блокировку наследуют дети: пока жив cutover.sh,
    # даже после гибели release.sh, второй запуск её не получит.
    exec 9>>"$STATE_DIR/release.lock"
    if ! flock -n 9; then
        log "another scripts/release.sh holds $STATE_DIR/release.lock: not starting"
        [[ ! -f $STATE ]] || report_state
        record_attempt blocked 3 "another scripts/release.sh holds $STATE_DIR/release.lock"
        exit "$FINISH_CODE"
    fi
    if [[ -f $STATE ]]; then
        if [[ $CONTINUE == 0 ]]; then
            report_state
            record_attempt state_exists 3 "a saved release state exists ($STATE): resume with --continue"
            exit "$FINISH_CODE"
        fi
        SHA=$(state_get sha)
        if ! [[ $SHA =~ ^[0-9a-f]{40}$ ]]; then
            log "state $STATE has no pinned sha"
            record_attempt refused 2 "state $STATE has no pinned sha"
            exit "$FINISH_CODE"
        fi
    elif [[ $CONTINUE == 1 ]]; then
        log "no saved state $STATE: nothing to continue (run without --continue)"
        record_attempt refused 2 "no saved state $STATE: nothing to continue"
        exit "$FINISH_CODE"
    fi
    preconditions
    RUN_STARTED=1
    trap 'SIGNALED=INT' INT
    trap 'SIGNALED=TERM' TERM
    STARTED_AT=$(ts)
    if [[ -z $SHA ]]; then
        SHA=$(git rev-parse HEAD)
        state_set format 1
        state_set version "$VER"
        state_set tag "$TAG"
        state_set sha "$SHA"
        state_set started_at "$STARTED_AT"
        log "pinned $SHA in $STATE"
    fi
    SHA12=${SHA:0:12}
    if [[ -n $(phase_status execute) ]]; then
        resume_after_execute
    fi
    if [[ $(phase_status proxy) == "done" ]]; then PROXY_CHECK=pass; fi
    if [[ $(phase_status publish) == "done" ]]; then PUBLISHED=yes; fi
    for p in "${PHASES[@]}"; do
        CURRENT_PHASE=$p
        stop_if_signaled
        case "$p" in
            journal) break ;;
            rehearsal | execute)
                # Пока боевой прогон не начат, репетиция каждый раз свежая (токен 2 ч).
                if [[ $(phase_status execute) == "done" ]]; then continue; fi ;;
            *)
                if [[ $(phase_status "$p") == "done" ]]; then
                    log "phase $p: already done"
                    continue
                fi ;;
        esac
        check_pin
        "phase_$p"
    done
    CURRENT_PHASE=journal
    finish "done" 0 "hub, worker and PWA on $SHA12, check-proxy passed, $TAG published"
}

case "${1:-}" in
    prepare)
        [[ $# == 2 ]] || usage
        prepare "$2" ;;
    ship)
        [[ $# == 2 || $# == 3 ]] || usage
        [[ ${2:-} =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
        if [[ $# == 3 ]]; then
            [[ $3 == --continue ]] || usage
            CONTINUE=1
        fi
        ship "$2" ;;
    *) usage ;;
esac
