#!/usr/bin/env bash
set -euo pipefail

source ~/.cargo/env
bash "$(dirname "${BASH_SOURCE[0]}")/reap-stray-tests.sh"
# .cargo/config.toml задаёт общую базу 55440; переменная окружения её перекрывает.
if [[ -n "${CANTERLOT_TEST_PG_PORT:-}" && -z "${DATABASE_URL:-}" ]]; then
    export DATABASE_URL="postgres://canterlot:canterlot@127.0.0.1:${CANTERLOT_TEST_PG_PORT}/canterlot"
fi
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
# Сборка без фич проверяется компиляцией: все её тесты, кроме одного, повторяет прогон ниже.
cargo test --workspace --no-run
cargo test -p canterlot-worker --lib synthetic_profiles_are_disabled_without_feature
cargo test --workspace --all-features --no-fail-fast
