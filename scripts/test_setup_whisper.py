"""Offline installer regression tests: python -m unittest discover -s scripts -p test_setup_whisper.py."""

import hashlib
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import setup_whisper as setup


class ModelTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.model = self.directory / setup.MODEL["name"]
        self.content = b"test model bytes"
        size = patch.object(setup, "MODEL_SIZE", len(self.content))
        checksum = patch.object(setup, "MODEL_SHA256", hashlib.sha256(self.content).hexdigest())
        size.start()
        checksum.start()
        self.addCleanup(size.stop)
        self.addCleanup(checksum.stop)

    def test_valid_download_commits_only_verified_complete_model(self):
        with patch.object(setup.urllib.request, "urlopen", return_value=io.BytesIO(self.content)):
            setup.ensure_model(self.model)
        self.assertEqual(self.model.read_bytes(), self.content)
        self.assertEqual(list(self.directory.iterdir()), [self.model])
        with patch.object(setup.urllib.request, "urlopen") as download:
            setup.ensure_model(self.model)
        download.assert_not_called()

    def test_invalid_existing_model_is_preserved_without_network(self):
        self.model.write_bytes(b"user file")
        with patch.object(setup.urllib.request, "urlopen") as download:
            with self.assertRaisesRegex(RuntimeError, "existing file was preserved"):
                setup.ensure_model(self.model)
        download.assert_not_called()
        self.assertEqual(self.model.read_bytes(), b"user file")

    def test_bad_download_never_becomes_model_and_removes_own_temporary_file(self):
        for payload in [b"short", b"X" * len(self.content), self.content + b"extra"]:
            with self.subTest(payload=payload):
                with patch.object(setup.urllib.request, "urlopen", return_value=io.BytesIO(payload)):
                    with self.assertRaises(RuntimeError):
                        setup.ensure_model(self.model)
                self.assertFalse(self.model.exists())
                self.assertEqual(list(self.directory.iterdir()), [])


class SourceTests(unittest.TestCase):
    def test_patch_is_idempotent_and_user_changes_are_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "source"
            source.mkdir()
            subprocess.run(["git", "init", "--quiet", str(source)], check=True)
            target = source / "examples" / "server" / "server.cpp"
            target.parent.mkdir(parents=True)
            target.write_text("original server\n", encoding="utf-8")
            setup.run(["git", "-C", source, "add", "."], capture_output=True)
            setup.run(["git", "-C", source, "-c", "user.name=Test", "-c",
                       "user.email=test@example.invalid", "-c", "commit.gpgsign=false",
                       "commit", "--quiet", "-m", "fixture"], capture_output=True)
            commit = setup.git_output(source, "rev-parse", "HEAD")
            target.write_text("patched server\n", encoding="utf-8")
            patch_file = Path(temporary) / "dynamic.patch"
            patch_file.write_text(setup.git_output(source, "diff", "--binary", "HEAD") + "\n",
                                  encoding="utf-8")
            target.write_text("original server\n", encoding="utf-8")
            with patch.object(setup, "COMMIT", commit), patch.object(setup, "PATCH", patch_file):
                setup.ensure_source(source)
                setup.ensure_source(source)
                self.assertEqual(target.read_text(encoding="utf-8"), "patched server\n")
                target.write_text("preserve my edit\n", encoding="utf-8")
                with self.assertRaisesRegex(RuntimeError, "changes other than"):
                    setup.ensure_source(source)
                self.assertEqual(target.read_text(encoding="utf-8"), "preserve my edit\n")

    def test_unrelated_directory_is_never_replaced(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            user_file = source / "keep.txt"
            user_file.write_text("keep me", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "Refusing to replace"):
                setup.ensure_source(source)
            self.assertEqual(user_file.read_text(encoding="utf-8"), "keep me")


if __name__ == "__main__":
    unittest.main()
