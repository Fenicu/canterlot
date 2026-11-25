# shellcheck shell=bash
# Решает, затронул ли diff обвязку scripts/deploy: гонять ли её тестовый набор.
# Подключается через source.
#
#   deploy_tests_needed fast BASE   diff от merge-base BASE плюс неотслеживаемые
#   deploy_tests_needed all         diff от последнего тега vX.Y.Z, не стоящего на HEAD;
#                                   такого тега нет — гнать
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
        # База — последний тег выпуска, который НЕ стоит на самом HEAD. На уже помеченном
        # коммите (ship ставит тег первой фазой; тег руками до гейта) собственный тег дал бы
        # пустой diff, и гейт записал бы deploy_exit=skipped для коммита, меняющего обвязку.
        # Нет родителя или более раннего тега — сравнивать не с чем: гнать.
        if git describe --tags --exact-match --match 'v[0-9]*' HEAD >/dev/null 2>&1; then
            base=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 'HEAD^' 2>/dev/null) || return 0
        else
            base=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null) || return 0
        fi
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
