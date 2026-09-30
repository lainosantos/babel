from pathlib import Path
import json
import tempfile
import subprocess
import sys
import unittest

import release_assets as release


class ReleaseAssetsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.cargo = self.root / "Cargo.toml"
        self.cargo.write_text('[package]\nversion = "1.2.3"\n')

    def test_semver_and_manifest_must_match(self):
        self.assertEqual(release.validate_tag("v1.2.3", self.cargo), ("1.2.3", False))
        for tag in ["v1.2", "1.2.3", "v01.2.3", "v1.2.3-01", "v1.2.3-", "v1.2.4", "v1.2.3\n", "v1.2.3;evil"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.validate_tag(tag, self.cargo)

    def test_prerelease_and_build_metadata_fail_before_native_builds(self):
        for version in ["1.2.3-rc.1", "1.2.3+build.01", "1.2.3-beta+sha.abc"]:
            self.cargo.write_text(f'[package]\nversion = "{version}"\n')
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, "Native installers currently require"):
                release.validate_tag("v" + version, self.cargo)

    def test_windows_numeric_version_limits(self):
        self.cargo.write_text('[package]\nversion = "65536.0.0"\n')
        with self.assertRaisesRegex(ValueError, "must be <= 65535"):
            release.validate_tag("v65536.0.0", self.cargo)

    def fixtures(self):
        source = self.root / "download"
        for name in release.EXPECTED:
            folder = source / name
            folder.mkdir(parents=True)
            for suffix in release.required_formats(name):
                (folder / (name + suffix)).write_bytes(b"test artifact")
            (folder / "manifest.json").write_text('{"test":true}')
        return source

    def test_every_artifact_and_duplicate_metadata_are_preserved(self):
        source = self.fixtures()
        (source / next(iter(release.EXPECTED)) / "SHA256SUMS.txt").write_text("existing checksum")
        output = self.root / "release"
        self.assertEqual(release.collect(source, output, "v1.2.3"), 23)
        manifest = json.loads((output / "release-manifest.json").read_text())
        self.assertEqual(len(manifest["files"]), 23)
        self.assertEqual(len({file["asset"] for file in manifest["files"]}), 23)
        self.assertEqual(len((output / "SHA256SUMS.txt").read_text().splitlines()), 24)

    def test_missing_or_empty_platform_aborts(self):
        source = self.fixtures()
        one = source / next(iter(release.EXPECTED))
        for file in one.iterdir():
            file.unlink()
        with self.assertRaisesRegex(ValueError, "Empty release artifact"):
            release.collect(source, self.root / "release", "v1.2.3")
        one.rmdir()
        with self.assertRaisesRegex(ValueError, "Missing release artifacts"):
            release.collect(source, self.root / "release", "v1.2.3")

    def test_symlink_is_not_uploaded(self):
        source = self.fixtures()
        link = source / next(iter(release.EXPECTED)) / "external"
        link.symlink_to(self.cargo)
        with self.assertRaisesRegex(ValueError, "symlinks"):
            release.collect(source, self.root / "release", "v1.2.3")

    def test_metadata_alone_does_not_count_as_an_installer(self):
        source = self.fixtures()
        rpm = next((source / "babel-installers-linux-amd64").glob("*.rpm"))
        rpm.unlink()
        with self.assertRaisesRegex(ValueError, "Missing .rpm package"):
            release.collect(source, self.root / "release", "v1.2.3")


    def draft_fixture(self):
        source = self.fixtures()
        assets = self.root / "release"
        release.collect(source, assets, "v1.2.3")
        state_path = self.root / "draft.json"
        state = {"isDraft": True, "tagName": "v1.2.3", "assets": []}
        return assets, state_path, state

    def test_empty_partial_and_complete_drafts_are_resumable(self):
        assets, state_path, state = self.draft_fixture()
        names = sorted(path.name for path in assets.iterdir())
        for subset in [[], names[:2], names]:
            with self.subTest(names=subset):
                state["assets"] = [{"name": name} for name in subset]
                state_path.write_text(json.dumps(state))
                release.validate_draft(state_path, assets, "v1.2.3")

    def test_unexpected_draft_assets_are_rejected_without_changing_any_files(self):
        assets, state_path, state = self.draft_fixture()
        state["assets"] = [{"name": "SHA256SUMS.txt"}, {"name": "old-installer.exe"}]
        state_path.write_text(json.dumps(state))
        before = {path: path.read_bytes() for path in [state_path, *assets.iterdir()]}
        with self.assertRaisesRegex(ValueError, "unexpected assets.*old-installer.exe"):
            release.validate_draft(state_path, assets, "v1.2.3")
        self.assertEqual(before, {path: path.read_bytes() for path in before})
        self.assertEqual(set(assets.iterdir()), set(before).difference({state_path}))

    def test_published_release_and_invalid_draft_metadata_are_rejected(self):
        assets, state_path, state = self.draft_fixture()
        for update, message in [
            ({"isDraft": False}, "already published"),
            ({"isDraft": "true"}, "Invalid remote release state"),
            ({"tagName": "v1.2.4"}, "tag or asset inventory"),
            ({"assets": None}, "tag or asset inventory"),
            ({"assets": [{}]}, "Invalid remote draft asset name"),
        ]:
            with self.subTest(update=update):
                state_path.write_text(json.dumps(state | update))
                with self.assertRaisesRegex(ValueError, message):
                    release.validate_draft(state_path, assets, "v1.2.3")

    def test_draft_validation_cli_fails_before_upload_on_unexpected_assets(self):
        assets, state_path, state = self.draft_fixture()
        command = [sys.executable, str(Path(release.__file__).resolve()),
                   "--tag", "v1.2.3", "--cargo", str(self.cargo),
                   "--draft-state", str(state_path), "--assets", str(assets)]
        state_path.write_text(json.dumps(state))
        self.assertEqual(subprocess.run(command, capture_output=True).returncode, 0)
        state["assets"] = [{"name": "unverified.zip"}]
        state_path.write_text(json.dumps(state))
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("unexpected assets", result.stdout)


if __name__ == "__main__":
    unittest.main()
