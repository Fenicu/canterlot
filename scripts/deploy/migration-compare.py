#!/usr/bin/env python3
"""Сверка манифеста миграций целевого хаба с применёнными миграциями живой базы.

Использование: migration-compare.py MANIFEST_JSON DB_ROWS

MANIFEST_JSON — вывод `canterlot-hub migrations manifest --json` (версия, описание,
контрольная сумма hex каждой вшитой миграции). DB_ROWS — строки «версия hex успех»
из `_sqlx_migrations` (успех 1 или 0), по одной на строку.

Печатает ровно одну строку:
  equal                         те же версии, те же контрольные суммы, все успешны
  pending V1,V2,...             в бинаре есть версии, которых в базе нет (всё остальное сошлось)
  mismatch <причина>            в базе версия, которой нет в бинаре, другая контрольная
                                сумма или неуспешная строка: ручной разбор, не выкат
Код выхода 0 для всех трёх ответов, 2 — если вход не разобран.
"""
import json
import re
import sys


def main():
    if len(sys.argv) != 3:
        print("usage: migration-compare.py MANIFEST_JSON DB_ROWS", file=sys.stderr)
        return 2
    try:
        manifest = json.loads(sys.argv[1])["migrations"]
        target = {int(m["version"]): str(m["checksum"]) for m in manifest}
        if len(target) != len(manifest) or not target:
            raise ValueError("empty or duplicated manifest")
    except (ValueError, KeyError, TypeError) as err:
        print(f"manifest not understood: {err}", file=sys.stderr)
        return 2
    applied = {}
    for line in sys.argv[2].splitlines():
        if not line.strip():
            continue
        m = re.fullmatch(r"(\d+) ([0-9a-f]+) ([01])", line.strip())
        if not m:
            print(f"database row not understood: {line!r}", file=sys.stderr)
            return 2
        version = int(m.group(1))
        if version in applied:
            print(f"database row repeated: {version}", file=sys.stderr)
            return 2
        applied[version] = (m.group(2), m.group(3) == "1")
    failed = sorted(v for v, (_, ok) in applied.items() if not ok)
    if failed:
        print(f"mismatch failed migration rows in the database: {','.join(map(str, failed))}")
        return 0
    unknown = sorted(v for v in applied if v not in target)
    if unknown:
        print(f"mismatch the database has migrations the target binary lacks: {','.join(map(str, unknown))}")
        return 0
    changed = sorted(v for v, (checksum, _) in applied.items() if target[v] != checksum)
    if changed:
        print(f"mismatch applied migrations whose checksum differs from the target binary: {','.join(map(str, changed))}")
        return 0
    pending = sorted(v for v in target if v not in applied)
    if pending:
        print(f"pending {','.join(map(str, pending))}")
        return 0
    print("equal")
    return 0


sys.exit(main())
