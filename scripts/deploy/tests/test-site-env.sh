#!/usr/bin/env bash
# shellcheck disable=SC2015 # assert pattern: both branches always exit 0
# Умолчания deploy.conf — нейтральный пример; .superpowers/site.env перекрывает
# их, а явная переменная окружения главнее файла стенда.
set -euo pipefail

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "${HARNESS_DIR}/.." && pwd)"

PASS_COUNT=0
FAIL_COUNT=0
CASE_FAILED=0
case_begin() { CASE_FAILED=0; printf '=== case: %s ===\n' "$1"; }
case_ok() { PASS_COUNT=$((PASS_COUNT + 1)); printf 'PASS: %s\n' "$1"; }
case_fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1)); CASE_FAILED=1
    printf 'FAIL: %s\n' "$1" >&2
}
case_end() { [ "${CASE_FAILED}" = "0" ] && printf 'case OK\n'; }
assert_eq() {
    if [ "$1" = "$2" ]; then case_ok "$3"; else case_fail "$3 (got [$1], want [$2])"; fi
}

# deploy.conf читает окружение и печатает три значения. env -i убирает переменные
# разработчика: проверяются только умолчания, файл стенда и явное окружение.
deploy_values() {
    local site_env="$1"
    shift
    # shellcheck disable=SC2016 # $1 и ${...} раскрывает дочерний bash, а не этот
    env -i PATH="${PATH}" HOME="${HOME}" CANTERLOT_SITE_ENV="${site_env}" "$@" \
        bash -c 'source "$1/deploy.conf"; printf "%s|%s|%s\n" "${HUB_SSH}" "${SITE_ORIGIN}" "${HUB_CTID}"' _ "${DEPLOY_DIR}"
}

tmp="$(mktemp -d /tmp/canterlot-site-env-test.XXXXXX)"
trap 'rm -rf "${tmp}"' EXIT

case_begin "neutral defaults without a site file"
assert_eq "$(deploy_values "${tmp}/absent.env")" \
    "hub|https://hub.example.com|101" "defaults are neutral examples"

case_begin "site file overrides a default"
cat >"${tmp}/site.env" <<'EOF'
: "${HUB_SSH:=stand-hub}"
: "${SITE_ORIGIN:=https://stand.example.net}"
: "${HUB_CTID:=555}"
EOF
assert_eq "$(deploy_values "${tmp}/site.env")" \
    "stand-hub|https://stand.example.net|555" "site file wins over the defaults"

case_begin "explicit environment overrides the site file"
assert_eq "$(deploy_values "${tmp}/site.env" HUB_SSH=explicit-hub)" \
    "explicit-hub|https://stand.example.net|555" "environment wins over the site file"
assert_eq "$(deploy_values "${tmp}/site.env" HUB_SSH=explicit-hub SITE_ORIGIN=https://explicit.example.net)" \
    "explicit-hub|https://explicit.example.net|555" "each variable is overridden on its own"

case_begin "site file sets only variables it names"
assert_eq "$(deploy_values "${tmp}/site.env")" \
    "stand-hub|https://stand.example.net|555" "same result on a second read"

printf 'site-env harness: %s passed, %s failed\n' "${PASS_COUNT}" "${FAIL_COUNT}"
[ "${FAIL_COUNT}" = "0" ]
