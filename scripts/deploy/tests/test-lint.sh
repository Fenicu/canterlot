#!/usr/bin/env bash
# Тесты статической проверки pipe-lint.py: каждое правило ловит образец ошибки,
# которая уже роняла прод, и молчит на исправленной форме; настоящие скрипты
# выката чисты. Процессов, хостов и fake-bin не нужно.
# shellcheck disable=SC2016 # образцы — литеральный текст скриптов
HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"
LINT="${HARNESS_DIR}/pipe-lint.py"
WORK="$(mktemp -d /tmp/canterlot-lint-test.XXXXXX)"
trap 'rm -rf "${WORK}"' EXIT
PASS_COUNT=0
FAIL_COUNT=0
ok() { PASS_COUNT=$((PASS_COUNT + 1)); printf 'PASS: %s\n' "$1"; }
bad() { FAIL_COUNT=$((FAIL_COUNT + 1)); printf 'FAIL: %s\n' "$1" >&2; }

# lint_case ИМЯ КОД ОЖИДАНИЕ(hit|clean) ТЕКСТ
lint_case() {
    local name="$1" code="$2" want="$3" text="$4" f out
    f="${WORK}/${name}.sh"
    printf '#!/usr/bin/env bash\nset -euo pipefail\n%s\n' "${text}" >"${f}"
    out="$(python3 "${LINT}" --prod "${f}" 2>&1 || true)"
    case "${want}:${out}" in
        hit:*"[${code}]"*) ok "${name}: [${code}] caught" ;;
        clean:*"[${code}]"*) bad "${name}: [${code}] false positive: ${out}" ;;
        clean:*) ok "${name}: clean form passes" ;;
        *) bad "${name}: [${code}] missed: ${out}" ;;
    esac
}

printf '=== case: the real deploy scripts are clean ===\n'
if out="$(python3 "${LINT}")"; then
    ok "scripts/deploy and tests: $(printf '%s' "${out}" | tail -1)"
else
    bad "real scripts have lint problems:"; printf '%s\n' "${out}" >&2
fi

printf '=== case: sigpipe (1.19.0) ===\n'
lint_case sigpipe-head sigpipe hit 'x="$(journalctl -u w | head -1)"'
lint_case sigpipe-grepq sigpipe hit 'if ldd /bin/ls | grep -q libssl; then echo y; fi'
lint_case sigpipe-python-break sigpipe clean 'x="$(journalctl -u w | python3 -c "import sys; [None for _ in sys.stdin]" || true)"'
lint_case sigpipe-sed1p sigpipe clean 'x="$(journalctl -u w | sed -n 1p)"'
lint_case sigpipe-sedq sigpipe hit "x=\"\$(journalctl -u w | sed -n '1p;q')\""
lint_case sigpipe-awk-exit sigpipe hit "x=\"\$(ps -e | awk '{print; exit}')\""
lint_case sigpipe-quoted-remote sigpipe clean 'x="$(ssh h "ls -t /d | head -1")"'
lint_case sigpipe-suppressed sigpipe clean 'x="$(cat small | head -1)"  # lint: sigpipe-ok bounded input'

printf '=== case: wait-subst (1.18.0, 1.18.1) ===\n'
lint_case wait-unguarded wait-subst hit 'for _ in 1 2 3; do
    PID="$(systemctl --user show w -p MainPID --value)"
    if [ -n "${PID}" ]; then break; fi
    sleep 1
done'
lint_case wait-guarded wait-subst clean 'for _ in 1 2 3; do
    PID="$(systemctl --user show w -p MainPID --value)" || PID=""
    if [ -n "${PID}" ]; then break; fi
    sleep 1
done'
lint_case wait-no-sleep wait-subst clean 'for x in a b; do
    y="$(printf %s "${x}")"
    echo "${y}"
done'
lint_case wait-arith wait-subst clean 'while :; do
    n=$((n + 1))
    sleep 1
done'

printf '=== case: test-break (1.17.0, 1.18.0) ===\n'
lint_case test-and-break test-break hit 'for _ in 1 2; do
    [ -n "${LIVE_TS:-}" ] && break
    sleep 1
done'
lint_case test-last-in-body test-break hit 'for p in 1 2; do
    echo "${p}"
    [ "${p}" = 2 ] && echo two
done'
lint_case if-break test-break clean 'for _ in 1 2; do
    if [ -n "${LIVE_TS:-}" ]; then break; fi
    sleep 1
done'

printf '=== case: source-exec (1.16.1) ===\n'
mkdir -p "${WORK}/lib"
printf '#!/usr/bin/env bash\n' >"${WORK}/lib/helper.sh"
chmod +x "${WORK}/lib/helper.sh"
cat >"${WORK}/sourcer.sh" <<'EOF'
#!/usr/bin/env bash
source "$(dirname "${BASH_SOURCE[0]}")/lib/helper.sh"
EOF
if python3 "${LINT}" "${WORK}/sourcer.sh" | grep -F -- '[source-exec]' >/dev/null; then ok "source of an executable helper caught"; else bad "source of an executable helper missed"; fi
chmod -x "${WORK}/lib/helper.sh"
if python3 "${LINT}" "${WORK}/sourcer.sh" | grep -F -- '[source-exec]' >/dev/null; then bad "non-executable library flagged"; else ok "non-executable library passes"; fi
printf '#!/usr/bin/env bash\n' >"${WORK}/lib/cutover.sh"
cat >"${WORK}/sourcer2.sh" <<'EOF'
#!/usr/bin/env bash
source "$(dirname "${BASH_SOURCE[0]}")/lib/cutover.sh"
EOF
if python3 "${LINT}" "${WORK}/sourcer2.sh" | grep -F -- 'entry point cutover.sh' >/dev/null; then ok "source of the cutover entry point caught"; else bad "source of cutover missed"; fi
if [ -x "${DEPLOY_DIR}/lib.sh" ] || [ -x "${HARNESS_DIR}/lib-harness.sh" ] || [ -x "${HARNESS_DIR}/cutover-fixture.sh" ]; then
    bad "a sourced library is executable"
else
    ok "lib.sh, lib-harness.sh and cutover-fixture.sh are not executable"
fi

printf '=== case: self-match (1.16.0) ===\n'
lint_case pgrep-f self-match hit 'pgrep -f "supervise --exec-dir /x" >/dev/null'
lint_case ps-grep-literal self-match hit 'ps -eo pid=,args= | grep vzdump'
lint_case ps-grep-bracket self-match clean 'ps -eo pid=,args= | grep "[v]zdump" || true'
lint_case joined-substring self-match hit 'case "${cmdline}" in *canterlot-worker*supervise*) echo y ;; esac'

printf '=== case: run-write (rehearsal choke point) ===\n'
lint_case raw-stop run-write hit 'systemctl --user stop canterlot-worker'
lint_case wrapped-stop run-write clean 'run_write "stop worker" systemctl --user stop canterlot-worker'
lint_case raw-locked run-write hit '    hub_locked "${HUB_FLIP_SCRIPT}" a b c'
lint_case raw-locked-subst run-write hit 'X="$(hub_locked "${HUB_FLIP_SCRIPT}" a b c)"'
lint_case wrapped-locked-subst run-write clean 'X="$(run_write "hub flip" hub_locked "${HUB_FLIP_SCRIPT}" a b c)"'
lint_case ssh-remote-write run-write hit 'ssh_web "cp a b"'
lint_case ssh-remote-read run-write clean 'ssh_web "cat /etc/x"'
lint_case pipe-recv run-write hit 'printf x | recv_to web "cat > /x"'
lint_case pipe-recv-wrapped run-write clean 'printf x | run_write_pipe "write env" recv_to web "cat > /x"'
lint_case pg-restore-list run-write clean 'ssh_hub "pg_restore --list /d" >/dev/null || true'
lint_case local-temp run-write clean 'rm -rf "${dir}"  # lint: local-temp'
lint_case writer-definition run-write clean 'reserve_releases() {
    hub_locked "${RESERVE_SCRIPT}" a "$@"
}'

printf 'passed: %s, failed: %s\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
