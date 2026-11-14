#!/usr/bin/env bash
# Выбор набора scripts/deploy/tests по diff: какие пути считаются обвязкой и
# когда тега выпуска нет. Быстрый тест, стенд не поднимает.
set -euo pipefail

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HARNESS_DIR}/../../.." && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=../changed.sh
source "${REPO_ROOT}/scripts/deploy/changed.sh"

tmp=$(mktemp -d /tmp/canterlot-deploy-changed.XXXXXX)
trap 'rm -rf "$tmp"' EXIT
pass=0

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

in_repo() {
    local dir=$1
    shift
    (cd "$dir" && "$@")
}

expect() {
    local want=$1 desc=$2
    shift 2
    local rc=0
    "$@" || rc=$?
    if [[ "$rc" == "$want" ]]; then
        pass=$((pass + 1))
        printf 'PASS: %s\n' "$desc"
    else
        fail "$desc (exit $rc, want $want)"
    fi
}

repo="${tmp}/repo"
mkdir -p "$repo"
git -C "$repo" init -q -b main
git -C "$repo" config user.email deploy-changed@example.invalid
git -C "$repo" config user.name deploy-changed
git -C "$repo" config commit.gpgsign false
printf 'base\n' >"${repo}/README.md"
git -C "$repo" add README.md
git -C "$repo" commit -qm base
base=$(git -C "$repo" rev-parse HEAD)

printf 'change\n' >>"${repo}/README.md"
expect 0 "no release tag: run" in_repo "$repo" deploy_tests_needed all
expect 1 "no deploy changes: skip" in_repo "$repo" deploy_tests_needed fast "$base"

mkdir -p "${repo}/scripts/deploy"
printf '#!/bin/sh\n' >"${repo}/scripts/deploy/cutover.sh"
git -C "$repo" add scripts/deploy/cutover.sh
git -C "$repo" commit -qm cutover
expect 0 "scripts/deploy/cutover.sh changed: run" in_repo "$repo" deploy_tests_needed fast "$base"

printf '#!/bin/sh\n' >"${repo}/scripts/deploy/new.sh"
expect 0 "untracked file under scripts/deploy/: run" in_repo "$repo" deploy_tests_needed fast HEAD
rm -f "${repo}/scripts/deploy/new.sh"

printf '#!/bin/sh\n' >"${repo}/scripts/release.sh"
git -C "$repo" add scripts/release.sh
git -C "$repo" commit -qm release
expect 0 "scripts/release.sh changed: run" in_repo "$repo" deploy_tests_needed fast HEAD~1

git -C "$repo" tag v1.0.0
printf 'after\n' >>"${repo}/README.md"
expect 1 "tag set, no deploy changes: skip" in_repo "$repo" deploy_tests_needed all

printf '#!/bin/sh\n' >>"${repo}/scripts/deploy/cutover.sh"
expect 0 "tag set, deploy change: run" in_repo "$repo" deploy_tests_needed all

printf 'all %d selection checks passed\n' "$pass"
