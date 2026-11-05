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

    def component(self, location="/"):
        component = self.root / "App.pkg"
        component.mkdir()
        ET.ElementTree(ET.Element("pkg-info", identifier=BUILD.APP_PACKAGE_ID,
                                 **{"install-location": location})).write(component / "PackageInfo")
        bundle = component / "Payload" / BUILD.APP_PATH
        (bundle / "Contents").mkdir(parents=True)
        with (bundle / "Contents/Info.plist").open("wb") as handle:
            plistlib.dump({"CFBundleIdentifier": BUILD.BUNDLE_ID}, handle)
        scripts = component / "Scripts"
        scripts.mkdir()
        path = scripts / "preinstall"
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)
        return component, bundle

    def test_bundle_uses_native_tray_and_microphone_permission(self):
        info = BUILD.app_info("1.2.3")
        self.assertEqual(info["CFBundleExecutable"], "babel-tray")
        self.assertEqual(info["CFBundleIdentifier"], "org.babel.audio")
        self.assertEqual(info["LSMinimumSystemVersion"], "14.2")
        self.assertTrue(info["LSUIElement"])
        self.assertIn("microphone", info["NSMicrophoneUsageDescription"])
        self.assertEqual(info["CFBundleShortVersionString"], info["CFBundleVersion"])

    def test_app_only_package_omits_driver_component_and_restart(self):
        tree = BUILD.distribution({"arm64", "x86_64"}).getroot()
        self.assertEqual([node.get("id") for node in tree.findall("choice")], ["app"])
        self.assertIsNone(tree.find("pkg-ref[@onConclusion='RequireRestart']"))
        self.component()
        self.assertEqual(set(BUILD.expanded_components(self.root)), {BUILD.APP_PACKAGE_ID})
        unexpected = self.root / "Unexpected.pkg"
        unexpected.mkdir()
        ET.ElementTree(ET.Element("pkg-info", identifier="unrelated.pkg")).write(unexpected / "PackageInfo")
        with self.assertRaisesRegex(RuntimeError, "Unexpected product components"):
            BUILD.expanded_components(self.root)

    def test_bundle_includes_app_docs_and_omits_user_files(self):
        for name in ["babel", "babel-tray"]:
            self.file(f"bin/{name}", executable=True)
        bundle = BUILD.stage_bundle(self.root / "Babel.app", self.root / "bin", "0.1.0")
        resources = bundle / "Contents/Resources"
        self.assertFalse((resources / "drivers").exists())
        self.assertIn("BlackHole", (resources / "INSTALLATION.txt").read_text())

        self.assertTrue((resources / "Documentation/local-inference.md").is_file())
        self.assertTrue((resources / "Licenses/Manrope-OFL.txt").is_file())
        self.assertTrue((resources / "pt.lproj/InfoPlist.strings").is_file())
        self.assertFalse(list(bundle.rglob("babel.toml")))
        self.assertFalse(list(bundle.rglob(".tools")))

    def test_expanded_payload_has_expected_path_and_script(self):
        component, bundle = self.component()
        self.assertEqual(BUILD.validate_payload(component), bundle)

    def test_payload_rejects_wrong_install_location(self):
        component, _ = self.component(location="/tmp")
        with self.assertRaisesRegex(RuntimeError, "installation location"):
            BUILD.validate_payload(component)

    def test_payload_rejects_files_outside_owned_bundle(self):
        component, _ = self.component()
        (component / "Payload/Applications/Other.txt").write_text("unexpected", encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "outside the Babel bundle"):
            BUILD.validate_payload(component)

    def test_payload_rejects_symlinks(self):
        component, bundle = self.component()
        (bundle / "Contents/escape").symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError, "symlink"):
            BUILD.validate_payload(component)

    def test_package_requires_preinstall_script(self):
        component, _ = self.component()
        (component / "Scripts/preinstall").unlink()
        with self.assertRaisesRegex(RuntimeError, "regular file"):
            BUILD.validate_payload(component)

    def test_modified_embedded_package_is_detected(self):
        self.file("a/BabelApp.pkg", content=b"original")
        self.file("b/BabelApp.pkg", content=b"changed")
        with self.assertRaisesRegex(RuntimeError, "differs from its input"):
            BUILD.verify_matching_files(self.root / "a", self.root / "b", ["BabelApp.pkg"])

    def test_version_rejects_paths_and_prerelease_text(self):
        self.assertEqual(BUILD.validate_version("0.1.0"), "0.1.0")
        for value in ["../outside", "1.2", "1.2.3-beta", "1.2.03", "1.2.3\n"]:
            with self.assertRaises(ValueError, msg=value):
                BUILD.validate_version(value)

    def test_macho_rejects_missing_slice_and_wrong_type(self):
        executable = self.file("babel", executable=True)
        with patch.object(BUILD, "output", return_value="arm64\n"):
            with self.assertRaisesRegex(RuntimeError, "expected architectures"):
                BUILD.validate_macho(executable, {"arm64", "x86_64"})
        with patch.object(BUILD, "output", side_effect=["arm64\n", "MH_MAGIC_64 ARM64 DYLIB\n"]):
            with self.assertRaisesRegex(RuntimeError, "not a Mach-O EXECUTE"):
                BUILD.validate_macho(executable, {"arm64"})

    def test_macho_rejects_nonportable_dependency(self):
        executable = self.file("babel", executable=True)
        with patch.object(BUILD, "output", side_effect=["arm64\n", "MH_MAGIC_64 ARM64 EXECUTE\n",
                "babel:\n\t/opt/homebrew/lib/libmissing.dylib (compatibility version 1.0.0)\n"]):
            with self.assertRaisesRegex(RuntimeError, "Unbundled dynamic dependency"):
                BUILD.validate_macho(executable, {"arm64"})

    def test_macho_accepts_system_framework_dependencies(self):
        executable = self.file("babel", executable=True)
        with patch.object(BUILD, "output", side_effect=["arm64\n", "MH_MAGIC_64 ARM64 EXECUTE\n",
                "babel:\n\t/System/Library/Frameworks/CoreAudio.framework/Versions/A/CoreAudio (compatibility version 1.0.0)\n"
                "\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n"]):
            BUILD.validate_macho(executable, {"arm64"})

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
