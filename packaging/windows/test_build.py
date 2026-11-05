import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('windows_package', HERE / 'build.py')
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


def pe(path, machine):
    value = bytearray(72)
    value[:2] = b'MZ'
    struct.pack_into('<I', value, 60, 64)
    value[64:68] = b'PE\0\0'
    struct.pack_into('<H', value, 68, machine)
    path.write_bytes(value)


class PackageTests(unittest.TestCase):
    def fixture(self, root, arch):
        binary = root / 'bin'
        binary.mkdir()
        for name in ('babel.exe', 'babel-tray.exe'):
            pe(binary / name, build.MACHINES[arch])
        return binary

    def test_stages_only_supported_application_files(self):
        for arch in build.MACHINES:
            with self.subTest(arch=arch), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                binary = self.fixture(root, arch)
                (binary / 'private.key').write_text('not a package file')
                payload = root / 'payload'
                manifest = build.stage(binary, payload, arch, '0.1.0')
                self.assertEqual(manifest['architecture'], arch)
                self.assertIn('babel.exe', manifest['files'])
                self.assertIn('babel-tray.exe', manifest['files'])
                self.assertFalse((payload / 'private.key').exists())
                self.assertIn('Virtual Cables', (payload / 'INSTALLATION.txt').read_text())

    def test_rejects_wrong_application_architecture_before_staging(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = self.fixture(root, 'x64')
            with self.assertRaisesRegex(ValueError, 'architecture'):
                build.stage(binary, root / 'payload', 'ARM64', '0.1.0')
            self.assertFalse((root / 'payload').exists())

    def test_pe_bounds_and_version_reject_invalid_inputs(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'bad.exe'
            path.write_bytes(b'MZ' + b'\0' * 62)
            with self.assertRaisesRegex(ValueError, 'offset'):
                build.pe_machine(path)
        for value in ('1.0', '1.2.3-beta', '../1.2.3', '1.2.65536'):
            with self.assertRaises(ValueError):
                build.version(value)
        self.assertEqual(build.version('1.2.3'), '1.2.3')


if __name__ == '__main__':
    unittest.main()
