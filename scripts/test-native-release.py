#!/usr/bin/env python3
import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location('native_release', Path(__file__).with_name('native-release.py'))
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)


class ReleaseDescriptorTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='sql-language-server-release-')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.args = argparse.Namespace(input=self.root, output=self.root / 'release.json')
        for platform, target in RELEASE.METADATA['platforms'].items():
            filename = f'fixture-{platform}.{target["format"]}'
            archive = self.root / filename
            archive.write_bytes(f'synthetic archive fixture for {platform}'.encode())
            descriptor = {
                'version': RELEASE.METADATA['version'],
                'sourceRevision': 'a' * 40,
                'assets': {platform: {
                    'url': 'https://example.test/' + filename,
                    'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
                    'format': target['format'],
                    'executable': target['executable'],
                }},
            }
            (self.root / f'{platform}.json').write_text(json.dumps(descriptor))

    def merge(self):
        with contextlib.redirect_stdout(io.StringIO()):
            RELEASE.merge(self.args)

    def test_merges_every_supported_platform(self):
        self.merge()
        descriptor = json.loads(self.args.output.read_text())
        self.assertEqual(set(descriptor['assets']), set(RELEASE.METADATA['platforms']))

    def test_rejects_changed_archive_bytes(self):
        (self.root / 'fixture-darwin-arm64.tar.gz').write_bytes(b'corrupted archive')
        with self.assertRaisesRegex(ValueError, 'checksum failed'):
            self.merge()

    def test_rejects_assets_from_different_source_commits(self):
        path = self.root / 'linux-x64.json'
        descriptor = json.loads(path.read_text())
        descriptor['sourceRevision'] = 'b' * 40
        path.write_text(json.dumps(descriptor))
        with self.assertRaisesRegex(ValueError, 'same pinned source'):
            self.merge()

    def test_requires_the_complete_platform_set(self):
        (self.root / 'win32-x64.json').unlink()
        with self.assertRaisesRegex(ValueError, 'exactly one descriptor for win32-x64'):
            self.merge()

    def test_rejects_stale_native_pins(self):
        for platform in RELEASE.METADATA['platforms']:
            path = self.root / f'{platform}.json'
            descriptor = json.loads(path.read_text())
            descriptor['version'] = '9.9.9'
            path.write_text(json.dumps(descriptor))
        with self.assertRaisesRegex(ValueError, 'native-source.json pins'):
            self.merge()


class SourcePinTests(unittest.TestCase):
    def test_native_source_matches_the_workspace(self):
        RELEASE.check_pins()


if __name__ == '__main__':
    unittest.main()
