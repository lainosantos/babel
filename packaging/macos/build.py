#!/usr/bin/env python3
"""Package prebuilt Babel executables and BabelAudio into a macOS installer.

Never builds application code, installs software, starts audio or downloads models.
Requires macOS packaging tools; portable validation tests run on other hosts.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET


HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
BUNDLE_ID = "org.babel.audio"
APP_PACKAGE_ID = BUNDLE_ID + ".app.pkg"
DRIVER_ID = BUNDLE_ID + ".driver"
DRIVER_PACKAGE_ID = DRIVER_ID + ".pkg"
MINIMUM_MACOS = "14.2"
ARCHES = {"universal": {"arm64", "x86_64"}, "arm64": {"arm64"}, "x86_64": {"x86_64"}}
APP_PATH = Path("Applications/Babel.app")
DRIVER_PATH = Path("Library/Audio/Plug-Ins/HAL/BabelAudio.driver")


def run(arguments, **kwargs):
    print("+", " ".join(str(value) for value in arguments), flush=True)
    return subprocess.run([str(value) for value in arguments], check=True, **kwargs)


def output(arguments):
    return run(arguments, capture_output=True, text=True).stdout


def root_version():
    package = (ROOT / "Cargo.toml").read_text(encoding="utf-8").split("[package]", 1)[1].split("\n[", 1)[0]
    return re.search(r'^version\s*=\s*"([^"]+)"', package, re.MULTILINE).group(1)


def validate_version(value):
    # CFBundleShortVersionString and Installer versions are numeric, unlike arbitrary SemVer.
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", value):
        raise ValueError("The package version must have three numeric components, for example 0.1.0")
    return value


def regular_file(path, executable=False):
    if path.is_symlink() or not path.is_file():
        raise RuntimeError(f"Expected a regular file, not a symlink: {path}")
    if executable and not path.stat().st_mode & stat.S_IXUSR:
        raise RuntimeError(f"Expected an executable file: {path}")


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def validate_macho(path, arches, file_type):
    regular_file(path, executable=True)
    actual = set(output(["lipo", "-archs", path]).split())
    if actual != arches:
        raise RuntimeError(f"{path}: expected architectures {sorted(arches)}, found {sorted(actual)}")
    for arch in sorted(arches):
        header = output(["otool", "-arch", arch, "-hv", path])
        if not re.search(r"\b" + file_type + r"\b", header):
            raise RuntimeError(f"{path} ({arch}) is not a Mach-O {file_type}")
        # Babel's release executables are self-contained apart from system frameworks.
        # Reject a local Homebrew/SDK dependency instead of shipping a broken bundle.
        dependencies = output(["otool", "-arch", arch, "-L", path]).splitlines()[1:]
        identity_seen = False
        for line in dependencies:
            if not line[:1].isspace():
                continue
            library = line.strip().split(" (", 1)[0]
            if file_type == "DYLIB" and not identity_seen:
                # otool -L lists LC_ID_DYLIB first for a dylib.
                identity_seen = True
                continue
            if not library.startswith(("/System/Library/", "/usr/lib/")):
                raise RuntimeError(f"Unbundled dynamic dependency in {path}: {library}")


def app_info(version):
    return {
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleName": "Babel",
        "CFBundleDisplayName": "Babel",
        "CFBundleExecutable": "babel-tray",
        "CFBundlePackageType": "APPL",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "CFBundleDevelopmentRegion": "en",
        "CFBundleLocalizations": ["en", "pt"],
        "LSMinimumSystemVersion": MINIMUM_MACOS,
        "LSUIElement": True,
        "NSHighResolutionCapable": True,
        "NSMicrophoneUsageDescription": "Babel uses your microphone to route audio, translate speech and listen for enabled voice commands.",
    }


def copy_file(source, destination, executable=False):
    regular_file(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    destination.chmod(0o755 if executable else 0o644)


def stage_bundle(destination, binaries, driver_directory, version):
    contents = destination / "Contents"
    resources = contents / "Resources"
    resources.mkdir(parents=True)
    for name in ["babel", "babel-tray"]:
        copy_file(binaries / name, contents / "MacOS" / name, executable=True)
    with (contents / "Info.plist").open("wb") as handle:
        plistlib.dump(app_info(version), handle)
    for language, text in {
        "en": app_info(version)["NSMicrophoneUsageDescription"],
        "pt": "O Babel usa seu microfone para rotear áudio, traduzir falas e ouvir comandos de voz habilitados.",
    }.items():
        localized = resources / (language + ".lproj")
        localized.mkdir()
        (localized / "InfoPlist.strings").write_text(
            '"NSMicrophoneUsageDescription" = "' + text + '";\n', encoding="utf-8")
    drivers = resources / "drivers" / "macos"
    copy_file(driver_directory / "BabelAudio.pkg", drivers / "BabelAudio.pkg")
    copy_file(driver_directory / "uninstall.sh", drivers / "uninstall.sh", executable=True)
    for name in ["README.md", "LICENSE-Apple-example.txt", "LICENSE-MIT.txt"]:
        copy_file(ROOT / "native" / "macos" / name, drivers / name)
    for source in sorted((ROOT / "docs").glob("*.md")):
        copy_file(source, resources / "Documentation" / source.name)
    copy_file(HERE / "INSTALLATION.txt", resources / "INSTALLATION.txt")
    # These are inert source/support files. Users copy them to a writable service
    # directory before explicitly installing helpers; the signed bundle is immutable.
    for relative in ["setup_whisper.py", "needle_bridge.py", "patches/whisper-dynamic-port.patch"]:
        copy_file(ROOT / "scripts" / relative, resources / "Support" / "scripts" / relative)
    return destination


def distribution(arches):
    tree = ET.Element("installer-gui-script", minSpecVersion="2")
    ET.SubElement(tree, "title").text = "Babel"
    ET.SubElement(tree, "options", customize="never", **{
        "require-scripts": "false", "hostArchitectures": ",".join(sorted(arches))})
    ET.SubElement(tree, "domains", enable_anywhere="false", enable_currentUserHome="false", enable_localSystem="true")
    versions = ET.SubElement(ET.SubElement(tree, "volume-check"), "allowed-os-versions")
    ET.SubElement(versions, "os-version", min=MINIMUM_MACOS)
    ET.SubElement(tree, "readme", file="INSTALLATION.txt", **{"mime-type": "text/plain"})
    outline = ET.SubElement(tree, "choices-outline")
    for choice, title, package, filename in [
        ("app", "Babel application", APP_PACKAGE_ID, "BabelApp.pkg"),
        ("driver", "Babel virtual audio driver", DRIVER_PACKAGE_ID, "BabelAudio.pkg"),
    ]:
        ET.SubElement(outline, "line", choice=choice)
        element = ET.SubElement(tree, "choice", id=choice, title=title, description=title,
                                visible="false", selected="true", enabled="false")
        ET.SubElement(element, "pkg-ref", id=package)
        reference = ET.SubElement(tree, "pkg-ref", id=package)
        reference.text = filename
        if choice == "driver":
            reference.set("onConclusion", "RequireRestart")
    closing = ET.SubElement(ET.SubElement(tree, "pkg-ref", id=APP_PACKAGE_ID), "must-close")
    ET.SubElement(closing, "app", id=BUNDLE_ID)
    ET.indent(tree)
    return ET.ElementTree(tree)


def validate_payload(component, expected_path, expected_id):
    """Inspect pkgutil --expand-full output before publishing any installer."""
    info_path = component / "PackageInfo"
    regular_file(info_path)
    info = ET.parse(info_path).getroot()
    if info.get("identifier") != expected_id or info.get("install-location") != "/":
        raise RuntimeError(f"Unexpected package identity or installation location: {info_path}")
    payload = component / "Payload"
    bundle = payload / expected_path
    expected_bundle_id = BUNDLE_ID if expected_id == APP_PACKAGE_ID else DRIVER_ID
    for path in payload.rglob("*"):
        relative = path.relative_to(payload)
        if path.is_symlink():
            raise RuntimeError(f"Unexpected symlink in installer payload: {relative}")
        if relative != expected_path and expected_path not in relative.parents and relative not in expected_path.parents:
            raise RuntimeError(f"Unexpected file outside the Babel bundle: {relative}")
        if relative in expected_path.parents and not path.is_dir():
            raise RuntimeError(f"Expected a payload directory: {relative}")
    with (bundle / "Contents" / "Info.plist").open("rb") as handle:
        if plistlib.load(handle).get("CFBundleIdentifier") != expected_bundle_id:
            raise RuntimeError(f"Incorrect bundle in package {expected_id}")
    for name in (["preinstall"] if expected_id == APP_PACKAGE_ID else ["preinstall", "postinstall"]):
        regular_file(component / "Scripts" / name, executable=True)
    return bundle


def expanded_components(directory):
    components = {}
    for path in directory.rglob("PackageInfo"):
        # A product contains two component archives. Nested installer resources
        # remain flat .pkg files, not expanded components.
        identifier = ET.parse(path).getroot().get("identifier")
        if identifier in components:
            raise RuntimeError(f"Duplicate component package: {identifier}")
        components[identifier] = path.parent
    if set(components) != {APP_PACKAGE_ID, DRIVER_PACKAGE_ID}:
        raise RuntimeError(f"Unexpected product components: {sorted(str(key) for key in components)}")
    return components


def verify_matching_files(first, second, relatives):
    for relative in relatives:
        regular_file(first / relative)
        regular_file(second / relative)
        if digest(first / relative) != digest(second / relative):
            raise RuntimeError(f"Package payload differs from its input: {relative}")


def check_output_destination(destination, bundle=False):
    if destination.is_symlink():
        raise RuntimeError(f"Refusing to replace output symlink: {destination}")
    if destination.exists() and bundle:
        regular_file(destination / "Contents" / "Info.plist")
        if (destination / "Contents").is_symlink():
            raise RuntimeError("Refusing an output bundle with a symlinked Contents directory")
        with (destination / "Contents" / "Info.plist").open("rb") as handle:
            if plistlib.load(handle).get("CFBundleIdentifier") != BUNDLE_ID:
                raise RuntimeError("The existing output app is not Babel; preserved")
    elif destination.exists() and not destination.is_file():
        raise RuntimeError(f"Expected an output file: {destination}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True, help="prebuilt executable babel and babel-tray")
    parser.add_argument("--driver-dir", type=Path, default=ROOT / "native/macos/dist")
    parser.add_argument("--output", type=Path, default=HERE / "dist")
    parser.add_argument("--version", type=validate_version, default=root_version())
    parser.add_argument("--arch", choices=ARCHES, default="universal")
    signing = parser.add_mutually_exclusive_group(required=True)
    signing.add_argument("--unsigned", action="store_true", help="explicit CI/development, ad-hoc app signature")
    signing.add_argument("--sign-identity", help="Developer ID Application identity already in the keychain")
    parser.add_argument("--installer-identity", help="Developer ID Installer identity (required for signed release)")
    parser.add_argument("--notary-profile", help="explicit notarytool profile; submit final package and staple")
    args = parser.parse_args(argv)
    if args.unsigned and (args.installer_identity or args.notary_profile):
        parser.error("Unsigned development builds cannot use installer signing or notarization")
    if not args.unsigned and not args.installer_identity:
        parser.error("Signed distribution requires both --sign-identity and --installer-identity")
    if sys.platform != "darwin":
        parser.error("Packaging requires macOS; portable script tests can run on other hosts")
    for command in ["lipo", "otool", "codesign", "pkgbuild", "productbuild", "pkgutil", "xcrun"]:
        if shutil.which(command) is None:
            parser.error(f"Missing macOS packaging tool: {command}")
    binaries, driver_directory = args.bin_dir.resolve(), args.driver_dir.resolve()
    arches = ARCHES[args.arch]
    for name in ["babel", "babel-tray"]:
        validate_macho(binaries / name, arches, "EXECUTE")
    regular_file(driver_directory / "BabelAudio.pkg")
    regular_file(driver_directory / "uninstall.sh", executable=True)
    driver = driver_directory / "BabelAudio.driver"
    validate_macho(driver / "Contents/MacOS/BabelAudio", arches, "DYLIB")
    run(["codesign", "--verify", "--strict", driver])
    if not args.unsigned:
        package_signature = output(["pkgutil", "--check-signature", driver_directory / "BabelAudio.pkg"])
        if "Developer ID Installer:" not in package_signature:
            raise RuntimeError("Signed distribution requires a Developer ID Installer signed driver package")
        identity = subprocess.run(["codesign", "-dv", "--verbose=4", str(driver)],
                                  check=True, capture_output=True, text=True).stderr
        if "Authority=Developer ID Application:" not in identity or "Signature=adhoc" in identity:
            raise RuntimeError("Signed distribution requires a Developer ID signed driver bundle")
    output_directory = args.output.resolve()
    output_directory.mkdir(parents=True, exist_ok=True)
    package_name = f"Babel-{args.version}-macos-{args.arch}.pkg"
    final_app = output_directory / "Babel.app"
    final_package = output_directory / package_name
    checksums = output_directory / "SHA256SUMS.txt"
    manifest = output_directory / "manifest.json"
    for destination in [final_package, checksums, manifest]:
        check_output_destination(destination)
    check_output_destination(final_app, bundle=True)
    with tempfile.TemporaryDirectory(prefix=".babel-package-", dir=output_directory) as temporary:
        stage = Path(temporary)
        expanded_driver = stage / "driver-expanded"
        run(["pkgutil", "--expand-full", driver_directory / "BabelAudio.pkg", expanded_driver])
        packaged_driver = validate_payload(expanded_driver, DRIVER_PATH, DRIVER_PACKAGE_ID)
        verify_matching_files(driver, packaged_driver, ["Contents/Info.plist", "Contents/MacOS/BabelAudio"])
        payload = stage / "payload"
        app = stage_bundle(payload / APP_PATH, binaries, driver_directory, args.version)
        entitlements = stage / "entitlements.plist"
        with entitlements.open("wb") as handle:
            plistlib.dump({"com.apple.security.device.audio-input": True}, handle)
        sign = ["codesign", "--force", "--sign", "-" if args.unsigned else args.sign_identity]
        if not args.unsigned:
            sign += ["--options", "runtime", "--timestamp"]
        run([*sign, "--entitlements", entitlements, app / "Contents/MacOS/babel"])
        run([*sign, "--entitlements", entitlements, app])
        run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", app])
        components = stage / "components"
        components.mkdir()
        component_list = stage / "components.plist"
        run(["pkgbuild", "--analyze", "--root", payload, component_list])
        with component_list.open("rb") as handle:
            entries = plistlib.load(handle)
        for entry in entries:
            entry.update(BundleIsRelocatable=False, BundleHasStrictIdentifier=True, BundleOverwriteAction="upgrade")
        with component_list.open("wb") as handle:
            plistlib.dump(entries, handle)
        run(["pkgbuild", "--root", payload, "--component-plist", component_list,
             "--identifier", APP_PACKAGE_ID, "--version", args.version,
             "--install-location", "/", "--ownership", "recommended",
             "--scripts", HERE / "installer", components / "BabelApp.pkg"])
        shutil.copy2(driver_directory / "BabelAudio.pkg", components / "BabelAudio.pkg")
        definition = stage / "Distribution.xml"
        distribution(arches).write(definition, encoding="utf-8", xml_declaration=True)
        installer_resources = stage / "installer-resources"
        copy_file(HERE / "INSTALLATION.txt", installer_resources / "INSTALLATION.txt")
        staged_package = stage / package_name
        command = ["productbuild", "--distribution", definition, "--package-path", components,
                   "--resources", installer_resources]
        if args.installer_identity:
            command += ["--sign", args.installer_identity]
        run([*command, staged_package])
        expanded = stage / "product-expanded"
        run(["pkgutil", "--expand-full", staged_package, expanded])
        packages = expanded_components(expanded)
        installed_app = validate_payload(packages[APP_PACKAGE_ID], APP_PATH, APP_PACKAGE_ID)
        installed_driver = validate_payload(packages[DRIVER_PACKAGE_ID], DRIVER_PATH, DRIVER_PACKAGE_ID)
        verify_matching_files(app, installed_app, ["Contents/Info.plist", "Contents/MacOS/babel",
            "Contents/MacOS/babel-tray", "Contents/Resources/drivers/macos/BabelAudio.pkg",
            "Contents/Resources/drivers/macos/uninstall.sh"])
        verify_matching_files(driver, installed_driver, ["Contents/Info.plist", "Contents/MacOS/BabelAudio"])
        verify_matching_files(expanded_driver, packages[DRIVER_PACKAGE_ID], ["Scripts/preinstall", "Scripts/postinstall"])
        if args.installer_identity:
            run(["pkgutil", "--check-signature", staged_package])
        if args.notary_profile:
            run(["xcrun", "notarytool", "submit", staged_package, "--keychain-profile", args.notary_profile, "--wait"])
            run(["xcrun", "stapler", "staple", staged_package])
            run(["xcrun", "stapler", "validate", staged_package])
        if final_app.exists():
            shutil.rmtree(final_app)
        shutil.move(str(app), str(final_app))
        os.replace(staged_package, final_package)
        checksums.write_text(f"{digest(final_package)}  {package_name}\n", encoding="utf-8")
        manifest.write_text(json.dumps({
            "version": args.version, "architectures": sorted(arches),
            "minimum_macos": MINIMUM_MACOS, "bundle_identifier": BUNDLE_ID,
            "package": package_name, "sha256": digest(final_package),
            "driver_package_sha256": digest(driver_directory / "BabelAudio.pkg"),
            "development_unsigned": args.unsigned, "notarized": bool(args.notary_profile),
            "payload_verified": True, "installed_on_build_host": False,
        }, indent=2) + "\n", encoding="utf-8")
    print(f"App: {final_app}")
    print(f"Installer (not installed): {final_package}")
    print("Development artifact: ad-hoc signed app, unsigned installer." if args.unsigned
          else "Developer ID signed installer. Notarization completed." if args.notary_profile
          else "Developer ID signed installer. Notarization was not requested.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, ET.ParseError, subprocess.CalledProcessError) as error:
        print(f"Babel macOS packaging failed: {error}", file=sys.stderr)
        sys.exit(1)
