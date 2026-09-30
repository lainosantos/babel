#!/usr/bin/env python3
"""Build a complete development Windows installer with Inno Setup 6.3+.

Stages an explicit file allowlist, verifies native PE architecture for every
executable/driver, produces an installer and portable ZIP, and hashes outputs.
Never executes the installer, app or driver. Python 3.11+ is required.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "scripts"))
import local_runtime_packaging as runtime_package
MACHINES = {"x64": 0x8664, "ARM64": 0xAA64}
DRIVER_FILES = ("BabelAudio.inf", "BabelAudio.sys", "BabelAudio.cat",
                "babel-driver-installer.exe", "install.ps1", "uninstall.ps1",
                "LICENSE-Microsoft.txt", "LICENSE-MIT.txt", "README.md",
                "toolchain.json", "SHA256SUMS")


def version(value=None):
    value = value or tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", value) or any(int(v) > 65535 for v in value.split('.')):
        raise ValueError("Installer version must have three numeric components <= 65535")
    return value


def require_file(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
        raise ValueError(f"Missing, empty or symlinked package input: {path}")


def pe_machine(path):
    require_file(path)
    with path.open('rb') as handle:
        header = handle.read(64)
        if len(header) != 64 or header[:2] != b'MZ':
            raise ValueError(f"Not a Windows executable: {path}")
        offset = struct.unpack_from('<I', header, 60)[0]
        if offset < 64 or offset > path.stat().st_size - 6:
            raise ValueError(f"Invalid PE offset: {path}")
        handle.seek(offset)
        signature = handle.read(6)
        if signature[:4] != b'PE\0\0':
            raise ValueError(f"Invalid PE signature: {path}")
        return struct.unpack_from('<H', signature, 4)[0]


def stage(binary, driver, destination, architecture, release, runtime_dir=None):
    for name in ('babel.exe', 'babel-tray.exe'):
        if pe_machine(binary / name) != MACHINES[architecture]:
            raise ValueError(f"Wrong {architecture} app architecture: {name}")
    for name in DRIVER_FILES:
        require_file(driver / name)
    for name in ('BabelAudio.sys', 'babel-driver-installer.exe'):
        if pe_machine(driver / name) != MACHINES[architecture]:
            raise ValueError(f"Wrong {architecture} driver architecture: {name}")
    inf_bytes = (driver / 'BabelAudio.inf').read_bytes()
    inf = inf_bytes.decode('utf-16') if inf_bytes[:2] in (b'\xff\xfe', b'\xfe\xff') else inf_bytes.decode('utf-8-sig')
    suffix = 'amd64' if architecture == 'x64' else 'arm64'
    for token in ('ROOT\\BabelAudio', 'BabelAudio.sys', 'BabelAudio.cat', f'NT{suffix}'):
        if token.casefold() not in inf.casefold():
            raise ValueError(f"INF is incomplete or has the wrong architecture: {token}")
    if '$ARCH$' in inf:
        raise ValueError('INF was not processed by the WDK stamp task')
    destination.mkdir(parents=True, exist_ok=False)
    for name in ('babel.exe', 'babel-tray.exe'):
        shutil.copy2(binary / name, destination / name)
    driver_out = destination / 'drivers' / 'windows'
    driver_out.mkdir(parents=True)
    for name in DRIVER_FILES:
        shutil.copy2(driver / name, driver_out / name)
    shutil.copy2(HERE / 'INSTALLATION.txt', destination / 'INSTALLATION.txt')
    shutil.copy2(ROOT / 'native/windows/LICENSE-MIT.txt', destination / 'LICENSE-MIT.txt')
    shutil.copy2(ROOT / 'Cargo.lock', destination / 'Cargo.lock')
    shutil.copy2(ROOT / 'README.md', destination / 'README.md')
    docs = destination / 'docs'
    docs.mkdir()
    for source in sorted((ROOT / 'docs').glob('*.md')):
        require_file(source)
        shutil.copy2(source, docs / source.name)
    fonts = destination / 'licenses'
    fonts.mkdir()
    shutil.copy2(ROOT / 'ui/fonts/OFL.txt', fonts / 'Manrope-OFL.txt')
    if runtime_dir is not None:
        runtime_package.stage(runtime_dir, destination / "local-runtime", "windows", ["x86_64" if architecture == "x64" else "aarch64"])
    manifest = {"version": release, "architecture": architecture, "driver_signature": "not-verified-development",
                "files": {p.relative_to(destination).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
                          for p in sorted(destination.rglob('*')) if p.is_file()}}
    (destination / 'package-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    return manifest


def build(args):
    release = version(args.version)
    if sys.platform != 'win32':
        raise RuntimeError('Inno Setup installer builds require a Windows runner')
    compiler = args.iscc or shutil.which('ISCC.exe')
    if not compiler:
        candidate = Path(os.environ.get('ProgramFiles(x86)', r'C:\Program Files (x86)')) / 'Inno Setup 6/ISCC.exe'
        if candidate.is_file():
            compiler = str(candidate)
    if not compiler:
        raise RuntimeError('Inno Setup 6.3+ is required (available in the windows-2022 GitHub image)')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    base = f'Babel-{release}-windows-{args.architecture}-development'
    installer = output / (base + '.exe')
    archive = output / (base + '.zip')
    checksum = output / (base + '-SHA256SUMS.txt')
    report = output / (base + '-manifest.json')
    if any(p.exists() for p in (installer, archive, checksum, report)):
        raise RuntimeError('Package output already exists; use an empty output directory')
    with tempfile.TemporaryDirectory(prefix='babel-installer-') as temp:
        payload = Path(temp) / 'payload'
        manifest = stage(args.bin_dir.resolve(), args.driver_dir.resolve(), payload, args.architecture, release, args.runtime_dir)
        subprocess.run([str(compiler), f'/DBabelVersion={release}', f'/DTargetArch={args.architecture}',
                        f'/DPayloadDir={payload}', f'/DOutputFolder={output}', str(HERE / 'Babel.iss')], check=True)
        require_file(installer)
        pe_machine(installer)  # setup is an x86 bootstrapper; payload has the native architecture
        with zipfile.ZipFile(archive, 'x', compression=zipfile.ZIP_DEFLATED) as bundle:
            for p in sorted(payload.rglob('*')):
                if p.is_file():
                    bundle.write(p, base + '/' + p.relative_to(payload).as_posix())
        with zipfile.ZipFile(archive) as bundle:
            if bundle.testzip() is not None:
                raise RuntimeError('Corrupted portable ZIP')
            for path, digest in manifest['files'].items():
                if hashlib.sha256(bundle.read(base + '/' + path)).hexdigest() != digest:
                    raise RuntimeError(f'ZIP payload mismatch: {path}')
        report.write_text(json.dumps(manifest, indent=2) + '\n')
    checksum.write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n'
                               for p in (installer, archive, report)))
    print(f'Installer and complete portable package verified: {output}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', type=Path, required=True)
    parser.add_argument('--bin-dir', type=Path, required=True)
    parser.add_argument('--driver-dir', type=Path, required=True)
    parser.add_argument('--architecture', choices=list(MACHINES), required=True)
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/installers')
    parser.add_argument('--version')
    parser.add_argument('--iscc')
    try:
        build(parser.parse_args())
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'Windows packaging failed: {error}\n')
