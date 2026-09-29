#!/usr/bin/env python3
"""Exercise a native Babel binary without opening audio, UI, network or user config.

Only --help, --version and init are invoked, with an explicit temporary config.
Requires Python 3.11+; writes a report suitable for CI artifacts on all three OSes.
"""

import argparse
import json
import platform
from pathlib import Path
import subprocess
import tempfile
import tomllib


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def smoke(binary, report):
    require(binary.is_file(), f"Babel executable does not exist: {binary}")
    with tempfile.TemporaryDirectory(prefix="babel-cli-smoke-") as directory:
        working = Path(directory)
        config = working / "config with spaces" / "sessão.toml"

        def run(label, arguments, success=True):
            result = subprocess.run(
                [str(binary), "--config", str(config), *arguments],
                cwd=working,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=20,
                check=False,
            )
            report["commands"].append({
                "name": label,
                "arguments": arguments,
                "exit_code": result.returncode,
                "stdout": result.stdout,
                "stderr": result.stderr,
            })
            require((result.returncode == 0) == success, f"Unexpected exit code for {label}: {result.returncode}")
            return result

        help_result = run("help", ["--help"])
        require(all(command in help_result.stdout for command in ["serve", "run", "init", "devices", "doctor", "setup", "uninstall"]), "CLI help omits a documented command")
        version = run("version", ["--version"])
        require(version.stdout.strip().startswith("babel "), "CLI version does not identify Babel")
        require(not config.exists(), "Help/version unexpectedly created a configuration")

        run("init", ["init"])
        data = tomllib.loads(config.read_text(encoding="utf-8"))
        require(data["version"] == 1, "Unexpected config schema")
        require(data["interface"]["language"] == "system", "Default language does not follow the host")
        require(data["agent"]["whisper_endpoint"] == "auto" and data["agent"]["needle_endpoint"] == "auto", "Voice services must discover their bound ports instead of assuming defaults")
        base = Path(data["files"]["base_path"])
        require(base.is_absolute(), "Default storage base is not absolute")
        require(base == Path.home() / "Babel", "Default storage base does not use the user's Babel folder")
        require(data["microphone"]["capture_device"] == "", "Init selected a physical microphone")
        require(data["speaker"]["playback_device"] == "", "Init selected physical playback")
        virtual_endpoints = [data["microphone"]["playback_device"], data["speaker"]["capture_device"]]
        expected = ["babel_mic_bus", "babel_speaker.monitor"] if platform.system() == "Linux" else ["", ""]
        require(virtual_endpoints == expected, "Default virtual endpoints do not match the native platform")
        require(not data["recording"]["enabled"] and not data["transcription"]["enabled"], "Init unexpectedly enabled recording or transcription")
        require(not (working / "babel.toml").exists(), "Init ignored the explicit config path")

        edited = config.read_bytes() + b"\n# Preserve this user edit.\n"
        config.write_bytes(edited)
        run("refuse-overwrite", ["init"], success=False)
        require(config.read_bytes() == edited, "Repeated init changed an existing configuration")
        report["checks"] = [
            "native executable help/version",
            "temporary config with spaces and Unicode",
            "platform-specific defaults without physical devices",
            "absolute storage base independent of the launch directory",
            "automatic local voice endpoints without default port assumptions",
            "recording and transcription initially disabled",
            "existing configuration is never overwritten",
        ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--babel", type=Path, default=Path("target/release") / ("babel.exe" if platform.system() == "Windows" else "babel"))
    parser.add_argument("--output-dir", type=Path, default=Path("target/cli-smoke"))
    args = parser.parse_args()
    report = {
        "system": platform.system(),
        "release": platform.release(),
        "architecture": platform.machine(),
        "python": platform.python_version(),
        "status": "failed",
        "commands": [],
        "scope": "CLI/config smoke only; no audio devices, tray, drivers or real providers are exercised.",
    }
    try:
        smoke(args.babel.resolve(), report)
        report["status"] = "passed"
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    args.output_dir.mkdir(parents=True, exist_ok=True)
    destination = args.output_dir / "report.json"
    destination.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"CLI smoke {report['status']}: {destination}")
    if "error" in report:
        print(report["error"])
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
