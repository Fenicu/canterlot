#!/usr/bin/env bash
# Каждая пересборка оставляет в target/*/deps старые копии тестовых бинарников (десятки мегабайт
# каждая), а cargo 1.100 держит готовые файлы единицы в target/*/build/<пакет>/<хэш>/out: за день
# target разрастается на десятки гигабайт. У каждой единицы сборки остаётся самая свежая копия:
# в deps прочие уходят вместе с .d и .dwo, в build — целым каталогом <хэш>. Единица — по отпечатку
# cargo: старый расклад хранит <профиль>/.fingerprint/<пакет>-<хэш>/<вид>-<цель>.json, новый —
# <профиль>/build/<пакет>/<хэш>/fingerprint/<вид>-<цель>.json (хэш тот же, что в имени файла
# deps). Ключ единицы: пакет, вид с целью, возможности, профиль и флаги. Стебель имени общий у
# разных текущих единиц (тест `version` в двух пакетах, бинарник и его тесты), поэтому по нему
# не группируется. В build трогаются только единицы бинарников и тестов с исполняемым файлом
# больше 1 МиБ в out; библиотеки и скрипты сборки не трогаются, как и каталог без ровно одного
# отпечатка. Та же логика в воркере: exec/maintenance.rs.
set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target}"
python3 - "$root" <<'PY'
import collections
import json
import os
import re
import shutil
import sys

root = sys.argv[1]
freed = 0
pattern = re.compile(r"^(.+)-([0-9a-f]{16})$")
FIELDS = ("features", "target", "profile", "compile_kind", "rustflags")
# Готовые файлы в out бывают и у библиотек (rlib, .so): уборке подлежат только
# единицы бинарников и тестов.
BINARY_UNITS = ("bin-", "test-")


def read_unit(directory):
    """Единственный `<вид>-<цель>.json` каталога отпечатка и его содержимое."""
    try:
        entries = os.listdir(directory)
    except OSError:
        return None
    units = [name[:-5] for name in entries if name.endswith(".json")]
    if len(units) != 1:
        return None
    try:
        with open(os.path.join(directory, units[0] + ".json")) as handle:
            return units[0], json.load(handle)
    except (OSError, ValueError):
        return None


def unit_key(package, unit, data):
    fields = tuple(json.dumps(data.get(field), sort_keys=True) for field in FIELDS)
    return (package, unit) + fields


def old_unit_keys(fingerprint):
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
        unit = read_unit(directory)
        if unit is None:
            continue
        keys[hash_] = unit_key(package, *unit)
    return keys


def has_binary(out):
    """Исполняемый файл больше 1 МиБ среди готовых файлов единицы."""
    try:
        entries = os.listdir(out)
    except OSError:
        return False
    for name in entries:
        path = os.path.join(out, name)
        try:
            if (
                os.path.isfile(path)
                and os.access(path, os.X_OK)
                and os.path.getsize(path) > 1 << 20
            ):
                return True
        except OSError:
            continue
    return False


def tree_size(path):
    total = 0
    for current, _, files in os.walk(path):
        for name in files:
            try:
                total += os.path.getsize(os.path.join(current, name))
            except OSError:
                pass
    return total


def prune_new_layout(build):
    """Убирает старые каталоги <хэш> единиц нового расклада cargo 1.100."""
    groups = collections.defaultdict(list)
    try:
        packages = os.listdir(build)
    except OSError:
        return 0
    for package in packages:
        package_dir = os.path.join(build, package)
        if not os.path.isdir(package_dir):
            continue
        for hash_ in os.listdir(package_dir):
            unit_dir = os.path.join(package_dir, hash_)
            if not os.path.isdir(unit_dir):
                continue
            unit = read_unit(os.path.join(unit_dir, "fingerprint"))
            if unit is None:
                continue
            name, data = unit
            if not name.startswith(BINARY_UNITS):
                continue
            out = os.path.join(unit_dir, "out")
            if not has_binary(out):
                continue
            try:
                modified = os.path.getmtime(out)
            except OSError:
                continue
            groups[unit_key(package, name, data)].append((modified, unit_dir))
    removed = 0
    for units in groups.values():
        units.sort()
        for _, path in units[:-1]:
            removed += tree_size(path)
            shutil.rmtree(path)
    return removed


deps_dirs = []
build_dirs = []
for current, _, _ in os.walk(root):
    name = os.path.basename(current)
    if name == "deps":
        deps_dirs.append(current)
    elif name == "build":
        # Только <профиль>/build и target/lint/<профиль>/build.
        relative = os.path.relpath(current, root).split(os.sep)
        if len(relative) == 2 or (len(relative) == 3 and relative[0] == "lint"):
            build_dirs.append(current)

for current in deps_dirs:
    keys = old_unit_keys(os.path.join(os.path.dirname(current), ".fingerprint"))
    groups = collections.defaultdict(list)
    for name in os.listdir(current):
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

for current in build_dirs:
    freed += prune_new_layout(current)

print(f"pruned stale test binaries: {freed / 1e9:.1f} GB")
PY
