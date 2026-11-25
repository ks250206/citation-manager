#!/usr/bin/env python3
"""#23 Native Messaging fixtures: pinned development extension IDs stay consistent.

The Chrome ID is derived from the public key committed in the smoke-test extension
(SHA-256 of the DER SubjectPublicKeyInfo, first 128 bits, hex digits mapped 0-f -> a-p).
"""
import base64
import hashlib
import json
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]
NATIVE = ROOT / 'browser-extension/native-manifests'


def chrome_id(public_key_b64):
    digest = hashlib.sha256(base64.b64decode(public_key_b64)).hexdigest()[:32]
    return ''.join(chr(ord('a') + int(c, 16)) for c in digest)


class NativeHostFixtures(unittest.TestCase):
    def setUp(self):
        self.ids = json.loads((NATIVE / 'extension-ids.json').read_text())
        self.chrome = json.loads((NATIVE / 'dev-extension/manifest.chrome.json').read_text())
        self.firefox = json.loads((NATIVE / 'dev-extension/manifest.firefox.json').read_text())
        self.cli = (ROOT / 'crates/native-host/src/cli.rs').read_text()

    def test_chrome_development_id_is_derived_from_the_committed_key(self):
        derived = chrome_id(self.chrome['key'])
        self.assertEqual(self.ids['development']['chrome'], [derived])
        self.assertIn(f'DEV_CHROME_EXTENSION_ID: &str = "{derived}"', self.cli)

    def test_firefox_development_id_matches_the_gecko_id(self):
        gecko = self.firefox['browser_specific_settings']['gecko']['id']
        self.assertEqual(self.ids['development']['firefox'], [gecko])
        self.assertTrue(gecko.endswith('.invalid'), 'development IDs use a reserved domain')
        self.assertIn(f'DEV_FIREFOX_EXTENSION_ID: &str = "{gecko}"', self.cli)

    def test_smoke_extension_only_requests_native_messaging(self):
        self.assertEqual(self.chrome['permissions'], ['nativeMessaging'])
        self.assertEqual(self.chrome['host_permissions'], ['http://127.0.0.1/*'])
        self.assertEqual(self.firefox['permissions'], ['nativeMessaging', 'http://127.0.0.1/*'])

    def test_release_ids_are_well_formed(self):
        for extension_id in self.ids['release']['chrome']:
            self.assertRegex(extension_id, r'^[a-p]{32}$')
        for extension_id in self.ids['release']['firefox']:
            self.assertTrue(re.fullmatch(r'[A-Za-z0-9._-]+@[A-Za-z0-9._-]+|\{[0-9a-fA-F-]{36}\}', extension_id))


if __name__ == '__main__':
    unittest.main()
