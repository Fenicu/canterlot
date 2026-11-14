#!/usr/bin/env bash
# scripts/release.sh: prepare (бамп во всех местах, запись CHANGES), предусловия ship,
# фазы, публикация только при done и нуле мягких ошибок, повтор репетиции только на
# переходных кодах, боевой прогон никогда не повторяется, --continue, журнал и итог
# на каждом пути. Большинство кейсов — с cutover.sh по сценарию; два — через
# настоящий cutover.sh на стенде cutover-fixture.
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
# cargo стенда cutover — функция; здесь нужен подменённый бинарь из fake-bin.
unset -f cargo

setup_release() { setup_case "release-$1"; make_release_repo; }
HAPPY_PLAN=("rehearse pass" "execute done")

# ---------------------------------------------------------------- prepare
case_begin "prepare bumps every version spot and commits with the CHANGES entry"
setup_release prepare
( cd "${RREPO}" && { printf 'История изменений\n=================\n\n'; changes_entry 1.2.0; changes_entry 1.1.0; changes_entry 1.0.0; } >CHANGES.rst )
ORIGIN_BEFORE="$("${REAL_GIT}" --git-dir="${FAKE_ROOT}/origin.git" rev-parse main)"
run_release prepare 1.2.0
assert_eq "${RREL_RC}" "0" "prepare exit 0"
grep -q '^version = "1.2.0"' "${RREPO}/Cargo.toml" && case_ok "Cargo.toml workspace version" || case_fail "Cargo.toml not bumped"
grep -q 'serde = { version = "1.0.0" }' "${RREPO}/Cargo.toml" && case_ok "dependency versions untouched" || case_fail "dependency version changed"
assert_eq "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "${RREPO}/web/package.json")" "1.2.0" "web/package.json version"
grep -q '"svelte": { "version": "5.0.0" }' "${RREPO}/web/package.json" && case_ok "nested package versions untouched" || case_fail "nested version changed"
assert_eq "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["info"]["version"])' "${RREPO}/docs/api/openapi.json")" "1.2.0" "openapi info.version"
grep -q 'D0%B8%D1%8F-1.2.0-00f4ee' "${RREPO}/README.md" && case_ok "README badge" || case_fail "README badge not bumped"
grep -q 'Текущая версия — 1.2.0\.' "${RREPO}/README.md" && case_ok "README «Текущая версия»" || case_fail "README line not bumped"
assert_contains "${FAKE_ROOT}/state/calls.log" "cargo update -w --offline" "cargo update -w --offline ran"
assert_eq "$(git -C "${RREPO}" log -1 --format=%s)" "Выпустить версию 1.2.0" "commit subject"
assert_eq "$(git -C "${RREPO}" log -1 --format=%B | grep -c 'Co-Authored' || true)" "0" "no trailers"
assert_eq "$(git -C "${RREPO}" show --name-only --format= HEAD | LC_ALL=C sort | paste -sd' ')" "CHANGES.rst Cargo.lock Cargo.toml README.md docs/api/openapi.json web/package.json" "commit contains every spot and CHANGES.rst"
assert_eq "$(git -C "${RREPO}" status --porcelain)" "" "tree clean after prepare"
assert_eq "$("${REAL_GIT}" --git-dir="${FAKE_ROOT}/origin.git" rev-parse main)" "${ORIGIN_BEFORE}" "nothing pushed"
assert_eq "$(calls_count 'git push')$(calls_count 'git tag')" "00" "no tag, no push"
teardown_case
case_end

case_begin "prepare refuses without the CHANGES entry"
setup_release prepare-nochanges
HEAD_BEFORE="$(git -C "${RREPO}" rev-parse HEAD)"
run_release prepare 1.2.0
assert_eq "${RREL_RC}" "2" "exit 2"
assert_contains "${FAKE_ROOT}/release.out" "CHANGES.rst has no entry for 1.2.0" "reason printed"
assert_eq "$(git -C "${RREPO}" rev-parse HEAD)" "${HEAD_BEFORE}" "no commit"
assert_eq "$(git -C "${RREPO}" status --porcelain)" "" "nothing bumped"
teardown_case
case_end

case_begin "prepare refuses a dirty tree besides CHANGES.rst"
setup_release prepare-dirty
( cd "${RREPO}" && { printf 'История изменений\n=================\n\n'; changes_entry 1.2.0; changes_entry 1.1.0; } >CHANGES.rst && printf 'x\n' >>README.md && : >stray.txt )
run_release prepare 1.2.0
assert_eq "${RREL_RC}" "2" "exit 2"
assert_contains "${FAKE_ROOT}/release.out" "M README.md" "the modified file named"
assert_contains "${FAKE_ROOT}/release.out" "?? stray.txt" "the untracked file named"
assert_not_contains "${FAKE_ROOT}/release.out" "M CHANGES.rst" "CHANGES.rst itself is allowed"
grep -q '^version = "1.1.0"' "${RREPO}/Cargo.toml" && case_ok "nothing bumped" || case_fail "bumped despite the refusal"
teardown_case
case_end

# ---------------------------------------------------------------- предусловия
# Отказ до старта: код 2, итог refused, ни состояния, ни тега, ни вызовов.
assert_refused() {
    assert_eq "${RREL_RC}" "2" "exit 2"
    assert_contains "${FAKE_ROOT}/release.out" "$1" "reason: $1"
    assert_eq "$(rres status)" "refused" "result file: refused"
    [ ! -e "${RDIR}/release-${RVER}.state" ] && case_ok "no state written" || case_fail "state written"
    assert_eq "$(grep -cvE '^$' "${FAKE_ROOT}/state/calls.log" || true)" "0" "no tag, push, build or cutover"
    [ ! -e "${RDIR}/pilot-deploy-log.md" ] && case_ok "no journal section" || case_fail "journal written"
}

case_begin "ship refuses a dirty tree"
setup_release dirty
printf 'x\n' >>"${RREPO}/README.md"
run_release ship "${RVER}"
assert_refused "the tree is dirty"
teardown_case
case_end

case_begin "ship refuses a branch other than main"
setup_release branch
git -C "${RREPO}" checkout -q -b feature
run_release ship "${RVER}"
assert_refused "not on main"
teardown_case
case_end

case_begin "ship refuses a linked worktree"
setup_release worktree
git -C "${RREPO}" worktree add -q "${FAKE_ROOT}/wt" -b wt-branch
cp -r "${RREPO}/.superpowers" "${FAKE_ROOT}/wt/"
RREL_RC=0
(cd "${FAKE_ROOT}/wt" && bash scripts/release.sh ship "${RVER}") >"${FAKE_ROOT}/release.out" 2>&1 || RREL_RC=$?
assert_eq "${RREL_RC}" "2" "exit 2"
assert_contains "${FAKE_ROOT}/release.out" "not from a linked worktree" "worktree refused"
teardown_case
case_end

case_begin "ship refuses a HEAD with another version"
setup_release version
run_release ship 1.2.0
assert_eq "${RREL_RC}" "2" "exit 2"
assert_contains "${FAKE_ROOT}/release.out" "HEAD is version 1.1.0, not 1.2.0" "version mismatch named"
assert_eq "$(sed -n 's/^status=//p' "${RDIR}/release-1.2.0.result")" "refused" "result file: refused"
teardown_case
case_end

case_begin "ship refuses without a gate attestation for HEAD"
setup_release noattest
OLD_SHA="$(git -C "${RREPO}" rev-parse HEAD~1)"
mv "${RREPO}/.superpowers/gate/$(git -C "${RREPO}" rev-parse HEAD).ok" "${RREPO}/.superpowers/gate/${OLD_SHA}.ok"
attest_head "${OLD_SHA}"
run_release ship "${RVER}"
assert_refused "no gate attestation for HEAD"
teardown_case
case_end

case_begin "ship refuses a foreign or failed attestation"
setup_release foreign
attest_head "$(git -C "${RREPO}" rev-parse HEAD)" "$(git -C "${RREPO}" rev-parse HEAD~1)"
run_release ship "${RVER}"
assert_refused "was written for another commit"
rm -f "${RDIR}/release-${RVER}.result"
ATTEST_WEB_EXIT=1 attest_head
run_release ship "${RVER}"
assert_refused "records web_exit=1"
teardown_case
case_end

case_begin "ship refuses the example site values (deploy and build origins), naming only the keys"
setup_release example
export SITE_ORIGIN=https://hub.example.com
run_release ship "${RVER}"
assert_refused "effective site values are the neutral example (SITE_ORIGIN)"
assert_not_contains "${FAKE_ROOT}/release.out" "hub.example.com" "the value itself is not printed"
release_site_env
rm -f "${RDIR}/release-${RVER}.result"
unset CANTERLOT_SITE_ORIGIN
run_release ship "${RVER}"
assert_refused "(CANTERLOT_SITE_ORIGIN)"
release_site_env
rm -f "${RDIR}/release-${RVER}.result"
export HUB_IP=203.0.113.7
run_release ship "${RVER}"
assert_refused "(HUB_IP)"
release_site_env
teardown_case
case_end

case_begin "ship refuses when the disk is short"
setup_release disk
RELEASE_MIN_FREE_MB=999999999 run_release ship "${RVER}"
assert_refused "MB free on the file system"
teardown_case
case_end

case_begin "a second concurrent ship is blocked by the release lock"
setup_release lock
mkdir -p "${RDIR}"
flock "${RDIR}/release.lock" sleep 30 &
LOCK_PID=$!
for _ in $(seq 1 50); do flock -n "${RDIR}/release.lock" true || break; command sleep 0.05; done
cutover_plan "${HAPPY_PLAN[@]}"
run_release ship "${RVER}"
kill "${LOCK_PID}" 2>/dev/null || true
wait "${LOCK_PID}" 2>/dev/null || true
assert_eq "${RREL_RC}" "3" "exit 3"
assert_contains "${FAKE_ROOT}/release.out" "another scripts/release.sh holds" "lock reported"
[ ! -e "${RDIR}/release-${RVER}.state" ] && case_ok "no state written" || case_fail "state written"
assert_eq "$(grep -c . "${FAKE_ROOT}/state/calls.log" || true)" "0" "nothing ran"
teardown_case
case_end

# ---------------------------------------------------------------- счастливый путь
case_begin "happy path: every phase, publication, journal and result done"
setup_release happy
cutover_plan "${HAPPY_PLAN[@]}"
SHA="$(git -C "${RREPO}" rev-parse HEAD)"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
C="${FAKE_ROOT}/state/calls.log"
in_order "${C}" "git tag -s v${RVER} -m Версия ${RVER}" "git push --atomic origin refs/heads/main:refs/heads/main refs/tags/v${RVER}:refs/tags/v${RVER}" \
    "build-release v${RVER} --local" "stage ${SHA:0:12} --source" "--rehearse" "--execute ${SHA:0:12} --confirm" \
    "check-proxy https://hub.canterlot.test" "publish-release v${RVER}" \
    && case_ok "phases in order: tag, push, build, stage, rehearsal, execute, check-proxy, publish" || case_fail "phases out of order"
TOKEN="$(sed -n 's/^token=//p' "${RDIR}/rehearsal-${RVER}-1.result")"
assert_contains "${C}" "--confirm ${TOKEN} " "execute uses the token from the rehearsal result file"
assert_not_contains "${C}" "--positive" "check-proxy only anonymous"
assert_eq "$(origin_tag)" "${SHA}" "origin has the tag at the release commit"
assert_eq "$("${REAL_GIT}" --git-dir="${FAKE_ROOT}/origin.git" rev-parse main)" "${SHA}" "origin main at the release commit"
for p in tag push build dist stage rehearsal execute proxy publish journal; do
    assert_eq "$(rstate "phase_${p}")" "done" "state: phase ${p} done"
done
assert_eq "$(rstate sha)" "${SHA}" "state: pinned sha"
assert_eq "$(rres status)/$(rres exit_code)/$(rres published)/$(rres proxy_check)" "done/0/yes/pass" "result: done, published, proxy pass"
assert_eq "$(rres cutover_status)/$(rres soft_failures)" "done/0" "result: cutover done, no soft failures"
assert_contains "${RDIR}/release-${RVER}.result" "pending=check-proxy --positive" "result: check-proxy --positive pending"
J="${RDIR}/pilot-deploy-log.md"
assert_contains "${J}" "## Update to ${RVER} (\`${SHA:0:12}\`) by scripts/release.sh: done" "journal section appended"
assert_contains "${J}" "scripts/check-proxy.sh https://hub.canterlot.test --positive" "journal: --positive pending for the owner/orchestrator"
assert_contains "${J}" "${RDIR}/cutover-${RVER}-1.log" "journal: cutover log path"
assert_contains "${J}" "restore points: dump" "journal: restore points"
assert_contains "${FAKE_ROOT}/release.out" "=== owner checklist" "owner checklist printed"
assert_contains "${FAKE_ROOT}/release.out" "release PASS" "PASS line"
assert_contains "${RDIR}/release-${RVER}.log" "release PASS" "progress log written"
teardown_case
case_end

case_begin "a second ship without --continue reports the saved state and runs nothing"
setup_release again
cutover_plan "${HAPPY_PLAN[@]}"
run_release ship "${RVER}"
: >"${FAKE_ROOT}/state/calls.log"
RESULT_BEFORE="$(cat "${RDIR}/release-${RVER}.result")"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "3" "exit 3"
assert_contains "${FAKE_ROOT}/release.out" "a release state exists" "state reported"
assert_contains "${FAKE_ROOT}/release.out" "phase_publish=done" "phases printed"
assert_eq "$(grep -c . "${FAKE_ROOT}/state/calls.log" || true)" "0" "nothing re-run"
assert_eq "$(cat "${RDIR}/release-${RVER}.result")" "${RESULT_BEFORE}" "result untouched"
teardown_case
case_end

# ---------------------------------------------------------------- правило публикации
case_begin "soft failures withhold the publication"
setup_release soft
cutover_plan "rehearse pass" "execute done soft_failures=2 soft_by_kind=pwa:1,verify:1"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "1" "exit 1"
assert_eq "$(calls_count publish-release)" "0" "publish-release never ran"
assert_eq "$(calls_count check-proxy)" "1" "check-proxy still ran"
assert_eq "$(rres status)/$(rres published)" "stopped_before_publish/no" "result: stopped before publish"
assert_contains "${RDIR}/release-${RVER}.result" "soft failures 2 (pwa:1,verify:1)" "reason names the soft failures"
assert_eq "$(rstate phase_publish)" "withheld" "state: publish withheld"
assert_contains "${RDIR}/pilot-deploy-log.md" "publication withheld" "journal: withheld"
teardown_case
case_end

case_begin "speech-only soft failures publish and are journaled as pending"
setup_release speech
cutover_plan "rehearse pass" "execute done speech_soft_failures=2"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(calls_count publish-release)" "1" "published"
assert_eq "$(rres status)/$(rres speech_soft_failures)" "done/2" "result: done with speech soft failures"
assert_contains "${RDIR}/release-${RVER}.result" "speech soft failures 2" "result: speech pending"
assert_contains "${RDIR}/pilot-deploy-log.md" "pending: speech soft failures 2" "journal: speech pending"
teardown_case
case_end

case_begin "a failed check-proxy withholds the publication"
setup_release proxyfail
cutover_plan "${HAPPY_PLAN[@]}"
: >"${FAKE_ROOT}/state/proxy-fail"
run_release ship "${RVER}"
assert_eq "$(calls_count publish-release)" "0" "publish-release never ran"
assert_eq "$(rres status)/$(rres proxy_check)" "stopped_before_publish/fail" "result: stopped, proxy fail"
teardown_case
case_end

case_begin "a failed publication is reported and resumed by --continue"
setup_release publishfail
cutover_plan "${HAPPY_PLAN[@]}"
: >"${FAKE_ROOT}/state/publish-fail"
run_release ship "${RVER}"
assert_eq "$(rres status)" "publish_failed" "result: publish_failed"
assert_contains "${RDIR}/pilot-deploy-log.md" "by scripts/release.sh: publish_failed" "journal: publish_failed section"
rm -f "${FAKE_ROOT}/state/publish-fail"
: >"${FAKE_ROOT}/state/calls.log"
run_release ship "${RVER}" --continue
assert_eq "${RREL_RC}" "0" "continue: exit 0"
assert_eq "$(calls_count 'cutover ')" "0" "continue: no rehearsal, no cutover"
assert_eq "$(calls_count check-proxy)" "0" "continue: check-proxy already done"
assert_eq "$(calls_count publish-release)" "1" "continue: publish re-run"
assert_eq "$(rres status)/$(rres published)" "done/yes" "result: done"
teardown_case
case_end

# ---------------------------------------------------------------- согласованность итога cutover
case_begin "a done result with a signal or a non-zero exit stops before the publication"
for plan in "execute done exit_code=143 signal=TERM" "execute done proc_exit=143" "execute done signal=TERM"; do
    setup_release "inconsistent-${plan// /-}"
    cutover_plan "rehearse pass" "${plan}"
    run_release ship "${RVER}"
    assert_eq "${RREL_RC}" "1" "[${plan}] exit 1"
    assert_eq "$(calls_count publish-release)" "0" "[${plan}] publish-release never ran"
    assert_eq "$(rres status)/$(rres published)" "stopped_before_publish/no" "[${plan}] result: stopped before publish"
    assert_contains "${RDIR}/release-${RVER}.result" "inconsistent" "[${plan}] reason: inconsistent cutover result"
    assert_eq "$(rstate phase_execute)" "inconsistent" "[${plan}] state: execute inconsistent"
    : >"${FAKE_ROOT}/state/calls.log"
    run_release ship "${RVER}" --continue
    assert_eq "${RREL_RC}" "1" "[${plan}] --continue: exit 1"
    assert_eq "$(calls_count publish-release)$(calls_count 'cutover ')" "00" "[${plan}] --continue: nothing published, nothing re-run"
    teardown_case
done
case_end

case_begin "the process exit of execute and rehearsal is persisted"
setup_release exits
cutover_plan "${HAPPY_PLAN[@]}"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(sed -n 's/^execute_exit=//p' "${RDIR}/release-${RVER}.state")/$(sed -n 's/^rehearsal_exit=//p' "${RDIR}/release-${RVER}.state")" "0/0" "state: execute_exit and rehearsal_exit"
teardown_case
case_end

case_begin "a pass rehearsal with a non-zero process exit gives no token to the real run"
setup_release rehearsal-exit
cutover_plan "rehearse pass proc_exit=143" "execute done"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "1" "exit 1"
assert_eq "$(calls_count '--execute')" "0" "no real cutover"
assert_eq "$(rres status)" "failed_before_cutover" "result: failed before the cutover"
teardown_case
case_end

# ---------------------------------------------------------------- повторы репетиции
case_begin "a transient rehearsal failure is retried once, then the release goes on"
setup_release transient-once
cutover_plan "rehearse fail fail_codes=session_starting" "rehearse pass" "execute done"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(calls_count '--rehearse')" "2" "two complete rehearsals"
assert_contains "${FAKE_ROOT}/release.out" "rehearsal failed only with transient codes (session_starting): one fresh rehearsal in 0 s" "retry announced"
assert_eq "$(rres status)/$(rres rehearsal_retried)" "done/yes" "result: done after a retry"
assert_contains "${RDIR}/pilot-deploy-log.md" "repeated once after transient failure codes \`session_starting\`" "journal: the retry is a deviation"
teardown_case
case_end

case_begin "two transient failures stop the release"
setup_release transient-twice
cutover_plan "rehearse fail fail_codes=vzdump_busy" "rehearse fail fail_codes=session_starting,vzdump_busy"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "1" "exit 1"
assert_eq "$(calls_count '--rehearse')" "2" "exactly two rehearsals"
assert_eq "$(calls_count '--execute')" "0" "no real cutover"
assert_eq "$(rres status)/$(rres rehearsal_fail_codes)" "failed_before_cutover/session_starting,vzdump_busy" "result: stopped with the codes"
assert_contains "${RDIR}/release-${RVER}.result" "(after one retry)" "reason says it was retried"
teardown_case
case_end

case_begin "a non-transient rehearsal failure stops at once"
for plan in "rehearse fail fail_codes=session_starting,unknown" "rehearse fail fail_codes=hub_readyz" "rehearse fail fail_codes=" \
        "rehearse fail fail_codes=vzdump_busy signal=TERM exit_code=143" "rehearse truncated fail_codes=vzdump_busy" "rehearse none" \
        "rehearse pass soft_failures=1 fail_codes=unknown"; do
    setup_release nontransient
    cutover_plan "${plan}"
    run_release ship "${RVER}"
    assert_eq "$(calls_count '--rehearse')/$(rres status)" "1/failed_before_cutover" "one rehearsal, then STOP: ${plan}"
    teardown_case
done
case_end

# ---------------------------------------------------------------- боевой прогон
case_begin "a failed real cutover is never retried, not even by --continue"
setup_release execfail
cutover_plan "rehearse pass" "execute failed_before_flip recovery=worker_restarted fail_codes=unknown vzdump_done=yes worker_stopped=yes" "rehearse pass" "execute done"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "1" "exit 1"
assert_eq "$(calls_count '--execute')" "1" "one real cutover"
assert_eq "$(calls_count publish-release)" "0" "nothing published"
assert_eq "$(rres status)" "failed_before_flip" "result: failed_before_flip"
assert_contains "${RDIR}/pilot-deploy-log.md" "recovery before the hub flip: worker_restarted" "journal: recovery"
: >"${FAKE_ROOT}/state/calls.log"
run_release ship "${RVER}" --continue
assert_eq "${RREL_RC}" "1" "continue: exit 1"
assert_eq "$(grep -c . "${FAKE_ROOT}/state/calls.log" || true)" "0" "continue: nothing re-run"
assert_contains "${FAKE_ROOT}/release.out" "it is never re-run" "continue: explains"
teardown_case
case_end

case_begin "STOP-AND-REPORT: nothing published, the STOP block is referenced"
setup_release stop
cutover_plan "rehearse pass" "execute stop_and_report"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "1" "exit 1"
assert_eq "$(calls_count publish-release)/$(calls_count check-proxy)" "0/0" "no proxy check, no publish"
STOPB="${RDIR}/cutover-${RVER}-1.stop.txt"
assert_eq "$(rres status)/$(rres stop_block)" "stop_and_report/${STOPB}" "result: stop_and_report with the block path"
assert_contains "${RDIR}/release-${RVER}.result" "pending=STOP-AND-REPORT" "result: STOP pending first"
assert_contains "${RDIR}/pilot-deploy-log.md" "STOP block: \`${STOPB}\`" "journal: the STOP block"
assert_contains "${FAKE_ROOT}/release.out" "STOP block: ${STOPB}" "STOP block path printed"
: >"${FAKE_ROOT}/state/calls.log"
run_release ship "${RVER}" --continue
assert_contains "${FAKE_ROOT}/release.out" "STOP block: ${STOPB}" "continue prints the STOP block path"
assert_eq "$(grep -c . "${FAKE_ROOT}/state/calls.log" || true)" "0" "continue: nothing re-run"
teardown_case
case_end

case_begin "--continue after a build failure resumes at the build"
setup_release build
cutover_plan "${HAPPY_PLAN[@]}"
: >"${FAKE_ROOT}/state/build-fail"
run_release ship "${RVER}"
assert_eq "$(rres status)/$(rres phase)" "failed_before_cutover/build" "result: stopped at build"
assert_eq "$(rstate phase_tag)/$(rstate phase_push)/$(rstate phase_build)" "done/done/" "state: tag and push done, build not"
assert_contains "${RDIR}/pilot-deploy-log.md" "by scripts/release.sh: failed_before_cutover" "journal: failure before the cutover"
assert_contains "${RDIR}/pilot-deploy-log.md" "${RDIR}/build-${RVER}.log" "journal: build log path"
rm -f "${FAKE_ROOT}/state/build-fail"
: >"${FAKE_ROOT}/state/calls.log"
run_release ship "${RVER}" --continue
assert_eq "${RREL_RC}" "0" "continue: exit 0"
assert_eq "$(calls_count 'git tag')$(calls_count 'git push')" "00" "continue: tag and push not repeated"
assert_contains "${FAKE_ROOT}/release.out" "phase tag: already done" "continue: tag skipped"
assert_eq "$(calls_count build-release)" "1" "continue: build ran"
assert_eq "$(rres status)/$(rres continued)" "done/yes" "result: done on --continue"
teardown_case
case_end

case_begin "--continue re-detects a tag and push already done at this SHA"
setup_release retag
cutover_plan "${HAPPY_PLAN[@]}"
git -C "${RREPO}" tag -a "v${RVER}" -m x
git -C "${RREPO}" push -q --atomic origin main "v${RVER}"
: >"${FAKE_ROOT}/state/calls.log"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_contains "${FAKE_ROOT}/release.out" "tag v${RVER} already at" "tag detected"
assert_contains "${FAKE_ROOT}/release.out" "origin already has main and v${RVER}" "push detected"
teardown_case
case_end

case_begin "--continue after the real cutover started never re-executes"
setup_release execstarted
cutover_plan "rehearse pass" "execute none" "rehearse pass" "execute done"
run_release ship "${RVER}"
assert_eq "$(rres status)" "cutover_unknown" "result: cutover_unknown without a result file"
assert_eq "$(rstate phase_execute)" "unknown" "state: execute unknown"
: >"${FAKE_ROOT}/state/calls.log"
run_release ship "${RVER}" --continue
assert_eq "${RREL_RC}" "1" "continue: exit 1"
assert_eq "$(calls_count 'cutover ')" "0" "continue: no rehearsal, no cutover"
assert_contains "${FAKE_ROOT}/release.out" "it is never re-run" "continue: refuses to replay"
teardown_case
case_end

case_begin "a token rejected before any write gets one fresh rehearsal"
setup_release token
cutover_plan "rehearse pass" "execute refused phase=token fail_codes=token_expired" "rehearse pass" "execute done"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
assert_eq "$(calls_count '--rehearse')/$(calls_count '--execute')" "2/2" "fresh rehearsal, then the real run again"
T2="$(sed -n 's/^token=//p' "${RDIR}/rehearsal-${RVER}-2.result")"
assert_contains "${FAKE_ROOT}/state/calls.log" "--confirm ${T2} " "the second run uses the fresh token"
assert_eq "$(rres status)" "done" "result: done"
teardown_case
case_end

case_begin "a refusal that is not a pre-write token rejection is not retried"
for plan in "execute refused phase=token fail_codes=token_invalid" "execute refused phase=preflight fail_codes=vzdump_busy" \
        "execute refused phase=token fail_codes=token_expired worker_stopped=yes"; do
    setup_release token-no
    cutover_plan "rehearse pass" "${plan}" "rehearse pass" "execute done"
    run_release ship "${RVER}"
    assert_eq "$(calls_count '--execute')/$(rres status)" "1/failed_before_cutover" "one real run, STOP: ${plan}"
    teardown_case
done
case_end

# ---------------------------------------------------------------- настоящий cutover.sh
# release.sh в своём чекауте, cutover.sh — обёртка над настоящим: релиз подменяется
# релизом стенда (SHA_NEW), --source и --dist — стендовыми, значения стенда — свои.
wire_real_cutover() {
    local saved
    saved="$(declare -p SITE_ORIGIN WSS_ORIGIN SITE_CODES HUB_IP | sed 's/^declare -x /export /;s/^declare -- /export /')"
    printf '%s\n' "${saved}" >"${FAKE_ROOT}/state/fixture.env"
    make_release_repo
    cat >"${RREPO}/scripts/deploy/cutover.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
source "${FAKE_ROOT}/state/fixture.env"
echo "cutover \$*" >>"${FAKE_ROOT}/state/calls.log"
n="\$(cat "${FAKE_ROOT}/state/rehearse-n" 2>/dev/null || echo 0)"
args=()
while ((\$#)); do
    case "\$1" in
        --source) args+=(--source "${REPO_ROOT}"); shift 2 ;;
        --dist) args+=(--dist "${FAKE_ROOT}/dist"); shift 2 ;;
        --execute) args+=(--execute "${SHA_NEW}"); shift 2 ;;
        --rehearse) args+=(--rehearse --state "${FAKE_ROOT}/rehearse.state"); n=\$((n + 1)); echo "\${n}" >"${FAKE_ROOT}/state/rehearse-n"; shift ;;
        --confirm | --result) args+=("\$1" "\$2"); shift 2 ;;
        *) args+=("${SHA_NEW}"); shift ;;
    esac
done
# Блокировка CT только у первых LOCKED_REHEARSALS репетиций (плановый бэкап закончился).
if [ "\${n}" -le "\$(cat "${FAKE_ROOT}/state/locked-rehearsals" 2>/dev/null || echo 0)" ] && [[ " \${args[*]} " == *" --rehearse "* ]]; then
    export FAKE_CT_LOCK=1
fi
exec bash "${DEPLOY_DIR}/cutover.sh" "\${args[@]}"
EOF
    chmod +x "${RREPO}/scripts/deploy/cutover.sh"
    git -C "${RREPO}" commit -qam "Обёртка cutover.sh стенда"
    attest_head
}

case_begin "happy path through the real cutover fixture"
setup_cutover release-real
wire_real_cutover
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
[ "${RREL_RC}" = "0" ] || tail -20 "${FAKE_ROOT}/release.out"
assert_eq "$(sed -n 's/^status=//p' "${RDIR}/rehearsal-${RVER}-1.result")" "pass" "real rehearsal result: pass"
assert_eq "$(sed -n 's/^status=//p' "${RDIR}/cutover-${RVER}-1.result")" "done" "real cutover result: done"
hub_is "${SHA_NEW}" && worker_is "${SHA_NEW}" && web_is "${SHA_NEW}" && case_ok "hub, worker and PWA on the new release" || case_fail "not all on new"
assert_eq "$(rres status)/$(rres published)" "done/yes" "result: done, published"
assert_contains "${FAKE_ROOT}/release.out" "=== owner checklist: authenticated checks" "the cutover's owner checklist printed"
teardown_case
case_end

case_begin "a CT backup lock in the real rehearsal is retried once, then the real cutover runs"
setup_cutover release-real-lock
wire_real_cutover
echo 1 >"${FAKE_ROOT}/state/locked-rehearsals"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "0" "exit 0"
[ "${RREL_RC}" = "0" ] || tail -20 "${FAKE_ROOT}/release.out"
assert_eq "$(sed -n 's/^fail_codes=//p' "${RDIR}/rehearsal-${RVER}-1.result")" "vzdump_busy" "first real rehearsal: vzdump_busy"
assert_eq "$(sed -n 's/^status=//p' "${RDIR}/rehearsal-${RVER}-2.result")" "pass" "second real rehearsal: pass"
assert_eq "$(rres status)/$(rres rehearsal_retried)" "done/yes" "result: done after one retry"
hub_is "${SHA_NEW}" && case_ok "hub on new" || case_fail "hub not on new"
teardown_case
case_end

case_begin "a CT backup lock in both real rehearsals stops before the cutover"
setup_cutover release-real-lock2
wire_real_cutover
echo 2 >"${FAKE_ROOT}/state/locked-rehearsals"
run_release ship "${RVER}"
assert_eq "${RREL_RC}" "1" "exit 1"
assert_eq "$(rres status)" "failed_before_cutover" "result: failed before the cutover"
assert_eq "$(calls_count '--execute')" "0" "no real cutover"
hub_is "${SHA_PREV}" && worker_is "${SHA_PREV}" && case_ok "production untouched" || case_fail "production moved"
teardown_case
case_end

case_begin "a crashed child after a transient code in the real rehearsal is not retried"
setup_cutover release-real-crash
wire_real_cutover
EXEC3="$(new_exec_id)"
spawn_session "${EXEC3}" "${SHA_PREV}" 300
printf '%s starting\n' "${EXEC3}" >"${FAKE_ROOT}/state/exec-states"
install_exe_readlink_fault
FAKE_READLINK_FAIL_EXE=1 run_release ship "${RVER}"
assert_eq "${RREL_RC}" "1" "exit 1"
CODES="$(sed -n 's/^fail_codes=//p' "${RDIR}/rehearsal-${RVER}-1.result")"
[[ ",${CODES}," == *",unknown,"* && ",${CODES}," == *",session_starting,"* ]] \
    && case_ok "real rehearsal codes: session_starting and unknown (${CODES})" || case_fail "real rehearsal codes [${CODES}]"
[ ! -e "${RDIR}/rehearsal-${RVER}-2.result" ] && case_ok "no second rehearsal" || case_fail "the rehearsal was retried"
assert_not_contains "${FAKE_ROOT}/release.out" "one fresh rehearsal" "no retry announced"
assert_eq "$(rres status)/$(rres rehearsal_retried)" "failed_before_cutover/no" "result: stopped, not retried"
assert_eq "$(calls_count '--execute')" "0" "no real cutover"
teardown_case
case_end

printf 'release harness: %s passed, %s failed (cumulative)\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
