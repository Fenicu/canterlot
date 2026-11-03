#!/usr/bin/env bash
# Сверяет версию и историю изменений перед выкаткой.
#   scripts/check-version.sh
# Версия одна: `[workspace.package]` в корневом Cargo.toml. Её повторяют
# web/package.json, бейдж версии в README.md и запись в CHANGES.rst;
# расхождение — ошибка.
set -euo pipefail

cd "$(dirname "$0")/.."

python3 - <<'PY'
import json
import re
import sys
import tomllib
from pathlib import Path

cargo = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))
version = cargo.get("workspace", {}).get("package", {}).get("version")
if not version:
    sys.exit("в Cargo.toml нет [workspace.package] version")

package = json.loads(Path("web/package.json").read_text(encoding="utf-8")).get("version")
if package != version:
    sys.exit(f"версия web/package.json {package!r} не совпадает с Cargo.toml {version!r}")

readme = Path("README.md").read_text(encoding="utf-8")
badge = re.search(
    r"shields\.fenicu\.com/badge/(?:%D0%B2%D0%B5%D1%80%D1%81%D0%B8%D1%8F|версия)-(\d+\.\d+\.\d+)-",
    readme,
)
if not badge:
    sys.exit("в README.md нет бейджа версии")
if badge.group(1) != version:
    sys.exit(f"бейдж версии в README.md {badge.group(1)!r} не совпадает с Cargo.toml {version!r}")

changes = Path("CHANGES.rst").read_text(encoding="utf-8")
entries = re.findall(r"^(\d+\.\d+\.\d+) — (.+)$", changes, re.MULTILINE)
if not entries:
    sys.exit("в CHANGES.rst нет ни одной записи вида «версия — ДД.ММ.ГГГГ»")
for entry_version, date in entries:
    if not re.fullmatch(r"\d{2}\.\d{2}\.\d{4}", date.strip()):
        sys.exit(f"дата записи {entry_version} не в формате ДД.ММ.ГГГГ: {date!r}")
if not any(entry_version == version for entry_version, _ in entries):
    sys.exit(f"в CHANGES.rst нет записи под текущую версию {version}")

print(f"Версия {version}: Cargo.toml, package.json, README.md и CHANGES.rst согласованы")
PY
