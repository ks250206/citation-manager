import copy
import hashlib
from pathlib import Path
import tempfile
import unittest
import check_architecture as architecture
import check_migrations as migrations

class ArchitectureTests(unittest.TestCase):
    def metadata(self):
        packages = [{'id': n, 'name': 'cm-' + n, 'dependencies': []} for n in architecture.ALLOWED]
        return {'packages': packages, 'workspace_members': [p['id'] for p in packages]}

    def test_valid_additive_edge_and_forbidden_reverse_edge(self):
        metadata = self.metadata()
        core = next(p for p in metadata['packages'] if p['name'] == 'cm-core')
        core['dependencies'] = [{'name': 'cm-domain'}]
        architecture.validate(metadata)
        core['dependencies'].append({'name': 'cm-storage'})
        with self.assertRaisesRegex(ValueError, 'forbidden dependency'):
            architecture.validate(metadata)

    def test_framework_in_core_or_wrong_crate_count_is_rejected(self):
        metadata = self.metadata()
        metadata['packages'][1]['dependencies'] = [{'name': 'tokio'}]
        with self.assertRaisesRegex(ValueError, 'implementation dependency'):
            architecture.validate(metadata)
        metadata = self.metadata()
        metadata['workspace_members'].pop()
        with self.assertRaisesRegex(ValueError, '12 planned crates'):
            architecture.validate(metadata)

class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.name = '20261106T080000Z_issue-8_001_initial.sql'
        self.sql = b'CREATE TABLE sample(id INTEGER PRIMARY KEY);\n'
        (self.root / self.name).write_bytes(self.sql)
        self.entries = {self.name: hashlib.sha256(self.sql).hexdigest()}

    def test_new_migration_and_additive_registry(self):
        migrations.validate(self.root, self.entries, {})
        migrations.validate(self.root, self.entries, copy.copy(self.entries))

    def test_modified_file_and_rewritten_checksum_are_both_rejected(self):
        (self.root / self.name).write_bytes(b'DROP TABLE sample;')
        with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
            migrations.validate(self.root, self.entries)
        new = {self.name: hashlib.sha256((self.root / self.name).read_bytes()).hexdigest()}
        with self.assertRaisesRegex(ValueError, 'changed or deleted'):
            migrations.validate(self.root, new, self.entries)

    def test_deletion_is_rejected_against_baseline(self):
        (self.root / self.name).unlink()
        with self.assertRaisesRegex(ValueError, 'changed or deleted'):
            migrations.validate(self.root, {}, self.entries)

    def test_colliding_ids_with_different_descriptions_are_rejected(self):
        other = self.name.replace('initial.sql', 'other.sql')
        (self.root / other).write_bytes(self.sql)
        self.entries[other] = self.entries[self.name]
        with self.assertRaisesRegex(ValueError, 'duplicate migration ID'):
            migrations.validate(self.root, self.entries)

    def test_duplicate_manifest_key_invalid_timestamp_and_zero_sequence(self):
        with self.assertRaisesRegex(ValueError, 'duplicate manifest key'):
            migrations.load_manifest('{"a":"x","a":"y"}')
        for name in ['20260230T080000Z_issue-8_001_initial.sql',
                     '20261106T080000Z_issue-8_000_initial.sql']:
            (self.root / self.name).rename(self.root / name)
            with self.assertRaises(ValueError):
                migrations.validate(self.root, {name: self.entries[self.name]})
            (self.root / name).rename(self.root / self.name)

    def test_unavailable_baseline_fails_closed(self):
        import subprocess
        with self.assertRaises(subprocess.CalledProcessError):
            migrations.baseline_manifest(architecture.ROOT, 'refs/heads/nonexistent-issue-7-validation-base')

    def test_subdirectory_is_not_silently_ignored(self):
        (self.root / 'nested').mkdir()
        with self.assertRaisesRegex(ValueError, 'regular files'):
            migrations.validate(self.root, self.entries)

    def test_unregistered_file_and_symlink_rejected(self):
        with self.assertRaisesRegex(ValueError, 'manifest differ'):
            migrations.validate(self.root, {})
        original = self.root / self.name
        original.unlink()
        target = self.root / 'external'
        target.write_bytes(self.sql)
        original.symlink_to(target)
        # Keep the link target outside the migration directory.
        target.rename(self.root.parent / (self.root.name + '-external'))
        self.addCleanup((self.root.parent / (self.root.name + '-external')).unlink)
        original.unlink()
        original.symlink_to(self.root.parent / (self.root.name + '-external'))
        with self.assertRaisesRegex(ValueError, 'symlink'):
            migrations.validate(self.root, self.entries)

if __name__ == '__main__':
    unittest.main()
