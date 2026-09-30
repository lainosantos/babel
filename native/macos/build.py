#!/usr/bin/env python3
"""Build/sign BabelAudio.driver using the real macOS SDK; never install or elevate.

Example CI: python3 native/macos/build.py --unsigned --arch universal --test --pkg
Install Rust targets beforehand: rustup target add aarch64-apple-darwin x86_64-apple-darwin
"""
import argparse
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent
VERSION = "1.0.0"
BUNDLE_ID = "org.babel.audio.driver"
FACTORY_ID = "D4189035-7C66-4D8D-9F21-7E378334542F"
TYPE_ID = "443ABAB8-E7B3-491A-B985-BEB9187030DB"
TARGETS = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}


def run(args, **kwargs):
    print("+", " ".join(str(arg) for arg in args), flush=True)
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def replace_owned_bundle(source, destination):
    if destination.is_symlink():
        raise RuntimeError(f"Refusing to replace a symlink: {destination}")
    if destination.exists():
        info = destination / "Contents" / "Info.plist"
        if info.is_symlink() or (destination / "Contents").is_symlink():
            raise RuntimeError("Unexpected symlink in the previous output bundle")
        with info.open("rb") as handle:
            if plistlib.load(handle).get("CFBundleIdentifier") != BUNDLE_ID:
                raise RuntimeError("Existing output bundle is not BabelAudio; preserved")
        shutil.rmtree(destination)
    shutil.move(str(source), str(destination))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    signing = parser.add_mutually_exclusive_group(required=True)
    signing.add_argument("--unsigned", action="store_true", help="explicit development/CI ad-hoc signature")
    signing.add_argument("--sign-identity", help="Developer ID Application identity already in the keychain")
    parser.add_argument("--arch", choices=["native", "arm64", "x86_64", "universal"], default="native")
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    parser.add_argument("--test", action="store_true", help="run the SDK/CFPlugIn contract in an isolated process")
    parser.add_argument("--pkg", action="store_true", help="build installer .pkg (requires admin only when later installed)")
    parser.add_argument("--installer-identity", help="Developer ID Installer identity for the .pkg")
    parser.add_argument("--notary-profile", help="explicit notarytool keychain profile; submits signed .pkg and staples it")
    args = parser.parse_args(argv)
    if sys.platform != "darwin":
        parser.error("The driver bundle requires macOS and its SDK; run cargo test on other hosts for the portable core")
    if args.installer_identity and not args.pkg:
        parser.error("--installer-identity requires --pkg")
    if args.notary_profile and (not args.pkg or args.unsigned or not args.installer_identity):
        parser.error("--notary-profile requires --pkg, --sign-identity and --installer-identity")
    for command in ["cargo", "rustup", "xcrun", "codesign", "lipo"]:
        if shutil.which(command) is None:
            parser.error(f"Missing build requirement: {command}")
    native = "arm64" if platform.machine() == "arm64" else "x86_64"
    arches = list(TARGETS) if args.arch == "universal" else [native if args.arch == "native" else args.arch]
    if args.test and native not in arches:
        parser.error("--test requires a build containing the current Mac architecture")
    installed = subprocess.check_output(["rustup", "target", "list", "--installed"], text=True).splitlines()
    missing = [TARGETS[arch] for arch in arches if TARGETS[arch] not in installed]
    if missing:
        parser.error("Install required Rust targets explicitly: rustup target add " + " ".join(missing))
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["MACOSX_DEPLOYMENT_TARGET"] = "11.0"
    environment["CARGO_INCREMENTAL"] = "0"
    # Ignore a caller's unrelated target directory: packaged paths are deterministic.
    environment["CARGO_TARGET_DIR"] = str(ROOT / "target")
    libraries = []
    for arch in arches:
        target = TARGETS[arch]
        run(["cargo", "build", "--manifest-path", ROOT / "Cargo.toml", "--locked",
             "--release", "--package", "babel-hal-driver", "--target", target], env=environment)
        libraries.append(ROOT / "target" / target / "release" / "libbabel_hal_driver.dylib")
    with tempfile.TemporaryDirectory(prefix=".babel-build-", dir=output) as temporary:
        staging = Path(temporary)
        bundle = staging / "BabelAudio.driver"
        macos = bundle / "Contents" / "MacOS"
        resources = bundle / "Contents" / "Resources"
        macos.mkdir(parents=True)
        resources.mkdir()
        executable = macos / "BabelAudio"
        if len(libraries) > 1:
            run(["lipo", "-create", *libraries, "-output", executable])
        else:
            shutil.copy2(libraries[0], executable)
        executable.chmod(0o755)
        info = {"CFBundleIdentifier": BUNDLE_ID, "CFBundleExecutable": "BabelAudio",
                "CFBundleName": "Babel Audio", "CFBundlePackageType": "BNDL",
                "CFBundleInfoDictionaryVersion": "6.0", "CFBundleShortVersionString": VERSION,
                "CFBundleVersion": VERSION, "CFBundleDevelopmentRegion": "en",
                "LSMinimumSystemVersion": "11.0", "CFPlugInDynamicRegistration": False,
                "CFPlugInFactories": {FACTORY_ID: "BabelCreate"},
                "CFPlugInTypes": {TYPE_ID: [FACTORY_ID]}}
        with (bundle / "Contents" / "Info.plist").open("wb") as handle:
            plistlib.dump(info, handle)
        shutil.copy2(ROOT / "README.md", resources / "README.md")
        shutil.copy2(ROOT / "LICENSE-Apple-example.txt", resources / "LICENSE-Apple-example.txt")
        shutil.copy2(ROOT / "LICENSE-MIT.txt", resources / "LICENSE-MIT.txt")
        sign = ["codesign", "--force", "--sign", "-" if args.unsigned else args.sign_identity]
        if not args.unsigned:
            sign += ["--options", "runtime", "--timestamp"]
        run([*sign, bundle])
        run(["codesign", "--verify", "--strict", "--verbose=2", bundle])
        # Dynamic exports must survive Rust LTO/stripping for CFPlugIn loading.
        symbols = subprocess.check_output(["nm", "-gU", executable], text=True)
        if "_BabelCreate" not in symbols:
            raise RuntimeError("The CFPlugIn factory export was stripped from the bundle")
        if args.test:
            contract = staging / "hal-contract"
            run(["xcrun", "--sdk", "macosx", "clang", "-std=c11", "-Wall", "-Wextra",
                 "-Werror", "-Wno-unused-parameter", ROOT / "tests" / "hal_contract.c",
                 "-framework", "CoreAudio", "-framework", "CoreFoundation", "-o", contract])
            run([contract, bundle])
        final_bundle = output / bundle.name
        replace_owned_bundle(bundle, final_bundle)
        if args.pkg:
            unsigned_pkg = staging / "BabelAudio-unsigned.pkg"
            payload = staging / "payload"
            hal = payload / "Library" / "Audio" / "Plug-Ins" / "HAL"
            hal.mkdir(parents=True)
            shutil.copytree(final_bundle, hal / final_bundle.name)
            components = staging / "components.plist"
            run(["pkgbuild", "--analyze", "--root", payload, components])
            with components.open("rb") as handle:
                entries = plistlib.load(handle)
            for entry in entries:
                entry["BundleIsRelocatable"] = False
                entry["BundleOverwriteAction"] = "upgrade"
                entry["BundleHasStrictIdentifier"] = True
            with components.open("wb") as handle:
                plistlib.dump(entries, handle)
            run(["pkgbuild", "--root", payload, "--component-plist", components,
                 "--identifier", BUNDLE_ID + ".pkg", "--version", VERSION,
                 "--install-location", "/", "--ownership", "recommended",
                 "--scripts", ROOT / "installer", unsigned_pkg])
            final_pkg = output / f"BabelAudio-{VERSION}.pkg"
            if final_pkg.is_symlink():
                raise RuntimeError("Refusing to replace a package symlink")
            if args.installer_identity:
                run(["productsign", "--sign", args.installer_identity, unsigned_pkg, final_pkg])
            else:
                shutil.copy2(unsigned_pkg, final_pkg)
            if args.notary_profile:
                run(["xcrun", "notarytool", "submit", final_pkg, "--keychain-profile", args.notary_profile, "--wait"])
                run(["xcrun", "stapler", "staple", final_pkg])
                run(["xcrun", "stapler", "validate", final_pkg])
            alias = output / "BabelAudio.pkg"
            if alias.is_symlink():
                raise RuntimeError("Refusing to replace a package symlink")
            shutil.copy2(final_pkg, alias)
            uninstaller = output / "uninstall.sh"
            if uninstaller.is_symlink():
                raise RuntimeError("Refusing to replace an uninstaller symlink")
            shutil.copy2(ROOT / "uninstall.sh", uninstaller)
            uninstaller.chmod(0o755)
            print(f"Installer (not installed): {final_pkg}")
    print(f"Driver bundle (not installed): {output / 'BabelAudio.driver'}")
    print("Installation needs an explicit administrator action followed by a macOS restart.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Babel HAL build failed: {error}", file=sys.stderr)
        sys.exit(1)
