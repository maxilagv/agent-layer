#!/usr/bin/env python3
"""Security regression checks for advisory-waiver scope and expiration."""
import datetime
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'scripts'))
from check_dependencies import waiver_scope


class WaiverTests(unittest.TestCase):
    def setUp(self):
        self.metadata = {'packages': [
            {'id': 'parser', 'name': 'parser', 'version': '1.0.0'},
            {'id': 'builder', 'name': 'builder', 'version': '2.0.0'},
        ], 'resolve': {'nodes': [
            {'id': 'parser', 'deps': []},
            {'id': 'builder', 'deps': [{'pkg': 'parser'}]},
        ]}}
        self.waiver = {'id': 'RUSTSEC-fixture', 'crate': 'parser', 'version': '1.0.0',
                       'parents': ['builder@2.0.0'], 'review_by': '2027-01-20'}
        self.today = datetime.date(2026, 10, 20)

    def test_exact_reviewed_dependency_is_allowed(self):
        waiver_scope(self.metadata, [self.waiver], self.today)

    def test_expired_review_fails_closed(self):
        with self.assertRaisesRegex(ValueError, 'expired'):
            waiver_scope(self.metadata, [self.waiver], datetime.date(2027, 1, 21))

    def test_changed_dependency_version_fails_closed(self):
        self.metadata['packages'][0]['version'] = '1.0.1'
        with self.assertRaisesRegex(ValueError, 'scope changed'):
            waiver_scope(self.metadata, [self.waiver], self.today)

    def test_new_dependency_parent_fails_closed(self):
        self.metadata['packages'].append({'id': 'other', 'name': 'untrusted-parent', 'version': '1'})
        self.metadata['resolve']['nodes'].append({'id': 'other', 'deps': [{'pkg': 'parser'}]})
        with self.assertRaisesRegex(ValueError, 'scope changed'):
            waiver_scope(self.metadata, [self.waiver], self.today)

    def test_unused_waiver_fails_closed(self):
        self.waiver['crate'] = 'removed-upstream'
        with self.assertRaisesRegex(ValueError, 'unused'):
            waiver_scope(self.metadata, [self.waiver], self.today)


if __name__ == '__main__':
    unittest.main()
