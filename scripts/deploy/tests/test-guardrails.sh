#!/usr/bin/env bash
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
# Собственные ограждения набора: ни один скрипт выкатки не печатает окружение
# контейнеров и командные строки процессов. Статический разбор текстов скриптов
# плюс поведенческая проверка proc_ticks. Прод не трогается.
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib-harness.sh
source "${HARNESS_DIR}/lib-harness.sh"

SCRIPTS=("${DEPLOY_DIR}"/*.sh "${DEPLOY_DIR}"/*.py)

# G1: окружение контейнера — только по одной именованной несекретной переменной.
case_begin "no script dumps container environment"
BAD="$(grep -nE 'Config\.Env' "${SCRIPTS[@]}" || true)"
[ -z "${BAD}" ] && case_ok "no Config.Env" || case_fail "Config.Env in scripts: ${BAD}"
# printenv обязан получать имя из белого списка несекретных переменных.
BAD="$(grep -nE 'printenv' "${SCRIPTS[@]}" | grep -vE 'printenv (CANTERLOT_CSP|GODEBUG)([^A-Za-z0-9_]|$)' || true)"
[ -z "${BAD}" ] && case_ok "printenv only for CANTERLOT_CSP/GODEBUG" || case_fail "printenv without a whitelisted name: ${BAD}"
# docker exec ... env / sh -c env, голый env в конвейере удалённых команд.
BAD="$(grep -nE 'docker (exec|run)[^"|]* (env|export)( |"|$)' "${SCRIPTS[@]}" || true)"
[ -z "${BAD}" ] && case_ok "no 'docker exec ... env'" || case_fail "docker exec env: ${BAD}"
# docker inspect только с --format, и формат не касается Env/Config.
BAD="$(grep -nE 'docker inspect' "${SCRIPTS[@]}" | grep -vE -- '(--format|-f) ' || true)"
[ -z "${BAD}" ] && case_ok "docker inspect always with --format" || case_fail "docker inspect without --format: ${BAD}"
case_end

# G2: /proc/*/environ не читается нигде; /cmdline — только в lib.sh, где строка идёт
# на сопоставление, а наружу уходит argv[0] с подкомандой (проверка G3).
case_begin "no script reads process environ or prints cmdlines"
BAD="$(grep -nE '/environ' "${SCRIPTS[@]}" || true)"
[ -z "${BAD}" ] && case_ok "no /environ" || case_fail "/environ in scripts: ${BAD}"
BAD="$(grep -lE '/cmdline' "${SCRIPTS[@]}" | grep -v '/lib\.sh$' || true)"
[ -z "${BAD}" ] && case_ok "/cmdline only in lib.sh" || case_fail "/cmdline outside lib.sh: ${BAD}"
# ps с аргументами допустим только внутри assert_no_remote_proc (выход там режется до pid и argv[0]).
BAD="$(grep -nE 'ps +(-[a-zA-Z]*o|-ef|aux|-eo)[^|]*args|pgrep +-[a-z]*a' "${SCRIPTS[@]}" | grep -v 'ps -eo pid=,args=' || true)"
[ -z "${BAD}" ] && case_ok "no other ps/pgrep that prints args" || case_fail "ps with args: ${BAD}"
assert_contains "${DEPLOY_DIR}/lib.sh" "awk '{print \$1, \$2}'" "assert_no_remote_proc prints pid and argv[0] only"
case_end

# G3: proc_ticks находит процесс по любому месту командной строки, но печатает только
# argv[0] и подкоманду; секретный аргумент в вывод не попадает.
case_begin "proc_ticks prints only argv[0] and the subcommand"
# shellcheck disable=SC1091 # lib.sh рядом с тестами, shellcheck без -x
source "${DEPLOY_DIR}/lib.sh"
python3 -I -c 'import time; time.sleep(20)' --token SECRET-MARKER-9f3a &
HARNESS_PIDS+=("$!")
sleep 0.3
OUT="$(proc_ticks "SECRET-MARKER-9f3a")"
[ -n "${OUT}" ] && case_ok "process found by a deep argument" || case_fail "process not found"
case "${OUT}" in
    *SECRET-MARKER-9f3a*|*time.sleep*) case_fail "proc_ticks leaked arguments: ${OUT}" ;;
    *) case_ok "arguments beyond argv[1] not printed" ;;
esac
case_end

# G4: сама проверка не вакуумна: плохой скрипт ловится теми же шаблонами.
case_begin "guardrail patterns catch a bad script"
BADFILE="$(mktemp)"
printf '%s\n' 'ssh h "docker inspect caddy"' 'ssh h "docker exec caddy printenv"' 'cat /proc/1/environ' >"${BADFILE}"
[ "$(grep -cE 'Config\.Env|/environ' "${BADFILE}")" = "1" ] && case_ok "environ pattern fires" || case_fail "environ pattern silent"
grep -nE 'docker inspect' "${BADFILE}" | grep -qvE -- '(--format|-f) ' && case_ok "inspect pattern fires" || case_fail "inspect pattern silent"
grep -nE 'printenv' "${BADFILE}" | grep -qvE 'printenv (CANTERLOT_CSP|GODEBUG)([^A-Za-z0-9_]|$)' && case_ok "printenv pattern fires" || case_fail "printenv pattern silent"
rm -f "${BADFILE}"
case_end

# G5: source любого скрипта ничего не исполняет (урок 1.16.1: source запустил
# боевой выкат, остановил воркер и попал в trap). lib.sh — библиотека, её source
# разрешён; остальные обязаны вернуться до первой команды.
case_begin "sourcing a deploy script executes nothing"
for script in cutover.sh preflight.sh stage.sh verify.sh retain.sh rollback-db.sh target-check.sh; do
    out="$(bash -c 'source "$1" >/dev/null 2>&1; printf "SOURCED"' _ "${DEPLOY_DIR}/${script}" 2>&1 || true)"
    if [ "${out}" = "SOURCED" ]; then
        case_ok "source ${script}: returned without executing"
    else
        case_fail "source ${script}: output or exit was [${out}]"
    fi
done
case_end

printf 'guardrails: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
