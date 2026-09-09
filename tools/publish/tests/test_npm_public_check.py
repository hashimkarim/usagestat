import io
from pathlib import Path
import sys
import tarfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from npm_public_check import contents


def archive(entries, mode=0o644):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w:gz') as package:
        for name, data in entries:
            item = tarfile.TarInfo(name)
            item.mode = mode
            if data is None:
                item.type = tarfile.SYMTYPE
                item.linkname = 'package/native'
                package.addfile(item)
            else:
                item.size = len(data)
                package.addfile(item, io.BytesIO(data))
    return output.getvalue()


class PublicNpmContentsTests(unittest.TestCase):
    def test_normalizes_filesystem_modes_but_detects_changed_missing_and_extra_bytes(self):
        files = [('package/native', b'checked executable'), ('package/LICENSE', b'license')]
        expected = contents(archive(files))
        self.assertEqual(expected, contents(archive(files, mode=0o755)))
        for changed in [files[:1], files + [('package/extra', b'extra')],
                        [('package/native', b'changed executable'), files[1]]]:
            self.assertNotEqual(expected, contents(archive(changed)))

    def test_rejects_symlinks_and_duplicate_paths(self):
        for entries in [[('package/link', None)], [('package/native', b'one'), ('package/native', b'two')]]:
            with self.assertRaises(ValueError):
                contents(archive(entries))
