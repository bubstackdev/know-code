"""Finite archive-boundary checks; does not sign, inspect keychains or execute natives."""
from pathlib import Path
import copy
import importlib.util
import io
import json
import shutil
import stat
import tarfile
import unittest
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('sign_packages', ROOT / 'scripts/sign_packages.py')
signing = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(signing)


class ArchiveBoundary(unittest.TestCase):
    def setUp(self):
        self.root = ROOT / '.artifacts/signing-boundaries' / uuid.uuid4().hex[:10]
        self.root.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.root)

    def tar(self, entries):
        archive = self.root / 'archive.tgz'
        with tarfile.open(archive, 'w:gz') as output:
            for name, kind in entries:
                item = tarfile.TarInfo(name)
                item.mode = 0o644
                item.type = kind
                item.linkname = '../../escape'
                item.size = 1 if kind == tarfile.REGTYPE else 0
                output.addfile(item, io.BytesIO(b'x') if item.size else None)
        return archive

    def test_tar_rejects_escape_links_and_duplicates(self):
        for index, entries in enumerate([
                [('../escape', tarfile.REGTYPE)], [('/escape', tarfile.REGTYPE)],
                [('a/../../escape', tarfile.REGTYPE)], [('a', tarfile.SYMTYPE)],
                [('a', tarfile.LNKTYPE)], [('a', tarfile.FIFOTYPE)],
                [('a', tarfile.REGTYPE), ('a', tarfile.REGTYPE)]]):
            with self.subTest(entries=entries), self.assertRaises(signing.Invalid):
                signing.unpack(self.tar(entries), self.root / str(index))

    def test_zip_rejects_links_duplicate_and_traversal(self):
        for index, entries in enumerate([
                [('a', stat.S_IFLNK | 0o777)], [('../escape', stat.S_IFREG | 0o644)],
                [('a', stat.S_IFREG | 0o644), ('a', stat.S_IFREG | 0o644)]]):
            archive = self.root / f'{index}.nupkg'
            with zipfile.ZipFile(archive, 'w') as output:
                for name, mode in entries:
                    item = zipfile.ZipInfo(name)
                    item.create_system = 3
                    item.external_attr = mode << 16
                    output.writestr(item, b'x')
            with self.subTest(entries=entries), self.assertRaises(signing.Invalid):
                signing.unpack(archive, self.root / f'zip-{index}')

    def test_unchanged_non_native_bytes_and_modes_survive_repack(self):
        original = self.tar([('native', tarfile.REGTYPE), ('data', tarfile.REGTYPE)])
        extracted = self.root / 'extracted'
        before = signing.unpack(original, extracted)
        (extracted / 'native').write_bytes(b'changed native signature')
        repacked = self.root / 'repacked.tgz'
        signing.repack(original, extracted, repacked)
        after = signing.unpack(repacked, self.root / 'repacked')
        signing.check_changes(before, after, {'native'})
        for field, value in [('sha256', 'bad'), ('mode', 0o755)]:
            changed = copy.deepcopy(after)
            changed['data'][field] = value
            with self.subTest(field=field), self.assertRaises(signing.Invalid):
                signing.check_changes(before, changed, {'native'})
        extra = copy.deepcopy(after)
        extra['new'] = extra['data']
        with self.assertRaises(signing.Invalid):
            signing.check_changes(before, extra, {'native'})
        after['native']['mode'] = 0o755
        with self.assertRaises(signing.Invalid):
            signing.check_changes(before, after, {'native'})

    def test_unlisted_native_payload_is_rejected(self):
        extracted = self.root / 'native-set'
        extracted.mkdir()
        (extracted / 'known').write_bytes(b'\xcf\xfa\xed\xfe')
        (extracted / 'unexpected').write_bytes(b'\xcf\xfa\xed\xfe')
        inventory = {name:{'kind':'file'} for name in ['known', 'unexpected']}
        with self.assertRaises(signing.Invalid):
            signing.check_native_set(extracted, inventory, {'known'})

    def test_bridge_only_two_native_inventory_rows_may_change(self):
        prefix = 'bridge'
        manifest = {'recipe_id':'unchanged', 'files':{
            name:{'bytes':4, 'sha256':'unsigned'} for name in signing.BRIDGE_NATIVE}}
        manifest['files']['resource'] = {'bytes':1, 'sha256':'resource'}
        before = {f'{prefix}/{name}':{'kind':'file', 'mode':0o755, **row}
                  for name, row in manifest['files'].items()}
        before[f'{prefix}/{signing.BRIDGE_MANIFEST}'] = {'kind':'file', 'mode':0o644, 'bytes':100, 'sha256':'manifest'}
        after, signed = copy.deepcopy(before), copy.deepcopy(manifest)
        for name in signing.BRIDGE_NATIVE:
            after[f'{prefix}/{name}'].update(bytes=8, sha256='signed')
            signed['files'][name] = {'bytes':8, 'sha256':'signed'}
        after[f'{prefix}/{signing.BRIDGE_MANIFEST}'].update(bytes=120, sha256='signed-manifest')
        signing.check_bridge_changes(before, after, manifest, signed, prefix)
        for changed in [dict(signed, recipe_id='changed'), dict(signed, extra=True), manifest]:
            with self.subTest(manifest=changed), self.assertRaises(signing.Invalid):
                signing.check_bridge_changes(before, after, manifest, changed, prefix)
        for field, value in [('sha256', 'tampered'), ('mode', 0o644)]:
            altered = copy.deepcopy(after)
            altered[f'{prefix}/resource'][field] = value
            with self.subTest(resource=field), self.assertRaises(signing.Invalid):
                signing.check_bridge_changes(before, altered, manifest, signed, prefix)
        altered = copy.deepcopy(after)
        altered[f'{prefix}/sourcekit-xcode-bsp']['mode'] = 0o644
        with self.assertRaises(signing.Invalid):
            signing.check_bridge_changes(before, altered, manifest, signed, prefix)

    def test_bridge_requires_original_recorded_builder(self):
        runtime = self.root / 'tool'
        runtime.mkdir()
        (runtime / signing.BRIDGE_MANIFEST).write_text('{}')
        recipe = {'changed_files':{}, 'package_manifest_sha256':'manifest', 'package_resolved_sha256':'lock', 'dependencies':{}}
        (runtime / 'source-recipe.json').write_text(json.dumps(recipe))
        builder = self.root / 'builder.py'
        builder.write_text('original source')
        producer = {'status':'staged_build_only', 'exit_code':0}
        with self.assertRaisesRegex(signing.Invalid, 'no builder digest'):
            signing.bridge_payload(runtime, {}, producer, builder)
        producer['builder_sha256'] = signing.digest(builder)
        builder.write_text('replaced source')
        with self.assertRaisesRegex(signing.Invalid, 'builder source differs'):
            signing.bridge_payload(runtime, {}, producer, builder)


if __name__ == '__main__':
    unittest.main()
