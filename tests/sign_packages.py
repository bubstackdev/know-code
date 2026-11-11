"""Finite archive-boundary checks; does not sign, inspect keychains or execute natives."""
from pathlib import Path
import copy
import importlib.util
import io
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


if __name__ == '__main__':
    unittest.main()
