#!/usr/bin/env bash
# Проверка перед сдачей работы.
#   scripts/check.sh           полная: Rust и web, смотрит только то, что затронуто относительно main
#   scripts/check.sh --fast    быстрая для небольшой правки: fmt, clippy, тесты изменённых крейтов, web check и vitest по diff
#   scripts/check.sh --all     полная без учёта путей
#   scripts/check.sh --all --no-web  полная без веб-половины: её целиком делает
#                                    scripts/check-web.sh --stack (гейт выпуска)
#   scripts/check.sh --base REF  сравнивать с REF вместо main
#   scripts/check.sh --fast -E EXPR  свой фильтр тестов nextest вместо выбора по diff (например 'binary(it) & test(/^ws::/)')
# Скрипт для рабочей копии берёт CANTERLOT_TEST_PG_PORT (своя тестовая PostgreSQL, scripts/test-db.sh).
# Прогон встаёт в очередь тяжёлых задач воркера: --all берёт аренду gate (весь станок),
# остальные режимы — build (слот). CANTERLOT_NO_LEASE=1 отключает очередь.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
original_args=("$@")
source ~/.cargo/env

mode=full
all=0
no_web=0
base_ref="${CHECK_BASE:-main}"
custom_filter=""
while (($#)); do
    case "$1" in
        --fast) mode=fast ;;
        --all) all=1 ;;
        --no-web) no_web=1 ;;
        --base) base_ref="${2:?--base требует ссылку}"; shift ;;
        -E) custom_filter="${2:?-E требует выражение}"; shift ;;
        *) echo "Использование: scripts/check.sh [--fast] [--all] [--no-web] [--base REF]" >&2; exit 2 ;;
    esac
    shift
done

lease_kind=build
((all)) && lease_kind=gate
source scripts/lease.sh
lease_self_wrap "$lease_kind" "check.sh${original_args[*]:+ ${original_args[*]}}" \
    bash "$PWD/scripts/check.sh" "${original_args[@]}"
# Гейт исключителен только среди арендованных задач: нагрузка в начале объясняет ложные падения.
printf 'нагрузка: load1 %s на %s ядер, аренда %s\n' "$(cut -d' ' -f1 /proc/loadavg)" "$(nproc)" \
    "${CANTERLOT_LEASE_HELD:-нет}"

# Бюджет аренды делится между линтом и тестовой сборкой, идущими параллельно: каждой по
# половине потоков сборки (CARGO_BUILD_JOBS аренды не больше уже заданного), тестам все.
half_jobs=""
if [[ -n "${CANTERLOT_LEASE_BUDGET:-}" ]]; then
    lease_jobs=${CARGO_BUILD_JOBS:-$CANTERLOT_LEASE_BUDGET}
    half_jobs=$(((lease_jobs + 1) / 2))
fi

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
# canterlot-speech собирает transcribe.cpp; его фича cuda требует CUDA, поэтому крейт
# идёт отдельно от прогонов --all-features и без cuda. В --fast — только если он менялся.
want_speech=$want_rust
if ((!unknown)) && [[ "$mode" == fast ]]; then
    want_speech=0
    while IFS= read -r path; do
        case "$path" in
            crates/canterlot-speech/* | Cargo.toml | Cargo.lock | rust-toolchain.toml | .cargo/* | .config/* | scripts/check.sh)
                want_speech=1 ;;
        esac
    done <<<"$changed"
fi
# --no-web снимает ровно веб-половину: версия, Rust и canterlot-speech идут как
# обычно. Так гейт выпуска не гоняет веб дважды: полный прогон с Playwright и
# стендом делает следующий scripts/check-web.sh --stack.
((no_web)) && want_web=0
printf 'режим: %s, Rust: %s, web: %s, canterlot-speech: %s\n' "$mode" "$((want_rust))" "$((want_web))" "$((want_speech))"

if ((all)); then
    stage "версия"
    bash scripts/check-version.sh
    stage_done "версия"
fi

# --- Rust ---
nextest_filter() {
    # Выражение nextest для изменённых крейтов; зависимые от них проверяет полный прогон.
    local expr="" path crate rest name
    declare -A src_changed=() test_bins=() it_modules=() whole_it=()
    while IFS= read -r path; do
        case "$path" in
            crates/*)
                crate=${path#crates/}
                crate=${crate%%/*}
                [[ "$crate" == canterlot-speech ]] && continue
                rest=${path#crates/$crate/}
                if [[ "$rest" =~ ^tests/it/([^/]+)\.rs$ ]]; then
                    # Один бинарник it: модуль выбирает свои тесты, main.rs — весь бинарник.
                    if [[ "${BASH_REMATCH[1]}" == main ]]; then
                        whole_it[$crate]=1
                    else
                        it_modules[$crate]+=" ${BASH_REMATCH[1]}"
                    fi
                elif [[ "$rest" == tests/it/support/* ]]; then
                    whole_it[$crate]=1
                elif [[ "$rest" =~ ^tests/([^/]+)\.rs$ ]]; then
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
    for crate in "${!whole_it[@]}"; do
        [[ -n "${src_changed[$crate]:-}" ]] && continue
        expr+="${expr:+ | }(package($crate) & binary(it))"
    done
    for crate in "${!it_modules[@]}"; do
        [[ -n "${src_changed[$crate]:-}" || -n "${whole_it[$crate]:-}" ]] && continue
        for name in ${it_modules[$crate]}; do
            expr+="${expr:+ | }(package($crate) & binary(it) & test(/^${name}::/))"
        done
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
            # Выбор по diff может не найти ни одного теста, который nextest запустит: например,
            # изменились только живые тесты под #[ignore]. Это не ошибка. Свой фильтр -E
            # остаётся строгим: пустая выборка там скорее опечатка.
            if [[ -z "$custom_filter" && "$expr" != all ]]; then
                filter+=(--no-tests=warn)
            fi
        fi
    fi
    local status=0
    ${half_jobs:+env CARGO_BUILD_JOBS="$half_jobs"} cargo nextest run --workspace --exclude canterlot-speech \
        --all-features --profile "$profile" --test-threads "${CANTERLOT_TEST_THREADS:-$(nproc)}" "${filter[@]}" || status=$?
    python3 scripts/slowest-tests.py "target/nextest/$profile/junit.xml" 8 || true
    return "$status"
}

run_speech_tests() {
    ((want_speech)) || return 0
    local profile=default
    local -a filter=()
    [[ "$mode" == fast ]] && profile=fast
    [[ -n "$custom_filter" ]] && filter=(-E "$custom_filter" --no-tests=pass)
    ${half_jobs:+env CARGO_BUILD_JOBS="$half_jobs"} cargo nextest run -p canterlot-speech --profile "$profile" \
        --test-threads "${CANTERLOT_TEST_THREADS:-$(nproc)}" "${filter[@]}"
}

lint_rust() {
    # Один clippy с всеми фичами; сборка без фич только проверяется компиляцией,
    # а из её тестов запускается единственный собственный. canterlot-speech —
    # отдельно и без фич (cuda нужна только сборке выпуска).
    cargo clippy --workspace --exclude canterlot-speech --all-targets --all-features -- -D warnings
    if ((want_speech)); then
        cargo clippy -p canterlot-speech --all-targets -- -D warnings
    fi
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
    # Инкрементальный кэш clippy почти не экономит, а target/lint растёт на
    # десятки гигабайт за день (57 ГБ за сутки), поэтому у линта он выключен.
    # Тестовая сборка ниже сохраняет обычное поведение.
    stage "линт и тесты (параллельно)"
    lint_log=$(mktemp)
    (
        export CARGO_TARGET_DIR="$PWD/target/lint"
        export CARGO_INCREMENTAL=0
        [[ -n "$half_jobs" ]] && export CARGO_BUILD_JOBS="$half_jobs"
        lint_rust
    ) >"$lint_log" 2>&1 &
    lint_pid=$!
    run_status=0
    run_nextest || run_status=$?
    run_speech_tests || run_status=$?
    lint_status=0
    wait "$lint_pid" || lint_status=$?
    if ((lint_status)); then
        echo "--- линт упал ---"
        tail -n 80 "$lint_log"
    fi
    rm -f "$lint_log"
    stage_done "линт и тесты"
    # В любом режиме и при любом исходе: у исполнителей идёт только --fast, часто с падениями,
    # а старые копии тестовых бинарников копятся по гигабайту за пересборку.
    bash scripts/prune-target.sh || true
    ((run_status == 0 && lint_status == 0)) || exit 1
    # После гейта инкрементальный кэш своей рабочей копии не нужен: при тех же исходниках
    # его удаление ничего не пересобирает, а копится он на десятки гигабайт. Сборка тестов
    # с CARGO_INCREMENTAL=0 вместо этого держала бы вторую копию тестовых бинарников, и
    # prune-target.sh заставлял бы следующий --fast их перелинковать (замер 2026-11-08).
    if ((all)) && [[ -d target/debug/incremental && ! -L target/debug/incremental ]]; then
        rm -rf -- "$PWD/target/debug/incremental"
        echo "удалён target/debug/incremental"
    fi
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
