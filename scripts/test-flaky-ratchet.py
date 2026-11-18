#!/usr/bin/env python3
"""Самотесты храповика флейковых паттернов: гоняют CLI по фикстурам."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name('flaky-ratchet.py')


def run(*args):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args], capture_output=True, text=True
    )


def make_tree(root, files):
    for rel, text in files.items():
        path = Path(root) / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding='utf-8')


def write_baseline(path, counts, allowances=None):
    Path(path).write_text(
        json.dumps(
            {'version': 1, 'counts': counts, 'allowances': allowances or {}},
            ensure_ascii=False,
            indent=2,
        ),
        encoding='utf-8',
    )


class RatchetFixtureCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = self.tmp.name
        self.baseline = os.path.join(self.root, 'baseline.json')

    def check(self):
        return run('check', '--root', self.root, '--baseline', self.baseline)

    def update(self, *extra):
        return run('update', '--root', self.root, '--baseline', self.baseline, *extra)

    def read_baseline(self):
        return json.loads(Path(self.baseline).read_text(encoding='utf-8'))


class TestDetection(RatchetFixtureCase):
    def test_rust_sleep_clock_and_bare_ignore_counted(self):
        make_tree(self.root, {
            'crates/canterlot-demo/tests/it/foo.rs': '''\
use tokio::time::Duration;
async fn t() {
    tokio::time::sleep(Duration::from_millis(10)).await;
    std::thread::sleep(Duration::from_millis(10));
    let now = Utc::now();
    let t = Instant::now();
    let s = SystemTime::now();
    #[ignore]
    fn slow() {}
    #[ignore = "причина из справочника"]
    fn slow2() {}
}
''',
        })
        write_baseline(self.baseline, {})
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        out = result.stdout
        self.assertIn('crates/canterlot-demo/tests/it/foo.rs: rust_sleep: 2 > 0', out)
        self.assertIn('crates/canterlot-demo/tests/it/foo.rs: rust_clock: 2 > 0', out)
        self.assertIn('crates/canterlot-demo/tests/it/foo.rs: rust_ignore: 1 > 0', out)
        self.assertNotIn('rust_ignore: 2 > 0', out)

    def test_rust_patterns_in_comments_and_strings_are_not_counted(self):
        make_tree(self.root, {
            'crates/canterlot-demo/tests/it/foo.rs': '''\
// tokio::time::sleep(Duration::ZERO).await; Utc::now() #[ignore]
/* std::thread::sleep(x); Instant::now(); */
let s = "tokio::time::sleep(x) Utc::now() #[ignore]";
let raw = r#"tokio::time::sleep(x) #[ignore]"#;
''',
        })
        write_baseline(self.baseline, {})
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('нарушений нет', result.stdout)
        self.assertNotIn('РОСТ', result.stdout)

    def test_rust_clock_in_production_code_is_not_counted(self):
        make_tree(self.root, {
            'crates/canterlot-demo/src/lib.rs': '''\
pub fn production() { let n = Instant::now(); }
#[cfg(test)]
mod tests {
    #[test]
    fn t() { let n = Instant::now(); let at = Utc::now(); }
}
''',
        })
        write_baseline(self.baseline, {})
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('crates/canterlot-demo/src/lib.rs: rust_clock: 1 > 0', result.stdout)
        self.assertIn('строка 5', result.stdout)
        self.assertNotIn('rust_clock: 2 > 0', result.stdout)

    def test_typescript_rules_count_waits_skips_and_retries(self):
        make_tree(self.root, {
            'web/e2e/ui/foo.spec.ts': '''\
test.setTimeout(90_000);
await page.waitForTimeout(100);
await new Promise(r => setTimeout(r, 20));
test.skip(cond, 'причина');
test.skip(cond);
test.fixme(cond);
test.fixme(cond, 'причина');
retries: 2,
''',
        })
        write_baseline(self.baseline, {})
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        out = result.stdout
        self.assertIn('web/e2e/ui/foo.spec.ts: ts_wait: 1 > 0', out)
        self.assertIn('web/e2e/ui/foo.spec.ts: ts_setTimeout: 1 > 0', out)
        self.assertIn('web/e2e/ui/foo.spec.ts: ts_skip: 1 > 0', out)
        self.assertIn('web/e2e/ui/foo.spec.ts: ts_fixme: 1 > 0', out)
        self.assertIn('web/e2e/ui/foo.spec.ts: web_retries: 1 > 0', out)

    def test_typescript_patterns_in_comments_and_strings_are_not_counted(self):
        make_tree(self.root, {
            'web/tests/unit/foo.test.ts': '''\
// await page.waitForTimeout(100); test.skip(cond);
/* test.fixme(cond); retries: 2 */
const s = "page.waitForTimeout(100) setTimeout(r, 20)";
const t = `test.skip(cond); retries: 2`;
''',
        })
        write_baseline(self.baseline, {})
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('нарушений нет', result.stdout)

    def test_web_retries_with_quarantine_comment_is_not_counted(self):
        make_tree(self.root, {
            'web/e2e/ui/foo.spec.ts': '''\
// QUARANTINE: причина карантина
retries: 2,
''',
        })
        write_baseline(self.baseline, {})
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('нарушений нет', result.stdout)
        self.assertNotIn('web_retries', result.stdout)

    def test_nextest_retries_requires_quarantine_above_override(self):
        make_tree(self.root, {
            '.config/nextest.toml': '''\
# QUARANTINE: причина карантина
[[profile.default.overrides]]
filter = 'test(=a)'
retries = 2

[[profile.default.overrides]]
filter = 'test(=b)'
retries = 3
''',
        })
        write_baseline(self.baseline, {})
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('.config/nextest.toml: nextest_retries: 1 > 0', result.stdout)
        self.assertIn('строка 8', result.stdout)
        self.assertNotIn('nextest_retries: 2 > 0', result.stdout)


class TestCheck(RatchetFixtureCase):
    def test_increase_reports_file_rule_counts_and_offending_lines(self):
        make_tree(self.root, {
            'web/e2e/ui/foo.spec.ts': '''\
import { test } from '@playwright/test';
test('demo', async ({ page }) => {
  await page.waitForTimeout(100);
  await page.waitForTimeout(200);
});
''',
        })
        write_baseline(self.baseline, {'web/e2e/ui/foo.spec.ts': {'ts_wait': 1}})
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('РОСТ: web/e2e/ui/foo.spec.ts: ts_wait: 2 > 1', result.stdout)
        self.assertIn('строка 3', result.stdout)
        self.assertIn('строка 4', result.stdout)
        self.assertIn('await page.waitForTimeout(100);', result.stdout)

    def test_decrease_hints_update_without_failing(self):
        make_tree(self.root, {
            'web/e2e/ui/foo.spec.ts': 'await page.waitForTimeout(100);\n',
        })
        write_baseline(self.baseline, {'web/e2e/ui/foo.spec.ts': {'ts_wait': 5}})
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('МЕНЬШЕ: web/e2e/ui/foo.spec.ts: ts_wait: 1 < 5', result.stdout)
        self.assertIn('обновите базу', result.stdout)
        self.assertIn('update', result.stdout)


class TestUpdate(RatchetFixtureCase):
    def test_update_only_lowers_counts_and_drops_disappeared_files(self):
        make_tree(self.root, {
            'web/e2e/ui/a.spec.ts': 'await page.waitForTimeout(100);\n',
        })
        write_baseline(self.baseline, {
            'web/e2e/ui/a.spec.ts': {'ts_wait': 3, 'ts_skip': 2},
            'web/e2e/ui/gone.spec.ts': {'ts_wait': 4},
        })
        result = self.update()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            self.read_baseline()['counts'],
            {'web/e2e/ui/a.spec.ts': {'ts_wait': 1}},
        )

    def test_update_rejects_increase_without_allowance(self):
        make_tree(self.root, {
            'web/e2e/ui/foo.spec.ts': '''\
await page.waitForTimeout(100);
await page.waitForTimeout(200);
''',
        })
        write_baseline(self.baseline, {'web/e2e/ui/foo.spec.ts': {'ts_wait': 1}})
        result = self.update()
        self.assertEqual(result.returncode, 1)
        self.assertIn('НУЖНО РАЗРЕШЕНИЕ: web/e2e/ui/foo.spec.ts: ts_wait: 2 > 1', result.stdout)
        self.assertEqual(self.read_baseline()['counts'], {'web/e2e/ui/foo.spec.ts': {'ts_wait': 1}})

    def test_update_with_allowance_raises_count_records_reason_and_check_passes(self):
        make_tree(self.root, {
            'web/e2e/ui/foo.spec.ts': '''\
await page.waitForTimeout(100);
await page.waitForTimeout(200);
''',
        })
        write_baseline(self.baseline, {'web/e2e/ui/foo.spec.ts': {'ts_wait': 1}})
        result = self.update(
            '--allow-increase', 'web/e2e/ui/foo.spec.ts', 'ts_wait', 'временная мера 19.11'
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            self.read_baseline()['counts'], {'web/e2e/ui/foo.spec.ts': {'ts_wait': 2}}
        )
        self.assertEqual(
            self.read_baseline()['allowances']['web/e2e/ui/foo.spec.ts']['ts_wait']['reason'],
            'временная мера 19.11',
        )
        self.assertEqual(self.check().returncode, 0)

    def test_further_increase_after_allowance_still_fails(self):
        make_tree(self.root, {
            'web/e2e/ui/foo.spec.ts': '''\
await page.waitForTimeout(100);
await page.waitForTimeout(200);
''',
        })
        write_baseline(
            self.baseline,
            {'web/e2e/ui/foo.spec.ts': {'ts_wait': 2}},
            {'web/e2e/ui/foo.spec.ts': {'ts_wait': {'count': 2, 'reason': 'мера'}}},
        )
        Path(self.root, 'web/e2e/ui/foo.spec.ts').write_text(
            'await page.waitForTimeout(100);\n'
            'await page.waitForTimeout(200);\n'
            'await page.waitForTimeout(300);\n',
            encoding='utf-8',
        )
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('web/e2e/ui/foo.spec.ts: ts_wait: 3 > 2', result.stdout)

    def test_first_update_creates_baseline(self):
        make_tree(self.root, {
            'web/e2e/ui/foo.spec.ts': 'await page.waitForTimeout(100);\n',
        })
        self.assertFalse(Path(self.baseline).exists())
        result = self.update()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('база создана', result.stdout)
        self.assertEqual(
            self.read_baseline()['counts'], {'web/e2e/ui/foo.spec.ts': {'ts_wait': 1}}
        )
        self.assertEqual(self.check().returncode, 0)


if __name__ == '__main__':
    unittest.main(verbosity=2)
