# Windows installer

The release workflow runs this packager on `windows-2022`, with Python 3.12,
Inno Setup 6.3+ (included in the runner image), and previously built binaries.

```powershell
python packaging/windows/build.py --architecture x64 `
  --runtime-dir artifacts/local-runtime `
  --bin-dir target/x86_64-pc-windows-msvc/release `
  --output artifacts/installers
```

For ARM64, use `--architecture ARM64` and Rust target `aarch64-pc-windows-msvc`.
`--iscc` selects the Inno Setup compiler path.
`--version` accepts three numeric components and defaults to `Cargo.toml`.

Outputs are `Babel-<version>-windows-<architecture>-development.exe`, `.zip`,
`-manifest.json`, and `-SHA256SUMS.txt`. The output directory must not already
contain packages with the same names. The default packages include the app and
local inference engines. They use separately installed audio devices. See
[Windows routing](../../docs/platforms.md#windows). The packages are unsigned,
so SmartScreen can warn and execution policy can block them.

The application uses `%APPDATA%\Babel\babel.toml`. Autostart is disabled
by default. The installer leaves the app closed and preserves user files.

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
and run `--version`. The script runs only inside GitHub Actions. ARM64 application binaries are inspected and
compiled on x64; ARM64 inference engines compile and run `--help` on a native
Windows ARM64 runner.
