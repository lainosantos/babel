#!/usr/bin/env python3
"""Package prebuilt Babel executables for macOS.

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
sys.path.insert(0, str(ROOT / "scripts"))
import local_runtime_packaging as runtime_package

BUNDLE_ID = "org.babel.audio"
APP_PACKAGE_ID = BUNDLE_ID + ".app.pkg"
MINIMUM_MACOS = "14.2"
ARCHES = {"universal": {"arm64", "x86_64"}, "arm64": {"arm64"}, "x86_64": {"x86_64"}}
APP_PATH = Path("Applications/Babel.app")


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


def validate_macho(path, arches):
    regular_file(path, executable=True)
    actual = set(output(["lipo", "-archs", path]).split())
    if actual != arches:
        raise RuntimeError(f"{path}: expected architectures {sorted(arches)}, found {sorted(actual)}")
    for arch in sorted(arches):
        header = output(["otool", "-arch", arch, "-hv", path])
        if not re.search(r"\bEXECUTE\b", header):
            raise RuntimeError(f"{path} ({arch}) is not a Mach-O EXECUTE")
        # Babel's release executables are self-contained apart from system frameworks.
        # Reject a local Homebrew/SDK dependency instead of shipping a broken bundle.
        dependencies = output(["otool", "-arch", arch, "-L", path]).splitlines()[1:]
        for line in dependencies:
            if not line[:1].isspace():
                continue
            library = line.strip().split(" (", 1)[0]
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
        "NSMicrophoneUsageDescription": "Babel uses your microphone to route audio, translate and transcribe speech, and record sessions.",
    }


def copy_file(source, destination, executable=False):
    regular_file(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    destination.chmod(0o755 if executable else 0o644)


def stage_bundle(destination, binaries, version, runtime_dir=None, arches=None):
    contents = destination / "Contents"
    resources = contents / "Resources"
    resources.mkdir(parents=True)
    for name in ["babel", "babel-tray"]:
        copy_file(binaries / name, contents / "MacOS" / name, executable=True)
    with (contents / "Info.plist").open("wb") as handle:
        plistlib.dump(app_info(version), handle)
    for language in app_info(version)["CFBundleLocalizations"]:
        relative = Path(language + ".lproj") / "InfoPlist.strings"
        copy_file(HERE / "locales" / relative, resources / relative)
    for source in sorted((ROOT / "docs").glob("*.md")):
        copy_file(source, resources / "Documentation" / source.name)
    copy_file(HERE / "INSTALLATION.txt", resources / "INSTALLATION.txt")
    copy_file(ROOT / "ui/fonts/OFL.txt", resources / "Licenses/Manrope-OFL.txt")
    if runtime_dir is not None:
        runtime_package.stage(runtime_dir, resources / "local-runtime", "macos", ["aarch64" if arch == "arm64" else arch for arch in sorted(arches)])
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
    ET.SubElement(outline, "line", choice="app")
    choice = ET.SubElement(tree, "choice", id="app", title="Babel application",
                           description="Babel application", visible="false",
                           selected="true", enabled="false")
    ET.SubElement(choice, "pkg-ref", id=APP_PACKAGE_ID)
    ET.SubElement(tree, "pkg-ref", id=APP_PACKAGE_ID).text = "BabelApp.pkg"
    closing = ET.SubElement(ET.SubElement(tree, "pkg-ref", id=APP_PACKAGE_ID), "must-close")
    ET.SubElement(closing, "app", id=BUNDLE_ID)
    ET.indent(tree)
    return ET.ElementTree(tree)


def validate_payload(component):
    """Inspect pkgutil --expand-full output before publishing any installer."""
    info_path = component / "PackageInfo"
    regular_file(info_path)
    info = ET.parse(info_path).getroot()
    if info.get("identifier") != APP_PACKAGE_ID or info.get("install-location") != "/":
        raise RuntimeError(f"Unexpected package identity or installation location: {info_path}")
    payload = component / "Payload"
    bundle = payload / APP_PATH
    for path in payload.rglob("*"):
        relative = path.relative_to(payload)
        if path.is_symlink():
            raise RuntimeError(f"Unexpected symlink in installer payload: {relative}")
        if relative != APP_PATH and APP_PATH not in relative.parents and relative not in APP_PATH.parents:
            raise RuntimeError(f"Unexpected file outside the Babel bundle: {relative}")
        if relative in APP_PATH.parents and not path.is_dir():
            raise RuntimeError(f"Expected a payload directory: {relative}")
    with (bundle / "Contents" / "Info.plist").open("rb") as handle:
        if plistlib.load(handle).get("CFBundleIdentifier") != BUNDLE_ID:
            raise RuntimeError(f"Incorrect bundle in package {APP_PACKAGE_ID}")
    regular_file(component / "Scripts/preinstall", executable=True)
    return bundle


def expanded_components(directory):
    components = {}
    for path in directory.rglob("PackageInfo"):
        # Nested installer resources remain flat .pkg files.
        identifier = ET.parse(path).getroot().get("identifier")
        if identifier in components:
            raise RuntimeError(f"Duplicate component package: {identifier}")
        components[identifier] = path.parent
    if set(components) != {APP_PACKAGE_ID}:
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
    parser.add_argument("--runtime-dir", type=Path, required=True)
    parser.add_argument("--bin-dir", type=Path, required=True, help="prebuilt executable babel and babel-tray")
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
    binaries = args.bin_dir.resolve()
    arches = ARCHES[args.arch]
    for name in ["babel", "babel-tray"]:
        validate_macho(binaries / name, arches)
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
        payload = stage / "payload"
        app = stage_bundle(payload / APP_PATH, binaries, args.version, args.runtime_dir, arches)
        entitlements = stage / "entitlements.plist"
        with entitlements.open("wb") as handle:
            plistlib.dump({"com.apple.security.device.audio-input": True}, handle)
        sign = ["codesign", "--force", "--sign", "-" if args.unsigned else args.sign_identity]
        if not args.unsigned:
            sign += ["--options", "runtime", "--timestamp"]
        # Each platform payload is thin; keep Intel and ARM providers separate.
        # Re-sign nested executables/dylibs before the immutable app signature.
        for runtime in sorted((app / "Contents/Resources/local-runtime").iterdir()):
            for binary in sorted(runtime.rglob("*")):
                if not binary.is_file():
                    continue
                with binary.open("rb") as stream:
                    macho = stream.read(4) == b"\xcf\xfa\xed\xfe"
                if macho:
                    run([*sign, binary])
            runtime_package.rehash(runtime)
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
        installed_app = validate_payload(packages[APP_PACKAGE_ID])
        verify_matching_files(app, installed_app, ["Contents/Info.plist", "Contents/MacOS/babel",
            "Contents/MacOS/babel-tray"])
        for runtime in (installed_app / "Contents/Resources/local-runtime").iterdir():
            arch = runtime.name.removeprefix("macos-")
            runtime_package.validate(runtime, "macos", arch)
        if (installed_app / "Contents/Resources/drivers").exists():
            raise RuntimeError("App-only installer contains a driver")
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
