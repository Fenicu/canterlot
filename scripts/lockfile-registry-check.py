#!/usr/bin/env python3
"""Ловит в отслеживаемых package-lock.json адреса `resolved` не с публичного реестра npm.

Приватное зеркало (например, в локальной сети делегата) попадает в лок-файл
молча и ломает установку у всех остальных. Проверка читает только файлы,
которые знает git, и печатает файл и первые найденные строки.
"""

import re
import subprocess
import sys
from pathlib import Path

ALLOWED = 'https://registry.npmjs.org/'
RESOLVED = re.compile(r'"resolved"\s*:\s*"([^"]*)"')
SHOWN = 5
HINT = (
    "подсказка: sed -i 's#\"resolved\": \"https\\?://[^/]*/#\"resolved\": \"https://registry.npmjs.org/#' <файл>; "
    "хеши integrity остаются верными, зеркало проксирует npm"
)


def offending(text):
    """Пары (номер строки, адрес) для `resolved` вне публичного реестра."""
    found = []
    for number, line in enumerate(text.splitlines(), 1):
        match = RESOLVED.search(line)
        if match and not match.group(1).startswith(ALLOWED):
            found.append((number, match.group(1)))
    return found


def tracked_locks(root):
    out = subprocess.run(
        ['git', 'ls-files', '-z', '--', '*package-lock.json'],
        cwd=root, capture_output=True, check=True,
    ).stdout.decode('utf-8')
    return [name for name in out.split('\0') if name]


def check(root):
    bad = 0
    for name in tracked_locks(root):
        path = Path(root) / name
        if not path.is_file():
            continue
        found = offending(path.read_text(encoding='utf-8'))
        if not found:
            continue
        bad += 1
        print(f'{name}: {len(found)} адресов resolved вне {ALLOWED}', file=sys.stderr)
        for number, url in found[:SHOWN]:
            print(f'  {name}:{number}: {url}', file=sys.stderr)
        print(HINT, file=sys.stderr)
    return 1 if bad else 0


def main():
    root = Path(__file__).resolve().parent.parent
    return check(root)


if __name__ == '__main__':
    sys.exit(main())
