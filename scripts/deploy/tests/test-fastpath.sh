#!/usr/bin/env bash
# Быстрый путь выпуска: решение fastpath_eval/fastpath.sh по точным хэшам коммитов и
# git diff между развёрнутым релизом и целью. Быстрым путь бывает, только если каждый
# изменённый файл входит в явный список прикладного кода; любая неясность — полный путь.
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

# eval_fp SRC TARGET_FULL LIVE_FULL HUB WEB WORKER -> вывод fastpath_eval (ключ=значение)
eval_fp() {
    bash -c 'source "$1/lib.sh"; shift; fastpath_eval "$@"' _ "${DEPLOY_DIR}" "$@"
}
# Решение по стенду make_fast_source (все три компонента на развёрнутом релизе).
eval_std() { eval_fp "${FAST_SRC}" "${FAST_TARGET_FULL}" "${FAST_LIVE_FULL}" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}"; }
fp_get() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1; }

# check_route ИМЯ ПУТЬ РЕПЕТИЦИЯ VZDUMP ОБРАЗЕЦ_ПРИЧИНЫ ИЗМЕНЕНИЯ...
check_route() {
    local name="$1" want_path="$2" want_reh="$3" want_vz="$4" want_reason="$5" out
    shift 5
    case_begin "fastpath_eval: ${name}"
    make_fast_source "$@"
    out="$(eval_std)"
    assert_eq "$(fp_get "${out}" path)" "${want_path}" "path"
    assert_eq "$(fp_get "${out}" rehearsal)" "${want_reh}" "rehearsal"
    assert_eq "$(fp_get "${out}" vzdump)" "${want_vz}" "vzdump"
    case "$(fp_get "${out}" reasons)" in *"${want_reason}"*) case_ok "reason mentions: ${want_reason}" ;; *) case_fail "reasons [$(fp_get "${out}" reasons)] lack [${want_reason}]" ;; esac
    drop_fast_source
    case_end
}

# --- список прикладного кода: быстрый путь
check_route "only CHANGES.rst: fast" fast skip skip "allowlisted" other
check_route "a crate source file: fast" fast skip skip "allowlisted" crate-src
check_route "docs and web sources: fast" fast skip skip "allowlisted" docs web-src
check_route "a version-only bump of Cargo.toml, Cargo.lock and package.json: fast" fast skip skip "allowlisted" version-bump
# --- вне списка: репетиция нужна, vzdump по git пропустим (решает ещё манифест в cutover)
check_route "release.sh changed" rehearsed run skip "scripts/release.sh" release-sh
check_route "build-release.sh changed" rehearsed run skip "scripts/build-release.sh" build-sh
check_route "publish-release.sh changed" rehearsed run skip "scripts/publish-release.sh" publish-sh
check_route "scripts/deploy changed" rehearsed run skip "scripts/deploy/lib.sh" tooling
check_route "any other script changed" rehearsed run skip "scripts/extra.sh" script
check_route "the release-build Dockerfile changed" rehearsed run skip "docker/release-build/Dockerfile" docker
check_route "rust-toolchain.toml changed" rehearsed run skip "rust-toolchain.toml" toolchain
check_route "a build.rs appeared" rehearsed run skip "crates/canterlot-hub/build.rs" buildrs
check_route "a dependency in Cargo.lock changed" rehearsed run skip "Cargo.lock" lock-dep
check_route "a dependency in Cargo.toml changed" rehearsed run skip "Cargo.toml" toml-dep
check_route "a dependency in package.json changed" rehearsed run skip "web/package.json" pkg-dep
# --- загрузка миграций: полный путь
check_route "a new migration: full" full run run "crates/canterlot-hub/migrations/9999_fixture.sql" migration
check_route "a migration with a non-ASCII name (git would quote it): full" full run run "0052_" unicode-migration
check_route "a file renamed into the migration directory: full" full run run "migrations/9998_moved.sql" rename-in
check_route "a migration renamed out of the directory: full" full run run "migrations/00" rename-out
check_route "the hub's db.rs (migrator) changed: full" full run run "crates/canterlot-hub/src/db.rs" dbrs
check_route "the hub's main.rs (startup migration call) changed: full" full run run "crates/canterlot-hub/src/main.rs" mainrs
check_route "migration and tooling together: full" full run run "migrations" migration tooling

case_begin "fastpath_eval: an existing migration edited or deleted is a migration change"
make_fast_source
G=(git -C "${FAST_SRC}" -c user.name=F -c user.email=f@canterlot.invalid -c commit.gpgsign=false)
FIRST="$(find "${FAST_SRC}/crates/canterlot-hub/migrations" -name '*.sql' | sort | head -1)"
printf -- '-- edited\n' >>"${FIRST}"
"${G[@]}" commit -q -am edited
FAST_TARGET_FULL="$("${G[@]}" rev-parse HEAD)"
assert_eq "$(fp_get "$(eval_std)" path)" "full" "an edited migration forces the full path"
git -C "${FAST_SRC}" rm -q -f "${FIRST}"
"${G[@]}" commit -q -m deleted
FAST_TARGET_FULL="$("${G[@]}" rev-parse HEAD)"
assert_eq "$(fp_get "$(eval_std)" path)" "full" "a deleted migration forces the full path"
drop_fast_source
case_end

case_begin "fastpath_eval: unclear conditions fail closed"
make_fast_source other
OUT="$(eval_fp "${FAST_SRC}" "${FAST_TARGET_FULL}" "${FAST_LIVE_FULL}" "" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "an empty live hub release"
case "$(fp_get "${OUT}" reasons)" in *"cannot be determined"*) case_ok "reason: live release unknown" ;; *) case_fail "reason: $(fp_get "${OUT}" reasons)" ;; esac
OUT="$(eval_fp "${FAST_SRC}" "${FAST_TARGET_FULL}" "${FAST_LIVE_FULL}" "${SHA_PREV}" "${SHA_PREV}" "UNREADABLE")"
assert_eq "$(fp_get "${OUT}" path)" "full" "an unreadable worker release"
OUT="$(eval_fp "${FAST_SRC}" "${FAST_TARGET_FULL}" "${FAST_LIVE_FULL}" "${SHA_PREV}" "${SHA_NEW}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "components on different releases"
OUT="$(eval_fp "${FAST_SRC}" "${FAST_TARGET_FULL}" "cccccccccccccccccccccccccccccccccccccccc" "cccccccccccc" "cccccccccccc" "cccccccccccc")"
assert_eq "$(fp_get "${OUT}" path)" "full" "live release unknown to the source repository"
OUT="$(eval_fp "${FAST_SRC}" "${FAST_LIVE_FULL}" "${FAST_TARGET_FULL}" "${SHA_NEW}" "${SHA_NEW}" "${SHA_NEW}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "live release is not an ancestor of the target (downgrade)"
case "$(fp_get "${OUT}" reasons)" in *"not an ancestor"*) case_ok "reason: not an ancestor" ;; *) case_fail "reason: $(fp_get "${OUT}" reasons)" ;; esac
OUT="$(eval_fp "${FAST_SRC}" "${FAST_TARGET_FULL}" "${FAST_TARGET_FULL}" "${SHA_NEW}" "${SHA_NEW}" "${SHA_NEW}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "target equals the live release"
OUT="$(eval_fp "/nonexistent/no-such-repo" "${FAST_TARGET_FULL}" "${FAST_LIVE_FULL}" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "no git repository at --source"
OUT="$(eval_fp "${FAST_SRC}" "not-a-sha" "${FAST_LIVE_FULL}" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "target is not a full commit id"
OUT="$(eval_fp "${FAST_SRC}" "${FAST_TARGET_FULL:0:12}" "${FAST_LIVE_FULL}" "${SHA_PREV}" "${SHA_PREV}" "${SHA_PREV}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "a 12-character target is not an exact identity"
OUT="$(eval_fp "${FAST_SRC}" "${FAST_TARGET_FULL}" "${FAST_LIVE_FULL}" "${SHA_NEW}" "${SHA_NEW}" "${SHA_NEW}")"
assert_eq "$(fp_get "${OUT}" path)" "full" "the 12-character live ids do not match the full live id"
drop_fast_source
case_end

case_begin "fastpath_eval: refs named like the target and replacement objects cannot change what is diffed"
make_fast_source migration
# Ветка с 12-значным именем цели, указывающая на чистого предка: короткое имя резолвилось бы в неё.
git -C "${FAST_SRC}" branch "${FAST_TARGET_FULL:0:12}" "${FAST_LIVE_FULL}"
git -C "${FAST_SRC}" tag "${FAST_TARGET_FULL:0:12}x" "${FAST_LIVE_FULL}"
assert_eq "$(fp_get "$(eval_std)" path)" "full" "a hex-named branch does not hide the migration"
# Объект-замена: подмена цели на предка. Любая ref замены — отказ.
git -C "${FAST_SRC}" replace "${FAST_TARGET_FULL}" "${FAST_LIVE_FULL}"
OUT="$(eval_std)"
assert_eq "$(fp_get "${OUT}" path)" "full" "a replacement ref forces the full path"
case "$(fp_get "${OUT}" reasons)" in *"replace"*) case_ok "reason names the replacement refs" ;; *) case_fail "reason: $(fp_get "${OUT}" reasons)" ;; esac
drop_fast_source
case_end

case_begin "fastpath_eval: a git failure or a contradictory classifier is the full path"
make_fast_source other
STUB="$(mktemp /tmp/canterlot-deploy-stub.XXXXXX)"
printf '#!/usr/bin/env bash\nexit 3\n' >"${STUB}"
chmod +x "${STUB}"
OUT="$(FASTPATH_CLASSIFIER="${STUB}" eval_std)"
assert_eq "$(fp_get "${OUT}" path)" "full" "a failing classifier"
printf '#!/usr/bin/env bash\necho "ancestor=yes"\necho "ancestor=no"\n' >"${STUB}"
OUT="$(FASTPATH_CLASSIFIER="${STUB}" eval_std)"
assert_eq "$(fp_get "${OUT}" path)" "full" "a contradictory classifier (ancestor twice)"
printf '#!/usr/bin/env bash\necho "ancestor=yes"\n' >"${STUB}"
OUT="$(FASTPATH_CLASSIFIER="${STUB}" eval_std)"
assert_eq "$(fp_get "${OUT}" path)" "full" "an incomplete classifier"
printf '#!/usr/bin/env bash\necho "ancestor=yes"\necho "migration_inputs="\necho "other="\necho "changed=1"\n' >"${STUB}"
OUT="$(FASTPATH_CLASSIFIER="${STUB}" eval_std)"
assert_eq "$(fp_get "${OUT}" path)" "fast" "a complete classifier is trusted (control)"
rm -f "${STUB}"  # lint: local-temp
rm -rf "${FAST_SRC}/.git/objects/${FAST_TARGET_FULL:0:2}"  # lint: local-temp
OUT="$(eval_std)"
assert_eq "$(fp_get "${OUT}" path)" "full" "a missing object (git cannot diff) is the full path"
drop_fast_source
case_end

# --- fastpath.sh: чтение развёрнутых релизов с хостов, точные хэши из RELEASE
setup_fast_cli() {
    make_fast_source "$@"
    setup_cutover "$1"
    # Развёрнутый релиз хранит полный хэш исходника, как настоящая сборка.
    printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${HUB_RELEASES}/${SHA_PREV}/RELEASE"
    printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${WORKER_RELEASES}/${SHA_PREV}/RELEASE"
}
fastpath_cli() { bash "${DEPLOY_DIR}/fastpath.sh" "${SHA_NEW}" --source "${FAST_SRC}" --dist "${FAKE_ROOT}/dist" 2>"${FAKE_ROOT}/fastpath.err"; }

case_begin "fastpath.sh reads the live release from the hosts and prints the decision"
setup_fast_cli other
OUT="$(fastpath_cli)"
assert_eq "$(fp_get "${OUT}" path)" "fast" "path fast"
assert_eq "$(fp_get "${OUT}" live)" "${SHA_PREV}" "the live release is reported"
assert_eq "$(fp_get "${OUT}" target)" "${SHA_NEW}" "the target is reported"
rm -f "${HUB_CURRENT}"  # lint: local-temp
OUT="$(fastpath_cli)" || true
assert_eq "$(fp_get "${OUT}" path)" "full" "a broken hub link: fail closed to the full path"
case "$(fp_get "${OUT}" reasons)" in *"cannot be determined"*) case_ok "reason: live release unknown" ;; *) case_fail "reason: $(fp_get "${OUT}" reasons)" ;; esac
drop_fast_source
teardown_case
case_end

case_begin "fastpath.sh: a valid-looking release followed by a transport failure is not trusted"
setup_fast_cli other
export FAKE_SSH_TRANSPORT_FAIL_MATCH="readlink"
OUT="$(fastpath_cli)" || true
unset FAKE_SSH_TRANSPORT_FAIL_MATCH
assert_eq "$(fp_get "${OUT}" path)" "full" "ssh exit 255 after the link was printed"
case "$(fp_get "${OUT}" reasons)" in *"cannot be determined"*) case_ok "reason: live release unknown" ;; *) case_fail "reason: $(fp_get "${OUT}" reasons)" ;; esac
drop_fast_source
teardown_case
case_end

case_begin "fastpath.sh: a hub release without its binary behind the link"
setup_fast_cli other
rm -rf "${HUB_RELEASES:?}/${SHA_PREV}/bin"  # lint: local-temp
assert_eq "$(fp_get "$(fastpath_cli)" path)" "full" "the hub release has no binary behind its link"
drop_fast_source
teardown_case
case_end

case_begin "fastpath.sh: a live RELEASE without source=, a dist source of another commit"
setup_fast_cli other
sed -i '/^source=/d' "${HUB_RELEASES}/${SHA_PREV}/RELEASE"
assert_eq "$(fp_get "$(fastpath_cli)" path)" "full" "the live RELEASE has no source="
printf 'source=%s\n' "${FAST_LIVE_FULL}" >>"${HUB_RELEASES}/${SHA_PREV}/RELEASE"
assert_eq "$(fp_get "$(fastpath_cli)" path)" "fast" "control: source= restored"
sed -i 's/^source=.*/source='"${FAST_LIVE_FULL}"'/' "${FAKE_ROOT}/dist/${SHA_NEW}/RELEASE"
assert_eq "$(fp_get "$(fastpath_cli)" path)" "full" "the dist source= is another commit than the target"
drop_fast_source
teardown_case
case_end

case_begin "fastpath.sh: the running hub or the served web disagree with the links"
setup_fast_cli other
printf '{"version":"%s"}' "cccccccccccc" >"${FAKE_ROOT}/container/version.json"
assert_eq "$(fp_get "$(fastpath_cli)" path)" "full" "the served PWA is not the web link's release"
printf '{"version":"%s"}' "${SHA_PREV}" >"${FAKE_ROOT}/container/version.json"
assert_eq "$(fp_get "$(fastpath_cli)" path)" "fast" "control: restored"
ln -sfn "releases/cccccccccccc" "${HUB_CURRENT}"
mkdir -p "${HUB_RELEASES}/cccccccccccc/bin"
cp "${FAKE_WORKER_BIN}" "${HUB_RELEASES}/cccccccccccc/bin/canterlot-hub"
assert_eq "$(fp_get "$(fastpath_cli)" path)" "full" "the running hub executable is not from the link's release"
drop_fast_source
teardown_case
case_end

printf '\nfastpath: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
