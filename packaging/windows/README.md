# Windows installer

The release workflow runs this packager on `windows-2022`, with Python 3.12,
Inno Setup 6.3+ (included in the runner image), and previously built binaries.
WDK setup is documented in [native/windows](../../native/windows/README.md).

```powershell
python packaging/windows/build.py --architecture x64 `
  --runtime-dir artifacts/local-runtime `
  --bin-dir target/x86_64-pc-windows-msvc/release `
  --driver-dir native/windows/dist/x64 --output artifacts/installers
```

For ARM64, use `--architecture ARM64`, target `aarch64-pc-windows-msvc`, and
`dist/ARM64` for the driver. `--iscc` selects the Inno Setup compiler path.
`--version` accepts three numeric components and defaults to `Cargo.toml`.

Outputs are `Babel-<version>-windows-<architecture>-development.exe`, `.zip`,
`-manifest.json`, and `-SHA256SUMS.txt`. The output directory must not already
contain packages with the same names. The ZIP preserves the application's
expected layout, including `drivers/windows/{BabelAudio.inf,BabelAudio.sys,BabelAudio.cat,...}`.
Do not mix components from different builds. PE checks verify the architecture
of both application executables, the helper, and the driver before packaging.

These are development artifacts without distribution signing. A CAT file's
presence does not mean it is signed. The installer makes the driver available
for an explicit administrative step; it does not try to load an unsigned kernel
driver. See the text displayed by the installer in
[INSTALLATION.txt](INSTALLATION.txt).

The application uses `%APPDATA%\Babel\babel.toml`, does not write to Program Files,
and does not start during setup. Autostart is disabled by default. Before
uninstalling, the uninstaller calls the helper's read-only `check-absent`: if
the driver is installed, remove it with `uninstall.ps1` first. This avoids
removing a driver before uninstaller confirmation or deleting its required helper.

Local engines ship in `local-runtime/windows-x86_64` or
`local-runtime/windows-aarch64`: Whisper, llama.cpp, Piper, ONNX Runtime, eSpeak
data, and private C++ runtime DLLs. Users do not separately install Python,
Ollama, Piper, or the VC++ redistributable. The first selection can download
Babel-verified model weights; engine binaries are already in the installer.
The manifest validates all hashes, executables, and DLL architectures.
Corresponding source for the GPL Piper/eSpeak processes and their patches ships
in `sources/`, with licenses in `licenses/`.

Release builds compile these engines on native Windows x64 and ARM64 runners
before assembling installers. To reproduce this, run
`python scripts/build_local_runtime.py --output artifacts/local-runtime`
on the desired architecture with Python 3.11+, Git, CMake 3.26+, and MSVC.
These are development requirements, not installed-application dependencies.

Portable assembly/validation tests:

```sh
python3 -m unittest discover -s packaging/windows -p 'test_*.py' -v
```

The x64 release job uses `.github/scripts/test-windows-package.ps1` to install
and remove the **application** in a disposable runner, compare payload hashes,
and run `--version`. The script refuses to run outside GitHub Actions and does
not install the driver. ARM64 application and driver binaries are inspected and
compiled on x64; ARM64 inference engines compile and run `--help` on a native
Windows ARM64 runner.
