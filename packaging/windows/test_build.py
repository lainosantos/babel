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
        binary, driver = root / 'bin', root / 'driver'
        binary.mkdir()
        driver.mkdir()
        for name in ('babel.exe', 'babel-tray.exe', 'babel-feedback.exe'):
            pe(binary / name, build.MACHINES[arch])
        for name in build.DRIVER_FILES:
            (driver / name).write_text('fixture')
        for name in ('BabelAudio.sys', 'babel-driver-installer.exe'):
            pe(driver / name, build.MACHINES[arch])
        suffix = 'amd64' if arch == 'x64' else 'arm64'
        (driver / 'BabelAudio.inf').write_text(f'ROOT\\BabelAudio\nBabelAudio.sys\nBabelAudio.cat\nBabel.NT{suffix}\n')
        return binary, driver

    def test_complete_allowlisted_payload_and_rejection_of_wrong_native_driver(self):
        for arch in build.MACHINES:
            with self.subTest(arch=arch), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                binary, driver = self.fixture(root, arch)
                (driver / 'private.key').write_text('not a package file')
                manifest = build.stage(binary, driver, root / 'payload', arch, '0.1.0')
                self.assertIn('drivers/windows/BabelAudio.cat', manifest['files'])
                self.assertIn('babel-tray.exe', manifest['files'])
                self.assertIn('babel-feedback.exe', manifest['files'])
                self.assertFalse((root / 'payload/drivers/windows/private.key').exists())
                other = 'ARM64' if arch == 'x64' else 'x64'
                pe(driver / 'BabelAudio.sys', build.MACHINES[other])
                with self.assertRaisesRegex(ValueError, 'architecture'):
                    build.stage(binary, driver, root / 'bad', arch, '0.1.0')
                self.assertFalse((root / 'bad').exists())

    def test_missing_catalog_and_unstamped_inf_fail_before_staging(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary, driver = self.fixture(root, 'x64')
            (driver / 'BabelAudio.cat').unlink()
            with self.assertRaisesRegex(ValueError, 'Missing'):
                build.stage(binary, driver, root / 'out', 'x64', '0.1.0')
            (driver / 'BabelAudio.cat').write_text('fixture')
            with (driver / 'BabelAudio.inf').open('a') as handle:
                handle.write('$ARCH$')
            with self.assertRaisesRegex(ValueError, 'stamp'):
                build.stage(binary, driver, root / 'out', 'x64', '0.1.0')

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
