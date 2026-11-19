#!/usr/bin/env bash
# scripts/release.sh ship: выбор пути (fast / rehearsed / full) по fastpath.sh, пропуск
# репетиции и vzdump, --full, отказ в закрытую сторону, запись пути в итог и журнал,
# --continue на каждом пути. cutover.sh и fastpath.sh — подмены стенда release-fixture.
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
# shellcheck source-path=SCRIPTDIR
# shellcheck source=release-fixture.sh
source "${HARNESS_DIR}/release-fixture.sh"
unset -f cargo

setup_release() { setup_case "release-$1"; make_release_repo; }
FAST_PLAN=("execute done vzdump_done=no vzdump_skipped=yes")
REHEARSED_PLAN=("rehearse pass" "execute done vzdump_done=no vzdump_skipped=yes")
FULL_PLAN=("rehearse pass" "execute done")
C="${FAKE_ROOT:-}"
# Вызовы cutover.sh одной строкой на вызов.
cutover_calls() { grep '^cutover ' "${FAKE_ROOT}/state/calls.log" || true; }
journal() { printf '%s' "${RDIR}/pilot-deploy-log.md"; }
reasons_line() { grep -m1 'release path:' "${RDIR}/release-${RVER}.log" || true; }

case_begin "fast path: no rehearsal, no token, the real run is --fast, the path is recorded everywhere"
setup_release fast
route_is fast "no migration and no deploy tooling change since the live release"
cutover_plan "${FAST_PLAN[@]}"
run_release ship "${RVER}"
SHA12="$(git -C "${RREPO}" rev-parse --short=12 HEAD)"
assert_eq "${RREL_RC}" "0" "exit 0"
C="${FAKE_ROOT}/state/calls.log"
assert_eq "$(grep -c -- '--rehearse' "${C}" || true)" "0" "no rehearsal ran"
assert_contains "${C}" "cutover --execute ${SHA12} --fast " "the real run is --fast"
assert_not_contains "${C}" "--confirm" "no token is passed"
assert_not_contains "${C}" "--skip-ct-backup" "--fast carries the vzdump skip itself"
assert_contains "${C}" "fastpath ${SHA12} --source" "fastpath.sh was asked about the target"
in_order "${C}" "stage ${SHA12} --source" "fastpath ${SHA12}" "--execute ${SHA12} --fast" "check-proxy" "publish-release" \
    && case_ok "order: stage -> decision -> fast execute -> check-proxy -> publish" || case_fail "order differs"
assert_eq "$(rstate phase_rehearsal)" "skipped" "state: phase rehearsal skipped"
assert_eq "$(rstate phase_execute)" "done" "state: phase execute done"
assert_eq "$(rstate route_path)" "fast" "state: route_path=fast"
assert_eq "$(rres path)/$(rres rehearsal_skipped)/$(rres vzdump_skipped)" "fast/yes/yes" "result: path=fast, rehearsal and vzdump skipped"
case "$(rres path_reasons)" in *"no migration and no deploy tooling change"*) case_ok "result: reasons recorded" ;; *) case_fail "result reasons [$(rres path_reasons)]" ;; esac
assert_eq "$(rres status)/$(rres published)" "done/yes" "result: done and published"
assert_eq "$(grep -c 'release path:' "${RDIR}/release-${RVER}.log")" "1" "the release log has exactly one path line"
assert_contains "${RDIR}/release-${RVER}.log" "release path: fast (rehearsal skip, vzdump skip): no migration and no deploy tooling change" "the path line names path and reasons"
assert_contains "$(journal)" "Path: \`fast\` (rehearsal skipped, vzdump skipped): no migration and no deploy tooling change" "journal: the path and the reasons"
assert_contains "$(journal)" "- rehearsal: skipped" "journal: the rehearsal phase says skipped"
assert_not_contains "$(journal)" "rehearsal #" "journal: no rehearsal entries"
teardown_case
case_end

case_begin "full path when a migration is new: rehearsal, token, vzdump kept"
setup_release fullmig
route_is full "migrations changed since the live release: crates/canterlot-hub/migrations/0099_x.sql"
cutover_plan "${FULL_PLAN[@]}"
run_release ship "${RVER}"
C="${FAKE_ROOT}/state/calls.log"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(cutover_calls | grep -c -- '--rehearse')" "1" "the rehearsal ran"
assert_contains "${C}" "--confirm " "the real run takes the token"
assert_not_contains "${C}" "--skip-ct-backup" "no vzdump skip"
assert_not_contains "${C}" "--fast" "no --fast"
assert_eq "$(rres path)/$(rres rehearsal_skipped)/$(rres vzdump_skipped)" "full/no/no" "result: path=full"
assert_contains "$(journal)" "Path: \`full\` (rehearsal run, vzdump run): migrations changed" "journal: path and reason"
teardown_case
case_end

case_begin "deploy tooling changed without a migration: the rehearsal runs, the vzdump is skipped"
setup_release rehearsed
route_is rehearsed "deploy tooling changed since the live release: scripts/deploy/lib.sh"
cutover_plan "${REHEARSED_PLAN[@]}"
run_release ship "${RVER}"
C="${FAKE_ROOT}/state/calls.log"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(cutover_calls | grep -c -- '--rehearse')" "1" "the rehearsal ran"
cutover_calls | grep -- '--rehearse' | grep -q -- '--skip-ct-backup' && case_ok "the rehearsal carries --skip-ct-backup" || case_fail "rehearsal without --skip-ct-backup"
cutover_calls | grep -- '--execute' | grep -q -- '--skip-ct-backup' && case_ok "the real run carries --skip-ct-backup" || case_fail "execute without --skip-ct-backup"
cutover_calls | grep -- '--execute' | grep -q -- '--confirm ' && case_ok "the real run takes the token" || case_fail "execute without a token"
assert_eq "$(cutover_calls | grep -c -- '--fast')" "0" "no --fast"
assert_eq "$(rres path)/$(rres rehearsal_skipped)/$(rres vzdump_skipped)" "rehearsed/no/yes" "result: path=rehearsed"
assert_contains "$(journal)" "Path: \`rehearsed\` (rehearsal run, vzdump skipped)" "journal: path"
assert_contains "$(journal)" "scripts/deploy/lib.sh" "journal: the tooling reason"
teardown_case
case_end

case_begin "--full forces the full path even when the decision says fast"
setup_release forcefull
route_is fast "no migration and no deploy tooling change"
cutover_plan "${FULL_PLAN[@]}"
run_release ship "${RVER}" --full
C="${FAKE_ROOT}/state/calls.log"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(cutover_calls | grep -c -- '--rehearse')" "1" "the rehearsal ran"
assert_not_contains "${C}" "--skip-ct-backup" "no vzdump skip"
assert_not_contains "${C}" "--fast" "no --fast"
assert_eq "$(rres path)/$(rres rehearsal_skipped)/$(rres vzdump_skipped)" "full/no/no" "result: path=full"
case "$(rres path_reasons)" in *"--full"*) case_ok "result: the reason is --full" ;; *) case_fail "reasons [$(rres path_reasons)]" ;; esac
assert_contains "${RDIR}/release-${RVER}.log" "release path: full (rehearsal run, vzdump run): --full" "the path line says --full"
teardown_case
case_end

case_begin "unclear conditions fail closed to the full path"
for why in fail garbage; do
    setup_release "closed-${why}"
    route_is fast "would be fast"
    : >"${FAKE_ROOT}/state/fastpath-${why}"
    cutover_plan "${FULL_PLAN[@]}"
    run_release ship "${RVER}"
    assert_eq "${RREL_RC}" "0" "${why}: exit 0"
    assert_eq "$(rres path)/$(rres rehearsal_skipped)/$(rres vzdump_skipped)" "full/no/no" "${why}: path=full"
    assert_eq "$(cutover_calls | grep -c -- '--rehearse')" "1" "${why}: the rehearsal ran"
    assert_not_contains "${FAKE_ROOT}/state/calls.log" "--fast" "${why}: no --fast"
    case "$(rres path_reasons)" in *"fastpath.sh"*) case_ok "${why}: the reason names fastpath.sh" ;; *) case_fail "${why}: reasons [$(rres path_reasons)]" ;; esac
    teardown_case
done
setup_release closed-inconsistent
printf 'path=fast\nrehearsal=run\nvzdump=skip\nreasons=odd\nvzdump_reason=x\n' >"${FAKE_ROOT}/state/fastpath-out"
cutover_plan "${FULL_PLAN[@]}"
run_release ship "${RVER}"
assert_eq "$(rres path)" "full" "an inconsistent decision is not trusted"
teardown_case
case_end

case_begin "the live release unknown or not an ancestor: full path (reasons from fastpath.sh are kept)"
setup_release notancestor
route_is full "the live release 0123456789ab is not an ancestor of the target"
cutover_plan "${FULL_PLAN[@]}"
run_release ship "${RVER}"
assert_eq "$(rres path)" "full" "path=full"
case "$(rres path_reasons)" in *"not an ancestor"*) case_ok "result: reason kept" ;; *) case_fail "reasons [$(rres path_reasons)]" ;; esac
assert_contains "$(journal)" "not an ancestor" "journal: reason kept"
teardown_case
case_end

case_begin "cutover refuses the fast path before any write: one fresh full rehearsal, then the token run"
setup_release fastrefused
route_is fast "would be fast"
cutover_plan "execute refused phase=token fail_codes=fast_path_refused" "rehearse pass" "execute done"
run_release ship "${RVER}"
C="${FAKE_ROOT}/state/calls.log"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(cutover_calls | wc -l | tr -d ' ')" "3" "fast attempt, rehearsal, token run"
cutover_calls | sed -n 1p | grep -q -- '--fast' && case_ok "first call: --fast" || case_fail "first call is not --fast"
cutover_calls | sed -n 3p | grep -q -- '--confirm ' && case_ok "last call: with the token" || case_fail "last call has no token"
cutover_calls | sed -n 3p | grep -q -- '--skip-ct-backup' && case_fail "the fallback skips the vzdump" || case_ok "the fallback keeps the vzdump"
assert_eq "$(rres path)/$(rres rehearsal_skipped)/$(rres vzdump_skipped)" "full/no/no" "result: path=full after the refusal"
case "$(rres path_reasons)" in *"refused the fast path"*) case_ok "result: the refusal is a reason" ;; *) case_fail "reasons [$(rres path_reasons)]" ;; esac
teardown_case
case_end

case_begin "cutover refuses to skip the vzdump in the rehearsal: the rehearsal is repeated on the full path"
setup_release rehearsed-refused
route_is rehearsed "tooling only"
cutover_plan "rehearse fail fail_codes=fast_path_refused" "rehearse pass" "execute done"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(cutover_calls | wc -l | tr -d ' ')" "3" "refused rehearsal, full rehearsal, token run"
cutover_calls | sed -n 2p | grep -q -- '--skip-ct-backup' && case_fail "the second rehearsal still skips the vzdump" || case_ok "the second rehearsal keeps the vzdump"
cutover_calls | sed -n 3p | grep -q -- '--skip-ct-backup' && case_fail "the real run skips the vzdump" || case_ok "the real run keeps the vzdump"
assert_eq "$(rres path)/$(rres vzdump_skipped)" "full/no" "result: path=full"
teardown_case
case_end

# --continue после сбоя на каждом пути: до боевого прогона путь пересчитывается,
# после начала боевого прогона он не повторяется никогда.
for kind in fast rehearsed full; do
    case_begin "--continue after a build failure on the ${kind} path"
    setup_release "cont-build-${kind}"
    route_is "${kind}" "reason for ${kind}"
    case "${kind}" in fast) cutover_plan "${FAST_PLAN[@]}" ;; rehearsed) cutover_plan "${REHEARSED_PLAN[@]}" ;; *) cutover_plan "${FULL_PLAN[@]}" ;; esac
    : >"${FAKE_ROOT}/state/build-fail"
    run_release ship "${RVER}"
    assert_eq "$(rres status)/$(rres phase)" "failed_before_cutover/build" "stopped at the build"
    rm -f "${FAKE_ROOT}/state/build-fail"
    : >"${FAKE_ROOT}/state/calls.log"
    run_release ship "${RVER}" --continue
    assert_eq "${RREL_RC}" "0" "continue: exit 0"
    assert_eq "$(rres path)" "${kind}" "continue: path ${kind}"
    assert_eq "$(cutover_calls | grep -c -- '--execute')" "1" "continue: one real run"
    if [ "${kind}" = fast ]; then
        assert_eq "$(cutover_calls | grep -c -- '--rehearse')" "0" "continue: still no rehearsal"
    else
        assert_eq "$(cutover_calls | grep -c -- '--rehearse')" "1" "continue: one fresh rehearsal"
    fi
    teardown_case
    case_end

    case_begin "--continue after a failed check-proxy on the ${kind} path never repeats the execute phase"
    setup_release "cont-proxy-${kind}"
    route_is "${kind}" "reason for ${kind}"
    case "${kind}" in fast) cutover_plan "${FAST_PLAN[@]}" ;; rehearsed) cutover_plan "${REHEARSED_PLAN[@]}" ;; *) cutover_plan "${FULL_PLAN[@]}" ;; esac
    : >"${FAKE_ROOT}/state/proxy-fail"
    run_release ship "${RVER}"
    assert_eq "$(rres status)" "stopped_before_publish" "stopped before the publication"
    assert_eq "$(rres path)" "${kind}" "result of the failed attempt: path ${kind}"
    rm -f "${FAKE_ROOT}/state/proxy-fail"
    : >"${FAKE_ROOT}/state/calls.log"
    # Подмена теперь решила бы иначе: путь после боевого прогона не пересчитывается.
    route_is full "the live release changed"
    run_release ship "${RVER}" --continue
    assert_eq "${RREL_RC}" "0" "continue: exit 0"
    assert_eq "$(calls_count 'cutover ')" "0" "continue: no cutover call at all"
    assert_eq "$(calls_count 'fastpath ')" "0" "continue: the path is not decided again"
    assert_eq "$(rres path)/$(rres published)" "${kind}/yes" "continue: path ${kind} kept, published"
    assert_contains "$(journal)" "Path: \`${kind}\`" "journal: path ${kind}"
    teardown_case
    case_end
done

case_begin "--continue after a STOP-AND-REPORT of a fast run never re-executes"
setup_release cont-stop-fast
route_is fast "reason"
cutover_plan "execute stop_and_report"
run_release ship "${RVER}"
assert_eq "$(rres status)/$(rres path)" "stop_and_report/fast" "stop_and_report on the fast path, path recorded"
: >"${FAKE_ROOT}/state/calls.log"
run_release ship "${RVER}" --continue
assert_eq "$(calls_count 'cutover ')" "0" "continue: no cutover call"
assert_eq "$(rres path)" "fast" "the refused continue still reports the path"
teardown_case
case_end

case_begin "--full given on --continue before the real run applies to the rerun"
setup_release cont-full
route_is fast "reason"
cutover_plan "${FULL_PLAN[@]}"
: >"${FAKE_ROOT}/state/build-fail"
run_release ship "${RVER}"
rm -f "${FAKE_ROOT}/state/build-fail"
run_release ship "${RVER}" --continue --full
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(rres path)" "full" "path=full on the continued run"
teardown_case
case_end

case_begin "the build phase warms the spec_compat test build, a failed warm-up never stops the release"
setup_release warm
cutover_plan "${FULL_PLAN[@]}"
run_release ship "${RVER}"
C="${FAKE_ROOT}/state/calls.log"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(grep -c '^cargo test --locked -p canterlot-worker --all-features --test it --no-run$' "${C}" || true)" "1" "one warm-up compile of the spec_compat test binary"
assert_not_contains "${C}" "spec_compat" "the check itself is not run by the warm-up (the preflight runs it)"
teardown_case
setup_release warm-failed
cutover_plan "${FULL_PLAN[@]}"
: >"${FAKE_ROOT}/state/warm-fail"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "a failing warm-up: exit 0"
assert_contains "${RDIR}/release-${RVER}.log" "warm-up of the spec_compat build failed" "the failure is logged"
teardown_case
case_end

printf '\nrelease-fast: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
