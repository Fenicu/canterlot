#!/usr/bin/env python3
"""Самотесты проверки реестра в лок-файлах: гоняют код по временным git-репозиториям."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name('lockfile-registry-check.py')
spec = importlib.util.spec_from_file_location('lockfile_registry_check', SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

GOOD = '    "resolved": "https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz",\n'
MIRROR = '    "resolved": "http://192.0.2.1:4873/left-pad/-/left-pad-1.3.0.tgz",\n'


def run(root):
    return subprocess.run(
        [sys.executable, str(SCRIPT)], capture_output=True, text=True,
        env={'PATH': '/usr/bin:/bin'}, cwd=root,
    )


def repo(files, track=True):
    tmp = tempfile.TemporaryDirectory()
    root = Path(tmp.name)
    subprocess.run(['git', 'init', '-q'], cwd=root, check=True)
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding='utf-8')
    if track:
        subprocess.run(['git', 'add', '-A'], cwd=root, check=True)
    return tmp, root


class Offending(unittest.TestCase):
    def test_public_registry_is_clean(self):
        self.assertEqual(module.offending(GOOD), [])

    def test_mirror_is_reported_with_line_number(self):
        self.assertEqual(
            module.offending(GOOD + MIRROR),
            [(2, 'http://192.0.2.1:4873/left-pad/-/left-pad-1.3.0.tgz')],
        )

    def test_lookalike_origin_is_rejected(self):
        text = '"resolved": "https://registry.npmjs.org.example.test/x.tgz"\n'
        self.assertEqual(len(module.offending(text)), 1)

    def test_other_fields_are_ignored(self):
        self.assertEqual(module.offending('"homepage": "http://192.0.2.1/"\n'), [])


class Check(unittest.TestCase):
    def test_clean_lock_passes(self):
        tmp, root = repo({'web/package-lock.json': GOOD})
        with tmp:
            self.assertEqual(module.check(root), 0)

    def test_mirror_fails_and_names_file(self):
        tmp, root = repo({'web/package-lock.json': GOOD + MIRROR})
        with tmp:
            self.assertEqual(module.check(root), 1)

    def test_untracked_lock_is_not_checked(self):
        tmp, root = repo({'web/package-lock.json': MIRROR}, track=False)
        with tmp:
            self.assertEqual(module.check(root), 0)

    def test_nested_lock_is_checked(self):
        tmp, root = repo({'a/b/package-lock.json': MIRROR})
        with tmp:
            self.assertEqual(module.check(root), 1)


if __name__ == '__main__':
    unittest.main()
