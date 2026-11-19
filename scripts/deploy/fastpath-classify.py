#!/usr/bin/env python3
"""Классификатор изменений между развёрнутым релизом и целью для быстрого пути выпуска.

Использование: fastpath-classify.py SRC LIVE_FULL TARGET_FULL

Только читает git-репозиторий SRC. Оба коммита — полные 40-значные хэши, они
разрешаются как есть (`--end-of-options`, без объектов-замен) и обязаны вернуть
тот же хэш. Любой сбой git, ссылка замены или неоднозначность — выход с кодом 2
и сообщение в stderr: вызывающий читает это как полный путь.

Печатает ключ=значение, по одному разу каждый:
  ancestor=yes|no          развёрнутый релиз — предок цели
  migration_inputs=...     изменённые файлы, от которых зависит, какие миграции вшиты
                           в бинарь и как они применяются (каталог миграций, db.rs, main.rs, installation*, presets* хаба)
  other=...                изменённые файлы вне списка прикладного кода (список ниже)
  changed=N                число изменённых путей

Список прикладного кода (быстрый путь без репетиции допустим, только если other пуст):
исходники и тесты крейтов, исходники/статика/тесты веба, документация, README.md,
CHANGES.rst и изменение одного номера версии в Cargo.toml, Cargo.lock, web/package.json,
web/package-lock.json. Всё остальное (scripts/, docker/, rust-toolchain*, .cargo/,
build.rs, зависимости) — только с репетицией.
"""
import json
import os
import re
import subprocess
import sys
import tomllib

FULL = re.compile(r"^[0-9a-f]{40}$")
MIGRATION_PREFIX = "crates/canterlot-hub/migrations/"
# Загрузка миграций и всё, что хаб исполняет при запуске с записью в базу вне манифеста:
# installation::check и presets::seed_all_missing (см. тест serve_startup_calls_are_pinned в main.rs).
MIGRATION_CODE = re.compile(
    r"^crates/canterlot-hub/src/(db\.rs|main\.rs|installation(\.rs|/.*)|presets(\.rs|/.*))$"
)
ALLOW = [
    re.compile(r"^crates/[^/]+/src/.+"),
    re.compile(r"^crates/[^/]+/tests/.+"),
    re.compile(r"^web/src/.+"),
    re.compile(r"^web/static/.+"),
    re.compile(r"^web/tests/.+"),
    re.compile(r"^docs/.+"),
    re.compile(r"^README\.md$"),
    re.compile(r"^CHANGES\.rst$"),
]
VERSION_ONLY = ("Cargo.toml", "Cargo.lock", "web/package.json", "web/package-lock.json")


def fail(message):
    print(f"fastpath-classify: {message}", file=sys.stderr)
    sys.exit(2)


def git(src, *args, check=True):
    env = dict(os.environ, GIT_NO_REPLACE_OBJECTS="1", GIT_TERMINAL_PROMPT="0")
    proc = subprocess.run(
        ["git", "--no-replace-objects", "-C", src, *args],
        capture_output=True,
        env=env,
    )
    if check and proc.returncode != 0:
        fail(f"git {' '.join(args[:3])} failed ({proc.returncode}): {proc.stderr.decode(errors='replace').strip()[:200]}")
    return proc


def exact_commit(src, full):
    if not FULL.match(full):
        fail(f"not a full commit id: {full!r}")
    out = git(src, "rev-parse", "--verify", "--end-of-options", f"{full}^{{commit}}").stdout.decode().strip()
    if out != full:
        fail(f"{full} resolved to {out}")


def show(src, rev, path):
    proc = git(src, "cat-file", "blob", f"{rev}:{path}", check=False)
    return proc.stdout if proc.returncode == 0 else None


def only_version_differs(src, live, target, path):
    a, b = show(src, live, path), show(src, target, path)
    if a is None or b is None:
        return False
    try:
        if path == "Cargo.toml":
            da, db = tomllib.loads(a.decode()), tomllib.loads(b.decode())
            for d in (da, db):
                d.get("workspace", {}).get("package", {})["version"] = ""
            return da == db
        if path == "Cargo.lock":
            da, db = tomllib.loads(a.decode()), tomllib.loads(b.decode())
            for d in (da, db):
                for pkg in d.get("package", []):
                    if "source" not in pkg:
                        pkg["version"] = ""
            return da == db
        ja, jb = json.loads(a), json.loads(b)
        for j in (ja, jb):
            j.pop("version", None)
            if isinstance(j.get("packages"), dict) and isinstance(j["packages"].get(""), dict):
                j["packages"][""].pop("version", None)
        return ja == jb
    except (ValueError, KeyError, AttributeError, TypeError):
        return False


def esc(paths):
    return ", ".join(json.dumps(p, ensure_ascii=True) for p in paths)


def main():
    if len(sys.argv) != 4:
        fail("usage: fastpath-classify.py SRC LIVE_FULL TARGET_FULL")
    src, live, target = sys.argv[1:]
    if not os.path.isdir(src):
        fail(f"no repository at {src}")
    if git(src, "for-each-ref", "refs/replace").stdout.strip():
        fail("replacement refs (refs/replace) are present: the diff could describe other objects")
    exact_commit(src, live)
    exact_commit(src, target)
    rc = git(src, "merge-base", "--is-ancestor", live, target, check=False).returncode
    if rc not in (0, 1):
        fail(f"git merge-base --is-ancestor failed ({rc})")
    raw = git(src, "diff", "-z", "--name-only", "--no-renames", "--no-ext-diff", live, target, "--").stdout
    if raw and not raw.endswith(b"\0"):
        fail("git diff -z output is not NUL-terminated")
    try:
        paths = [p.decode("utf-8") for p in raw.split(b"\0") if p]
    except UnicodeDecodeError:
        fail("a changed path is not valid UTF-8")
    migration, other = [], []
    for path in paths:
        if path.startswith(MIGRATION_PREFIX) or MIGRATION_CODE.match(path):
            migration.append(path)
        elif any(rule.match(path) for rule in ALLOW):
            continue
        elif path in VERSION_ONLY and only_version_differs(src, live, target, path):
            continue
        else:
            other.append(path)
    print(f"ancestor={'yes' if rc == 0 else 'no'}")
    print(f"migration_inputs={esc(migration)}")
    print(f"other={esc(other)}")
    print(f"changed={len(paths)}")


main()
