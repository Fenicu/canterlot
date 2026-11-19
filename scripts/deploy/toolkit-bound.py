#!/usr/bin/env python3
"""Исполняемая обвязка выката и проверяемый чекаут относятся к одному коммиту цели.

Использование: toolkit-bound.py SRC TARGET_FULL TOOLKIT_DIR

Для быстрого пути без репетиции: HEAD чекаута SRC обязан быть коммитом цели (полный
хэш), отслеживаемые файлы чистые, а каждый файл scripts/deploy цели (кроме tests/)
побайтно совпадает с файлом TOOLKIT_DIR, который сейчас исполняется, и лишних файлов
в TOOLKIT_DIR нет. Печатает «ok» (код 0) или причину (код 1); любой сбой git — код 2.
"""
import os
import subprocess
import sys


def git(src, *args):
    env = dict(os.environ, GIT_NO_REPLACE_OBJECTS="1")
    proc = subprocess.run(["git", "--no-replace-objects", "-C", src, *args], capture_output=True, env=env)
    if proc.returncode != 0:
        print(f"git {' '.join(args[:2])} failed: {proc.stderr.decode(errors='replace').strip()[:200]}", file=sys.stderr)
        sys.exit(2)
    return proc.stdout


def main():
    if len(sys.argv) != 4:
        print("usage: toolkit-bound.py SRC TARGET_FULL TOOLKIT_DIR", file=sys.stderr)
        return 2
    src, target, toolkit = sys.argv[1:]
    head = git(src, "rev-parse", "HEAD").decode().strip()
    if head != target:
        print(f"the checkout is at {head[:12]}, not at the target {target[:12]}")
        return 1
    if git(src, "status", "--porcelain", "--untracked-files=no").strip():
        print("the checkout has uncommitted changes to tracked files")
        return 1
    tree = git(src, "ls-tree", "-r", "-z", target, "scripts/deploy").split(b"\0")
    want = {}
    for entry in tree:
        if not entry:
            continue
        meta, path = entry.split(b"\t", 1)
        rel = path.decode()[len("scripts/deploy/"):]
        if rel.startswith("tests/"):
            continue
        want[rel] = meta.split()[2].decode()
    have = {}
    for name in sorted(os.listdir(toolkit)):
        full = os.path.join(toolkit, name)
        if os.path.isfile(full):
            have[name] = git(src, "hash-object", "--no-filters", full).decode().strip()
    if set(want) != set(have):
        extra = sorted(set(have) ^ set(want))
        print(f"the executing toolkit and the target differ in files: {', '.join(extra)}")
        return 1
    differs = sorted(n for n in want if want[n] != have[n])
    if differs:
        print(f"the executing toolkit differs from the target in: {', '.join(differs)}")
        return 1
    print("ok")
    return 0


sys.exit(main())
