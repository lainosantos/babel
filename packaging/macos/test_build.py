"""Portable packaging policy tests. No app, driver or Apple command is executed."""
import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET


SPEC = importlib.util.spec_from_file_location("babel_macos_packaging", Path(__file__).with_name("build.py"))
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def file(self, relative, content=b"fixture", executable=False):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        path.chmod(0o755 if executable else 0o644)
        return path

    def component(self, driver=False, location="/"):
        component = self.root / ("Driver.pkg" if driver else "App.pkg")
        component.mkdir()
        expected_path = BUILD.DRIVER_PATH if driver else BUILD.APP_PATH
        package_id = BUILD.DRIVER_PACKAGE_ID if driver else BUILD.APP_PACKAGE_ID
        bundle_id = BUILD.DRIVER_ID if driver else BUILD.BUNDLE_ID
        ET.ElementTree(ET.Element("pkg-info", identifier=package_id,
                                 **{"install-location": location})).write(component / "PackageInfo")
        bundle = component / "Payload" / expected_path
        (bundle / "Contents").mkdir(parents=True)
        with (bundle / "Contents/Info.plist").open("wb") as handle:
            plistlib.dump({"CFBundleIdentifier": bundle_id}, handle)
        scripts = component / "Scripts"
        scripts.mkdir()
        for name in (["preinstall", "postinstall"] if driver else ["preinstall"]):
            path = scripts / name
            path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            path.chmod(0o755)
        return component, bundle, expected_path, package_id

    def test_bundle_uses_native_tray_and_microphone_permission(self):
        info = BUILD.app_info("1.2.3")
        self.assertEqual(info["CFBundleExecutable"], "babel-tray")
        self.assertEqual(info["CFBundleIdentifier"], "org.babel.audio")
        self.assertEqual(info["LSMinimumSystemVersion"], "14.2")
        self.assertTrue(info["LSUIElement"])
        self.assertIn("microphone", info["NSMicrophoneUsageDescription"])
        self.assertEqual(info["CFBundleShortVersionString"], info["CFBundleVersion"])

    def test_distribution_requires_both_components_and_restart(self):
        tree = BUILD.distribution({"arm64", "x86_64"}).getroot()
        self.assertEqual(tree.find("options").get("hostArchitectures"), "arm64,x86_64")
        self.assertEqual(tree.find("volume-check/allowed-os-versions/os-version").get("min"), "14.2")
        self.assertEqual(tree.find("domains").get("enable_currentUserHome"), "false")
        choices = tree.findall("choice")
        self.assertEqual({node.find("pkg-ref").get("id") for node in choices},
                         {BUILD.APP_PACKAGE_ID, BUILD.DRIVER_PACKAGE_ID})
        self.assertTrue(all(node.get("selected") == "true" and node.get("enabled") == "false" for node in choices))
        self.assertEqual(tree.find("pkg-ref[@onConclusion='RequireRestart']").get("id"), BUILD.DRIVER_PACKAGE_ID)
        self.assertEqual(tree.find("pkg-ref/must-close/app").get("id"), BUILD.BUNDLE_ID)

    def test_staging_preserves_driver_helpers_and_omits_user_files(self):
        binaries = self.root / "bin"
        drivers = self.root / "driver"
        self.file("bin/babel", executable=True)
        self.file("bin/babel-tray", executable=True)
        package = self.file("driver/BabelAudio.pkg", content=b"original package")
        helper = self.file("driver/uninstall.sh", content=b"#!/bin/sh\n", executable=True)
        self.file("driver/babel.toml", content=b"secret")
        bundle = BUILD.stage_bundle(self.root / "Babel.app", binaries, drivers, "0.1.0")
        resources = bundle / "Contents/Resources"
        self.assertEqual((resources / "drivers/macos/BabelAudio.pkg").read_bytes(), package.read_bytes())
        self.assertEqual((resources / "drivers/macos/uninstall.sh").read_bytes(), helper.read_bytes())
        self.assertTrue((resources / "drivers/macos/uninstall.sh").stat().st_mode & 0o100)
        self.assertTrue((resources / "Documentation/voice-commands.md").is_file())
        self.assertTrue((resources / "Support/scripts/patches/whisper-dynamic-port.patch").is_file())
        self.assertTrue((resources / "pt.lproj/InfoPlist.strings").is_file())
        self.assertFalse(list(bundle.rglob("babel.toml")))
        self.assertFalse(list(bundle.rglob(".tools")))

    def test_expanded_payloads_have_expected_paths_and_scripts(self):
        for driver in [False, True]:
            component, bundle, expected, identifier = self.component(driver)
            self.assertEqual(BUILD.validate_payload(component, expected, identifier), bundle)
        components = BUILD.expanded_components(self.root)
        self.assertEqual(set(components), {BUILD.APP_PACKAGE_ID, BUILD.DRIVER_PACKAGE_ID})

    def test_payload_rejects_wrong_install_location(self):
        component, _, expected, identifier = self.component(location="/tmp")
        with self.assertRaisesRegex(RuntimeError, "installation location"):
            BUILD.validate_payload(component, expected, identifier)

    def test_payload_rejects_files_outside_owned_bundle(self):
        component, _, expected, identifier = self.component()
        (component / "Payload/Applications/Other.txt").write_text("unexpected", encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "outside the Babel bundle"):
            BUILD.validate_payload(component, expected, identifier)

    def test_payload_rejects_symlinks(self):
        component, bundle, expected, identifier = self.component()
        (bundle / "Contents/escape").symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError, "symlink"):
            BUILD.validate_payload(component, expected, identifier)

    def test_driver_cannot_lose_postinstall_script(self):
        component, _, expected, identifier = self.component(driver=True)
        (component / "Scripts/postinstall").unlink()
        with self.assertRaisesRegex(RuntimeError, "regular file"):
            BUILD.validate_payload(component, expected, identifier)

    def test_modified_embedded_package_is_detected(self):
        self.file("a/BabelAudio.pkg", content=b"original")
        self.file("b/BabelAudio.pkg", content=b"changed")
        with self.assertRaisesRegex(RuntimeError, "differs from its input"):
            BUILD.verify_matching_files(self.root / "a", self.root / "b", ["BabelAudio.pkg"])

    def test_version_rejects_paths_and_prerelease_text(self):
        self.assertEqual(BUILD.validate_version("0.1.0"), "0.1.0")
        for value in ["../outside", "1.2", "1.2.3-beta", "1.2.03", "1.2.3\n"]:
            with self.assertRaises(ValueError, msg=value):
                BUILD.validate_version(value)

    def test_macho_rejects_missing_slice_and_wrong_type(self):
        executable = self.file("babel", executable=True)
        with patch.object(BUILD, "output", return_value="arm64\n"):
            with self.assertRaisesRegex(RuntimeError, "expected architectures"):
                BUILD.validate_macho(executable, {"arm64", "x86_64"}, "EXECUTE")
        with patch.object(BUILD, "output", side_effect=["arm64\n", "MH_MAGIC_64 ARM64 DYLIB\n"]):
            with self.assertRaisesRegex(RuntimeError, "not a Mach-O EXECUTE"):
                BUILD.validate_macho(executable, {"arm64"}, "EXECUTE")

    def test_macho_rejects_nonportable_dependency(self):
        executable = self.file("babel", executable=True)
        with patch.object(BUILD, "output", side_effect=["arm64\n", "MH_MAGIC_64 ARM64 EXECUTE\n",
                "babel:\n\t/opt/homebrew/lib/libmissing.dylib (compatibility version 1.0.0)\n"]):
            with self.assertRaisesRegex(RuntimeError, "Unbundled dynamic dependency"):
                BUILD.validate_macho(executable, {"arm64"}, "EXECUTE")

    def test_macho_accepts_system_framework_dependencies(self):
        executable = self.file("babel", executable=True)
        with patch.object(BUILD, "output", side_effect=["arm64\n", "MH_MAGIC_64 ARM64 EXECUTE\n",
                "babel:\n\t/System/Library/Frameworks/CoreAudio.framework/Versions/A/CoreAudio (compatibility version 1.0.0)\n"
                "\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n"]):
            BUILD.validate_macho(executable, {"arm64"}, "EXECUTE")

    def test_foreign_output_bundle_is_preserved(self):
        bundle = self.root / "Babel.app"
        (bundle / "Contents").mkdir(parents=True)
        with (bundle / "Contents/Info.plist").open("wb") as handle:
            plistlib.dump({"CFBundleIdentifier": "unrelated.application"}, handle)
        with self.assertRaisesRegex(RuntimeError, "not Babel; preserved"):
            BUILD.check_output_destination(bundle, bundle=True)
        self.assertTrue(bundle.exists())


if __name__ == "__main__":
    unittest.main()
