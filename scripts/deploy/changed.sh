# shellcheck shell=bash
# Решает, затронул ли diff обвязку scripts/deploy: гонять ли её тестовый набор.
# Подключается через source.
#
#   deploy_tests_needed fast BASE   diff от merge-base BASE плюс неотслеживаемые
#   deploy_tests_needed all         diff от последнего тега vX.Y.Z; тега нет — гнать
#
# 0 — гнать наборы, 1 — пропустить.
#
# scripts/build-release.sh в наборах подменён, а scripts/gate.sh они не запускают,
# поэтому эти два в список не входят.

deploy_path() {
    case "$1" in
        scripts/deploy/* | scripts/release.sh | scripts/check-proxy.sh | scripts/check-version.sh | scripts/lease.sh)
            return 0
            ;;
    esac
    return 1
}

deploy_changed() {
    local path
    while IFS= read -r path; do
        [[ -n "$path" ]] || continue
        deploy_path "$path" && return 0
    done
    return 1
}

deploy_tests_needed() {
    local mode=${1:-} base=${2:-} paths
    if [[ "$mode" == all ]]; then
        base=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null) || return 0
    fi
    [[ -n "$base" ]] || return 0
    paths=$({ git diff --name-only "$base"; git ls-files --others --exclude-standard; } | sort -u) || return 0
    deploy_changed <<<"$paths"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    if deploy_tests_needed "$@"; then
        echo run
    else
        echo skip
    fi
fi
