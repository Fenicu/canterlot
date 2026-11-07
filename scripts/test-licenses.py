#!/usr/bin/env python3
"""Unit-style self-tests for SPDX license expression evaluation."""

import os
import sys
import unittest

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(__file__))
from spdx import parse_spdx, ALLOWLIST, LICENSE_OVERRIDES, apply_license_override, rust_allowlist_dependency_phrase


class TestRussianDependencyPhrase(unittest.TestCase):
    def test_rust_allowlist_dependency_phrase(self):
        cases = {
            1: 'все 1 зависимость удовлетворяет allowlist',
            2: 'все 2 зависимости удовлетворяют allowlist',
            4: 'все 4 зависимости удовлетворяют allowlist',
            5: 'все 5 зависимостей удовлетворяют allowlist',
            11: 'все 11 зависимостей удовлетворяют allowlist',
            12: 'все 12 зависимостей удовлетворяют allowlist',
            14: 'все 14 зависимостей удовлетворяют allowlist',
            21: 'все 21 зависимость удовлетворяет allowlist',
            22: 'все 22 зависимости удовлетворяют allowlist',
            25: 'все 25 зависимостей удовлетворяют allowlist',
            101: 'все 101 зависимость удовлетворяет allowlist',
            111: 'все 111 зависимостей удовлетворяют allowlist',
            341: 'все 341 зависимость удовлетворяет allowlist',
            360: 'все 360 зависимостей удовлетворяют allowlist',
        }
        for n, expected in cases.items():
            with self.subTest(n=n):
                self.assertEqual(rust_allowlist_dependency_phrase(n), expected)


class TestLicenseOverrides(unittest.TestCase):
    def test_duck_bsd_is_corrected_to_bsd_2_clause(self):
        self.assertIn('duck@0.1.12', LICENSE_OVERRIDES)
        # «BSD» не идентификатор SPDX и не проходит allowlist; LICENSE пакета — BSD-2-Clause.
        ok, _chosen, _unallowed = parse_spdx('BSD')
        self.assertFalse(ok)
        corrected = apply_license_override('duck', '0.1.12', 'BSD')
        self.assertEqual(corrected, 'BSD-2-Clause')
        ok, chosen, unallowed = parse_spdx(corrected)
        self.assertTrue(ok)
        self.assertEqual(chosen, 'BSD-2-Clause')
        self.assertEqual(unallowed, [])

    def test_override_is_scoped_to_the_exact_package_version_and_declaration(self):
        self.assertEqual(apply_license_override('duck', '0.1.13', 'BSD'), 'BSD')
        self.assertEqual(apply_license_override('another', '0.1.12', 'BSD'), 'BSD')
        self.assertEqual(apply_license_override('duck', '0.1.12', 'MIT'), 'MIT')

    def test_override_never_replaces_an_allowlisted_license(self):
        # Если пакет исправит метаданные на разрешённую лицензию, таблица не должна её подменять.
        for declared in ['MIT', 'Apache-2.0', 'BSD-2-Clause']:
            with self.subTest(declared=declared):
                self.assertEqual(apply_license_override('duck', '0.1.12', declared), declared)


class TestSpdxLicenseEvaluation(unittest.TestCase):
    def test_single_licenses(self):
        ok, chosen, _ = parse_spdx('MIT')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        ok, chosen, _ = parse_spdx('Apache-2.0')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'Apache-2.0')

        ok, chosen, _ = parse_spdx('BSD-3-Clause')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'BSD-3-Clause')

    def test_mit_0_in_allowlist(self):
        self.assertIn('MIT-0', ALLOWLIST)
        ok, chosen, unallowed = parse_spdx('MIT-0')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT-0')
        self.assertEqual(unallowed, [])

    def test_or_expressions_and_preference(self):
        # A OR B prefers MIT when both allowed
        ok, chosen, _ = parse_spdx('Apache-2.0 OR MIT')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        ok, chosen, _ = parse_spdx('MIT OR Apache-2.0')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        # Prefers Apache-2.0 when paired with other licenses
        ok, chosen, _ = parse_spdx('BSL-1.0 OR Apache-2.0')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'Apache-2.0')

        # OR satisfied when one side is unallowed
        ok, chosen, _ = parse_spdx('MIT OR LGPL-2.1-or-later')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        ok, chosen, _ = parse_spdx('Apache-2.0 OR NonExistentLicense')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'Apache-2.0')

        # OR fails when all alternatives unallowed
        ok, chosen, unallowed = parse_spdx('GPL-3.0-only OR LGPL-2.1-or-later')
        self.assertFalse(ok)
        self.assertIsNone(chosen)
        self.assertIn('GPL-3.0-only', unallowed)
        self.assertIn('LGPL-2.1-or-later', unallowed)

    def test_and_expressions(self):
        # AND satisfied only when both sides allowed
        ok, chosen, _ = parse_spdx('MIT AND BSD-3-Clause')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT AND BSD-3-Clause')

        ok, chosen, _ = parse_spdx('(MIT OR Apache-2.0) AND Unicode-3.0')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT AND Unicode-3.0')

        # AND fails when one side is unallowed
        ok, chosen, unallowed = parse_spdx('MIT AND LGPL-2.1-or-later')
        self.assertFalse(ok)
        self.assertIsNone(chosen)
        self.assertIn('LGPL-2.1-or-later', unallowed)

    def test_with_exception_clause(self):
        # LLVM-exception is allowed
        ok, chosen, _ = parse_spdx('Apache-2.0 WITH LLVM-exception')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'Apache-2.0 WITH LLVM-exception')

        # In combination with OR
        ok, chosen, _ = parse_spdx('Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        # Unsupported exception fails
        ok, chosen, unallowed = parse_spdx('Apache-2.0 WITH Autoconf-exception-2.0')
        self.assertFalse(ok)
        self.assertIsNone(chosen)
        self.assertTrue(any('Autoconf' in u for u in unallowed))

        # LLVM-exception on unallowed base license fails
        ok, chosen, _ = parse_spdx('GPL-2.0 WITH LLVM-exception')
        self.assertFalse(ok)
        self.assertIsNone(chosen)

    def test_nested_parentheses(self):
        ok, chosen, _ = parse_spdx('((MIT OR Apache-2.0) AND Unicode-3.0)')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT AND Unicode-3.0')

        ok, chosen, _ = parse_spdx('(((MIT)))')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        ok, chosen, _ = parse_spdx('((MIT OR BSD-3-Clause) OR (Apache-2.0 AND ISC))')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        ok, chosen, _ = parse_spdx('(MIT OR (Apache-2.0 AND UnknownLicense))')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        ok, chosen, _ = parse_spdx('(MIT AND (Apache-2.0 AND UnknownLicense))')
        self.assertFalse(ok)
        self.assertIsNone(chosen)

    def test_empty_and_missing_license(self):
        for empty_val in ['', '   ', None, '-']:
            ok, chosen, unallowed = parse_spdx(empty_val)
            self.assertFalse(ok)
            self.assertIsNone(chosen)
            self.assertEqual(unallowed, ['Лицензия не указана'])

    def test_legacy_slash_syntax(self):
        ok, chosen, _ = parse_spdx('Apache-2.0 / MIT')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')

        ok, chosen, _ = parse_spdx('Apache-2.0/MIT')
        self.assertTrue(ok)
        self.assertEqual(chosen, 'MIT')


if __name__ == '__main__':
    unittest.main(verbosity=2)
