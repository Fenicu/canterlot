#!/usr/bin/env bash
# Быстрый путь выпуска без миграций: решение fastpath_eval/fastpath.sh по git diff
# между развёрнутым релизом и целью. Любая неясность — полный путь (репетиция и vzdump).
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

# eval_fp SRC TARGET HUB WEB WORKER -> вывод fastpath_eval (ключ=значение)
eval_fp() {
    bash -c 'source "$1/lib.sh"; shift; fastpath_eval "$@"' _ "${DEPLOY_DIR}" "$@"
}
fp_get() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1; }

# expect_eval ИМЯ ПУТЬ РЕПЕТИЦИЯ VZDUMP ОБРАЗЕЦ_ПРИЧИНЫ — изменения цели в "$@" после --
check_route() {
    local name="$1" want_path="$2" want_reh="$3" want_vz="$4" want_reason="$5" out
    shift 5
    case_begin "fastpath_eval: ${name}"
    make_fast_source "$@"
    out="$(eval_fp "${FAST_SRC}" "${SHA_NEW}" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}")"
    assert_eq "$(fp_get "${out}" path)" "${want_path}" "path"
    assert_eq "$(fp_get "${out}" rehearsal)" "${want_reh}" "rehearsal"
    assert_eq "$(fp_get "${out}" vzdump)" "${want_vz}" "vzdump"
    case "$(fp_get "${out}" reasons)" in *"${want_reason}"*) case_ok "reason mentions: ${want_reason}" ;; *) case_fail "reasons [$(fp_get "${out}" reasons)] lack [${want_reason}]" ;; esac
    drop_fast_source
    case_end
}

check_route "no migration and no tooling change: fast" fast skip skip "no migration" other
check_route "a new migration: full" full run run "crates/canterlot-hub/migrations/9999_fixture.sql" migration
check_route "tooling under scripts/deploy changed: rehearsed, no vzdump" rehearsed run skip "scripts/deploy/lib.sh" tooling
check_route "release.sh changed" rehearsed run skip "scripts/release.sh" release-sh
check_route "build-release.sh changed" rehearsed run skip "scripts/build-release.sh" build-sh
check_route "publish-release.sh changed" rehearsed run skip "scripts/publish-release.sh" publish-sh
check_route "migration and tooling together: full" full run run "migrations" migration tooling

case_begin "fastpath_eval: a changed existing migration is a migration change"
make_fast_source
G=(git -C "${FAST_SRC}" -c user.name=F -c user.email=f@canterlot.invalid -c commit.gpgsign=false)
FIRST="$(find "${FAST_SRC}/crates/canterlot-hub/migrations" -name '*.sql' | sort | head -1)"
printf -- '-- edited\n' >>"${FIRST}"
"${G[@]}" commit -q -am edited
SHA_NEW="$("${G[@]}" rev-parse --short=12 HEAD)"
OUT="$(eval_fp "${FAST_SRC}" "${SHA_NEW}" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "an edited migration forces the full path"
git -C "${FAST_SRC}" rm -q -f "${FIRST}"
"${G[@]}" commit -q -m deleted
SHA_NEW="$("${G[@]}" rev-parse --short=12 HEAD)"
OUT="$(eval_fp "${FAST_SRC}" "${SHA_NEW}" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "a deleted migration forces the full path"
drop_fast_source
case_end

case_begin "fastpath_eval: unclear conditions fail closed"
make_fast_source other
OUT="$(eval_fp "${FAST_SRC}" "${SHA_NEW}" "" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "an empty live hub release"
case "$(fp_get "${OUT}" reasons)" in *"cannot be determined"*) case_ok "reason: live release unknown" ;; *) case_fail "reason: $(fp_get "${OUT}" reasons)" ;; esac
OUT="$(eval_fp "${FAST_SRC}" "${SHA_NEW}" "${SHA_PREV}" "${SHA_PREV}" "UNREADABLE")"
assert_eq "$(fp_get "${OUT}" path)" "full" "an unreadable worker release"
OUT="$(eval_fp "${FAST_SRC}" "${SHA_NEW}" "${SHA_PREV}" "${SHA_NEW}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "components on different releases"
OUT="$(eval_fp "${FAST_SRC}" "${SHA_NEW}" "cccccccccccc" "cccccccccccc" "cccccccccccc")"
assert_eq "$(fp_get "${OUT}" path)" "full" "live release unknown to the source repository"
OUT="$(eval_fp "${FAST_SRC}" "${SHA_PREV}" "${SHA_NEW}" "${SHA_NEW}" "${SHA_NEW}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "live release is not an ancestor of the target (downgrade)"
case "$(fp_get "${OUT}" reasons)" in *"not an ancestor"*) case_ok "reason: not an ancestor" ;; *) case_fail "reason: $(fp_get "${OUT}" reasons)" ;; esac
OUT="$(eval_fp "${FAST_SRC}" "${SHA_NEW}" "${SHA_NEW}" "${SHA_NEW}" "${SHA_NEW}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "target equals the live release"
OUT="$(eval_fp "${FAKE_ROOT:-/nonexistent}/no-such-repo" "${SHA_NEW}" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "no git repository at --source"
OUT="$(eval_fp "${FAST_SRC}" "not-a-sha" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "target is not a revision"
drop_fast_source
case_end

case_begin "fastpath.sh reads the live release from the hosts and prints the decision"
make_fast_source other
setup_cutover fastpath-cli
OUT="$(bash "${DEPLOY_DIR}/fastpath.sh" "${SHA_NEW}" --source "${FAST_SRC}")"
assert_eq "$(fp_get "${OUT}" path)" "fast" "path fast"
assert_eq "$(fp_get "${OUT}" live)" "${SHA_PREV}" "the live release is reported"
assert_eq "$(fp_get "${OUT}" target)" "${SHA_NEW}" "the target is reported"
rm -f "${HUB_CURRENT}"  # lint: local-temp
OUT="$(bash "${DEPLOY_DIR}/fastpath.sh" "${SHA_NEW}" --source "${FAST_SRC}" 2>/dev/null)" || true
assert_eq "$(fp_get "${OUT}" path)" "full" "a broken hub link: fail closed to the full path"
case "$(fp_get "${OUT}" reasons)" in *"cannot be determined"*) case_ok "reason: live release unknown" ;; *) case_fail "reason: $(fp_get "${OUT}" reasons)" ;; esac
drop_fast_source
teardown_case
case_end

printf '\nfastpath: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
