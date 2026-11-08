#!/usr/bin/env bash
# Каждая пересборка оставляет в target/*/deps старые копии тестовых бинарников (десятки мегабайт
# каждая): за день каталог разрастается на десятки гигабайт. У каждой единицы сборки остаётся
# самая свежая копия, прочие уходят вместе с .d и .dwo. Единица — по отпечатку cargo
# (`<профиль>/.fingerprint/<пакет>-<хэш>/<вид>-<цель>.json`, хэш тот же, что в имени бинарника):
# пакет, вид с целью, возможности, профиль и флаги. Стебель имени общий у разных текущих единиц
# (тест `version` в двух пакетах, бинарник и его тесты), поэтому по нему не группируется.
# Бинарник без отпечатка не трогается. Та же логика в воркере: exec/maintenance.rs.
set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target}"
python3 - "$root" <<'PY'
import collections
import json
import os
import re
import sys

root = sys.argv[1]
freed = 0
pattern = re.compile(r"^(.+)-([0-9a-f]{16})$")
FIELDS = ("features", "target", "profile", "compile_kind", "rustflags")


def unit_keys(fingerprint):
    keys = {}
    try:
        entries = os.listdir(fingerprint)
    except OSError:
        return keys
    for entry in entries:
        package, _, hash_ = entry.rpartition("-")
        directory = os.path.join(fingerprint, entry)
        if not package or not os.path.isdir(directory):
            continue
        units = [name[:-5] for name in os.listdir(directory) if name.endswith(".json")]
        if len(units) != 1:
            continue
        try:
            with open(os.path.join(directory, units[0] + ".json")) as handle:
                data = json.load(handle)
        except (OSError, ValueError):
            continue
        fields = tuple(json.dumps(data.get(field), sort_keys=True) for field in FIELDS)
        keys[hash_] = (package, units[0]) + fields
    return keys


for current, _, files in os.walk(root):
    if os.path.basename(current) != "deps":
        continue
    keys = unit_keys(os.path.join(os.path.dirname(current), ".fingerprint"))
    groups = collections.defaultdict(list)
    for name in files:
        path = os.path.join(current, name)
        match = pattern.match(name)
        if not match or match.group(2) not in keys:
            continue
        if os.path.isfile(path) and os.access(path, os.X_OK) and os.path.getsize(path) > 1 << 20:
            groups[keys[match.group(2)]].append(name)
    for names in groups.values():
        names.sort(key=lambda n: (os.path.getmtime(os.path.join(current, n)), n))
        for name in names[:-1]:
            for suffix in ("", ".d", ".dwo"):
                path = os.path.join(current, name + suffix)
                if os.path.exists(path):
                    freed += os.path.getsize(path)
                    os.remove(path)
print(f"pruned stale test binaries: {freed / 1e9:.1f} GB")
PY
