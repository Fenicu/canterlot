#!/usr/bin/env bash
# Проверка перед сдачей работы.
#   scripts/check.sh           полная: Rust и web, смотрит только то, что затронуто относительно main
#   scripts/check.sh --fast    быстрая для небольшой правки: fmt, clippy, тесты изменённых крейтов, web check и vitest по diff
#   scripts/check.sh --all     полная без учёта путей
#   scripts/check.sh --base REF  сравнивать с REF вместо main
#   scripts/check.sh --fast -E EXPR  свой фильтр тестов nextest вместо выбора по diff (например 'binary(ws)')
# Скрипт для рабочей копии берёт CANTERLOT_TEST_PG_PORT (своя тестовая PostgreSQL, scripts/test-db.sh).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source ~/.cargo/env

mode=full
all=0
base_ref="${CHECK_BASE:-main}"
custom_filter=""
while (($#)); do
    case "$1" in
        --fast) mode=fast ;;
        --all) all=1 ;;
        --base) base_ref="${2:?--base требует ссылку}"; shift ;;
        -E) custom_filter="${2:?-E требует выражение}"; shift ;;
        *) echo "Использование: scripts/check.sh [--fast] [--all] [--base REF]" >&2; exit 2 ;;
    esac
    shift
done

started=$SECONDS
stage() { printf '\n==> %s\n' "$1"; stage_started=$SECONDS; }
stage_done() { printf '    %s: %ds\n' "$1" $((SECONDS - stage_started)); }

# .cargo/config.toml задаёт общую базу 55440; переменная окружения её перекрывает.
if [[ -n "${CANTERLOT_TEST_PG_PORT:-}" && -z "${DATABASE_URL:-}" ]]; then
    export DATABASE_URL="postgres://canterlot:canterlot@127.0.0.1:${CANTERLOT_TEST_PG_PORT}/canterlot"
fi

# --- что изменилось относительно main ---
changed=""
unknown=1
if ((!all)) && git rev-parse --verify -q "$base_ref" >/dev/null; then
    merge_base=$(git merge-base HEAD "$base_ref")
    changed=$({ git diff --name-only "$merge_base"; git ls-files --others --exclude-standard; } | sort -u)
    [[ -n "$changed" ]] && unknown=0
fi

want_rust=1
want_web=1
if ((!unknown)); then
    want_rust=0
    want_web=0
    while IFS= read -r path; do
        case "$path" in
            docs/api/* | scripts/* | .config/*) want_rust=1; want_web=1 ;;
            web/*) want_web=1 ;;
            crates/* | Cargo.toml | Cargo.lock | rust-toolchain.toml | .cargo/*) want_rust=1 ;;
        esac
    done <<<"$changed"
fi
printf 'режим: %s, Rust: %s, web: %s\n' "$mode" "$((want_rust))" "$((want_web))"

if ((all)); then
    stage "версия"
    bash scripts/check-version.sh
    stage_done "версия"
fi

# --- Rust ---
nextest_filter() {
    # Выражение nextest для изменённых крейтов; зависимые от них проверяет полный прогон.
    local expr="" path crate rest name
    declare -A src_changed=() test_bins=()
    while IFS= read -r path; do
        case "$path" in
            crates/*)
                crate=${path#crates/}
                crate=${crate%%/*}
                rest=${path#crates/$crate/}
                if [[ "$rest" =~ ^tests/([^/]+)\.rs$ ]]; then
                    test_bins[$crate]+=" ${BASH_REMATCH[1]}"
                else
                    src_changed[$crate]=1
                fi
                ;;
            Cargo.toml | Cargo.lock | rust-toolchain.toml | .cargo/* | docs/api/* | .config/* | scripts/*)
                echo "all"
                return
                ;;
        esac
    done <<<"$changed"
    for crate in "${!src_changed[@]}"; do
        expr+="${expr:+ | }package($crate)"
    done
    for crate in "${!test_bins[@]}"; do
        [[ -n "${src_changed[$crate]:-}" ]] && continue
        for name in ${test_bins[$crate]}; do
            expr+="${expr:+ | }(package($crate) & binary($name))"
        done
    done
    echo "$expr"
}

run_nextest() {
    local profile=default
    local -a filter=()
    if [[ "$mode" == fast ]]; then
        profile=fast
        if ((!unknown)) || [[ -n "$custom_filter" ]]; then
            local expr
            expr=${custom_filter:-$(nextest_filter)}
            if [[ -z "$expr" ]]; then
                echo "тестов для изменённых файлов нет"
                return 0
            fi
            [[ "$expr" != all ]] && filter=(-E "$expr")
        fi
    fi
    local status=0
    cargo nextest run --workspace --all-features --profile "$profile" \
        --test-threads "${CANTERLOT_TEST_THREADS:-$(nproc)}" "${filter[@]}" || status=$?
    python3 scripts/slowest-tests.py "target/nextest/$profile/junit.xml" 8 || true
    return "$status"
}

lint_rust() {
    # Один clippy с всеми фичами; сборка без фич только проверяется компиляцией,
    # а из её тестов запускается единственный собственный.
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    [[ "$mode" == fast ]] && return 0
    cargo check --workspace --all-targets
    cargo nextest run -p canterlot-worker --lib -E 'test(synthetic_profiles_are_disabled_without_feature)' --no-tests=fail
}

if ((want_rust)); then
    bash scripts/reap-stray-tests.sh
    if ! cargo nextest --version >/dev/null 2>&1; then
        bash scripts/install-tools.sh
    fi
    if ! (exec 3<>"/dev/tcp/127.0.0.1/${CANTERLOT_TEST_PG_PORT:-55440}") 2>/dev/null; then
        stage "тестовая PostgreSQL"
        bash scripts/test-db.sh up
        stage_done "база"
    fi
    stage "fmt"
    cargo fmt --all --check
    stage_done fmt

    # Линт идёт в отдельном каталоге сборки одновременно с тестовой сборкой.
    stage "линт и тесты (параллельно)"
    lint_log=$(mktemp)
    (
        export CARGO_TARGET_DIR="$PWD/target/lint"
        lint_rust
    ) >"$lint_log" 2>&1 &
    lint_pid=$!
    run_status=0
    run_nextest || run_status=$?
    lint_status=0
    wait "$lint_pid" || lint_status=$?
    if ((lint_status)); then
        echo "--- линт упал ---"
        tail -n 80 "$lint_log"
    fi
    rm -f "$lint_log"
    stage_done "линт и тесты"
    ((run_status == 0 && lint_status == 0)) || exit 1
    [[ "$mode" == fast ]] || bash scripts/prune-target.sh
fi

# --- web ---
if ((want_web)); then
    stage "web"
    if [[ "$mode" == fast ]]; then
        bash scripts/check-web.sh --fast ${merge_base:+--base "$merge_base"}
    else
        bash scripts/check-web.sh
    fi
    stage_done web
fi

bash scripts/list-quarantine.sh
printf '\nготово за %ds\n' $((SECONDS - started))
