#!/usr/bin/env bash
# Ставит cargo-nextest готовым бинарником в ~/.cargo/bin (сборка из исходников занимает минуты).
set -euo pipefail

if cargo nextest --version >/dev/null 2>&1; then
    exit 0
fi
case "$(uname -sm)" in
    "Linux x86_64") target=linux ;;
    "Linux aarch64") target=linux-arm ;;
    *) echo "Нет готового cargo-nextest для $(uname -sm): cargo install --locked cargo-nextest" >&2; exit 1 ;;
esac
mkdir -p "$HOME/.cargo/bin"
curl -fsSL "https://get.nexte.st/latest/$target" | tar zxf - -C "$HOME/.cargo/bin"
cargo nextest --version
