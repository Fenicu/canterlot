#!/usr/bin/env bash
# Каждая пересборка оставляет в target/*/deps старые копии тестовых бинарников (десятки мегабайт
# каждая): за день каталог разрастается на десятки гигабайт. Оставляет самый свежий бинарник
# каждого имени и убирает остальные вместе с их .d и .dwo.
set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target}"
python3 - "$root" <<'PY'
import collections
import os
import re
import sys

root = sys.argv[1]
freed = 0
pattern = re.compile(r"^(.+)-([0-9a-f]{16})$")
for current, _, files in os.walk(root):
    if os.path.basename(current) != "deps":
        continue
    groups = collections.defaultdict(list)
    for name in files:
        path = os.path.join(current, name)
        match = pattern.match(name)
        if match and os.access(path, os.X_OK) and os.path.getsize(path) > 1 << 20:
            groups[match.group(1)].append(name)
    for names in groups.values():
        names.sort(key=lambda n: os.path.getmtime(os.path.join(current, n)))
        for name in names[:-1]:
            for suffix in ("", ".d", ".dwo"):
                path = os.path.join(current, name + suffix)
                if os.path.exists(path):
                    freed += os.path.getsize(path)
                    os.remove(path)
print(f"pruned stale test binaries: {freed / 1e9:.1f} GB")
PY
