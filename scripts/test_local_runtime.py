"""Native runtime packaging contracts; no model, microphone or service is used."""
import io
import json
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_local_runtime as build
import local_runtime_packaging as package


def fixture(root, system="linux", arch="x86_64"):
    services = {}
    for name in ("whisper", "llama", "piper"):
        path = root / name / "bin" / name
        path.parent.mkdir(parents=True)
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", header, 18, 62)
        path.write_bytes(header)
        path.chmod(0o755)
        services[name] = {"executable": path.relative_to(root).as_posix()}
    data = root / "piper/share/espeak-ng-data"
    data.mkdir(parents=True)
    (data / "phondata").write_bytes(b"test-data")
    services["piper"]["data"] = data.relative_to(root).as_posix()
    return build.write_manifest(root, system, arch, services)


class RuntimeTests(unittest.TestCase):
    def test_windows_cmake_explicitly_selects_a_generator_supporting_architecture(self):
        for architecture, platform in [("aarch64", "ARM64"), ("x86_64", "x64")]:
            with self.subTest(architecture=architecture), mock.patch.object(build.sys, "platform", "win32"), mock.patch.object(build, "host", return_value=("windows", architecture)), mock.patch.object(build, "windows_cmake_generator", return_value="Visual Studio 17 2022"), mock.patch.object(build, "run") as run:
                build.cmake(Path("source"), Path("build"), [], 2, ["whisper-server"])
                args = run.call_args_list[0].args
                self.assertEqual(args[args.index("-G") + 1], "Visual Studio 17 2022")
                self.assertEqual(args[args.index("-A") + 1], platform)

    def test_windows_generator_matches_installed_visual_studio_and_cmake(self):
        generators = json.dumps({"generators": [{"name": name} for name in ["NMake Makefiles", "Visual Studio 17 2022", "Visual Studio 18 2026"]]})
        for architecture, component, version, expected in [
            ("aarch64", "ARM64", "18.10.12210.168", "Visual Studio 18 2026"),
            ("x86_64", "x86.x64", "17.14.36811.4", "Visual Studio 17 2022"),
        ]:
            with self.subTest(architecture=architecture), mock.patch.object(build, "host", return_value=("windows", architecture)), mock.patch.object(build, "run", side_effect=[mock.Mock(stdout=version), mock.Mock(stdout=generators)]) as run:
                self.assertEqual(build.windows_cmake_generator(), expected)
                self.assertIn("Microsoft.VisualStudio.Component.VC.Tools." + component, run.call_args_list[0].args)

    def test_windows_generator_explains_missing_compiler_or_outdated_cmake(self):
        with mock.patch.object(build, "host", return_value=("windows", "aarch64")), mock.patch.object(build, "run", return_value=mock.Mock(stdout="")):
            with self.assertRaisesRegex(RuntimeError, "Visual Studio.*was not found"):
                build.windows_cmake_generator()
        with mock.patch.object(build, "host", return_value=("windows", "aarch64")), mock.patch.object(build, "run", side_effect=[mock.Mock(stdout="18.10.12210.168"), mock.Mock(stdout='{"generators": [{"name": "Visual Studio 17 2022"}]}')]):
            with self.assertRaisesRegex(RuntimeError, "update CMake"):
                build.windows_cmake_generator()

    def test_executable_smoke_reports_bounded_native_failure_output(self):
        command = [sys.executable, "-c", "import sys; sys.stderr.write('x'*20000+'native failure sentinel'); sys.exit(7)"]
        with self.assertRaises(RuntimeError) as failure:
            build.smoke_command(command)
        message = str(failure.exception)
        self.assertIn("exited with code 7", message)
        self.assertIn("native failure sentinel", message)
        self.assertLess(len(message), 8400)

    def test_executable_smoke_timeout_keeps_failure_context(self):
        command = [sys.executable, "-c", "import sys,time; print('startup sentinel',flush=True); time.sleep(10)"]
        with self.assertRaises(RuntimeError) as failure:
            build.smoke_command(command, timeout=1)
        self.assertIn("timed out after 1s", str(failure.exception))
        self.assertIn("startup sentinel", str(failure.exception))

    def test_git_format_patch_is_applied_inside_an_outer_checkout_and_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "--quiet", root], check=True)
            source = root / ".tools/source"
            source.mkdir(parents=True)
            (source / "server.cpp").write_text("old\n")
            patch = root / "port.patch"
            patch.write_text("diff --git a/server.cpp b/server.cpp\n--- a/server.cpp\n+++ b/server.cpp\n@@ -1 +1 @@\n-old\n+announced\n")
            build.verify_patch(source, patch)
            self.assertEqual((source / "server.cpp").read_text(), "announced\n")
            build.verify_patch(source, patch)
            self.assertEqual((source / "server.cpp").read_text(), "announced\n")
            (source / "server.cpp").write_text("unrelated edit\n")
            with self.assertRaises(subprocess.CalledProcessError):
                build.verify_patch(source, patch)
            self.assertEqual((source / "server.cpp").read_text(), "unrelated edit\n")

    def test_manifest_validates_integrity_architecture_and_complete_provider_set(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            package.validate(root, "linux", "x86_64")
            with self.assertRaises(ValueError):
                package.validate(root, "windows", "x86_64")
            binary = root / "whisper/bin/whisper"
            binary.write_bytes(b"modified")
            with self.assertRaisesRegex(ValueError, "checksum"):
                package.validate(root, "linux", "x86_64")

    def test_unlisted_files_and_manifest_path_traversal_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            extra = root / "extra"
            extra.write_bytes(b"untrusted")
            with self.assertRaisesRegex(ValueError, "unlisted"):
                package.validate(root, "linux", "x86_64")
            extra.unlink()
            manifest = json.loads((root / "manifest.json").read_text())
            manifest["files"][0]["path"] = "../outside"
            (root / "manifest.json").write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "relative path"):
                package.validate(root, "linux", "x86_64")

    def test_archive_extraction_preserves_executable_modes_and_validates_the_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "original/linux-x86_64"
            fixture(original)
            archives = root / "archives"
            archives.mkdir()
            with tarfile.open(archives / "linux-x86_64.tar.gz", "w:gz") as archive:
                archive.add(original, arcname="linux-x86_64")
            build.extract_artifacts(archives, root / "extracted")
            package.validate(root / "extracted/linux-x86_64", "linux", "x86_64")
            with self.assertRaisesRegex(ValueError, "overwrite"):
                build.extract_artifacts(archives, root / "extracted")

    def test_archive_paths_cannot_escape_and_bad_download_cache_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            malicious = root / "malicious.tar.gz"
            with tarfile.open(malicious, "w:gz") as archive:
                member = tarfile.TarInfo("root/../../outside")
                member.size = 1
                archive.addfile(member, io.BytesIO(b"x"))
            with self.assertRaisesRegex(ValueError, "archive path"):
                build.unpack(malicious, root / "output")
            cached = root / "pinned.tar.gz"
            cached.write_bytes(b"preserve")
            pin = {"archive": cached.name, "size": 3, "sha256": "0" * 64}
            with self.assertRaisesRegex(ValueError, "checksum"):
                build.fetch(pin, root)
            self.assertEqual(cached.read_bytes(), b"preserve")

    def test_lock_records_exact_size_hash_and_immutable_source_refs(self):
        pins = json.loads(build.LOCK.read_text())["sources"]
        for name, pin in pins.items():
            with self.subTest(name=name):
                self.assertGreater(pin["size"], 0)
                self.assertRegex(pin["sha256"], r"^[0-9a-f]{64}$")
                if name in ("whisper", "llama", "piper", "espeak"):
                    self.assertRegex(pin["url"], r"/tar.gz/[0-9a-f]{40}$")


if __name__ == "__main__":
    unittest.main()
