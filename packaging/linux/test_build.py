"""Package tests never install packages, enable services or run Babel."""
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tarfile
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("babel_linux_build", HERE / "build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class PolicyTests(unittest.TestCase):
    def test_versions_cannot_escape_staging_or_inject_debian_fields(self):
        self.assertEqual(build.package_version("1.2.3-rc.1+ci.4"), "1.2.3-rc.1+ci.4")
        self.assertEqual(build.deb_version("1.2.3-rc.1"), "1.2.3~rc.1")
        self.assertEqual(build.rpm_version("1.2.3-rc-1"), "1.2.3~rc~1")
        for value in ["../1.2.3", "/tmp/1.2.3", "1.2.3\nDepends: something", "1.2.3;touch x", "next"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                build.package_version(value)

    def test_non_linux_and_wrong_architecture_inputs_are_rejected_before_tools_run(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "babel"
            for content in [b"MZ" + bytes(100), b"#!/bin/sh\n", b"\x7fELF\x02\x01" + bytes(58)]:
                binary.write_bytes(content)
                with self.assertRaises(ValueError):
                    build.elf_runtime(binary)

    def test_archive_validation_rejects_traversal_links_and_unexpected_files(self):
        for name, kind in [("../outside", tarfile.REGTYPE), ("/absolute", tarfile.REGTYPE), ("usr/bin/babel", tarfile.SYMTYPE), ("usr/bin/extra", tarfile.REGTYPE)]:
            with self.subTest(name=name):
                contents = io.BytesIO()
                with tarfile.open(fileobj=contents, mode="w") as archive:
                    info = tarfile.TarInfo(name); info.type = kind
                    info.linkname = "outside" if kind == tarfile.SYMTYPE else ""
                    archive.addfile(info, io.BytesIO(b""))
                contents.seek(0)
                with tarfile.open(fileobj=contents, mode="r") as archive, self.assertRaises(ValueError):
                    build.validate_archive(archive, {})

    def test_launcher_uses_user_config_with_quoted_paths_and_does_not_start_sessions(self):
        with tempfile.TemporaryDirectory(prefix="Babel space é $") as directory:
            root = Path(directory); binaries = root / "bin"
            binaries.mkdir()
            build.copy_file(HERE / "babel-launch", binaries / "babel-launch", 0o755)
            stub = binaries / "babel-tray"
            stub.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$BABEL_TEST_OUTPUT"\n')
            stub.chmod(0o755)
            output = root / "args.txt"
            for xdg, config_root in [("", root / "home/.config"), ("relative", root / "home/.config"), (str(root / "config with spaces"), root / "config with spaces")]:
                env = {**os.environ, "HOME": str(root / "home"), "XDG_CONFIG_HOME": xdg, "BABEL_TEST_OUTPUT": str(output)}
                subprocess.run([str(binaries / "babel-launch")], check=True, env=env, cwd="/")
                self.assertEqual(output.read_text().splitlines(), ["--config", str(config_root / "babel/babel.toml"), "--port", "0"])
                self.assertTrue((config_root / "babel").is_dir())
                self.assertFalse((config_root / "babel/babel.toml").exists())
                self.assertEqual((config_root / "babel").stat().st_mode & 0o777, 0o700)
            self.assertFalse((root / "home/.config/autostart").exists())
            # The systemd drop-in supplies a complete invocation. Preserve its
            # arguments exactly and do not require HOME or create default dirs.
            args = ["--config", str(root / "custom config é.toml"), "--port", "0"]
            subprocess.run([str(binaries / "babel-launch"), *args], check=True,
                           env={**os.environ, "HOME": "", "XDG_CONFIG_HOME": "", "BABEL_TEST_OUTPUT": str(output)})
            self.assertEqual(output.read_text().splitlines(), args)

    def test_user_unit_is_graphical_opt_in_and_shuts_down_cleanly(self):
        unit = (HERE / "org.babel.audio.service").read_text()
        for line in ["# Babel-Autostart-Owner: org.babel.audio.autostart.v1",
                     "PartOf=graphical-session.target", "WantedBy=graphical-session.target",
                     "After=graphical-session-pre.target", "ExecStart=/usr/bin/babel-launch",
                     "ConditionUser=!root", "ConditionEnvironment=|DISPLAY",
                     "ConditionEnvironment=|WAYLAND_DISPLAY", "Restart=on-failure",
                     "KillSignal=SIGINT", "KillMode=mixed", "TimeoutStopSec=15"]:
            self.assertIn(line, unit.splitlines())
        self.assertNotIn("After=graphical-session.target", unit)
        self.assertNotIn("multi-user.target", unit)
        self.assertNotIn("ExecStartPre=", unit)

    def test_cpio_rejects_paths_links_owners_missing_and_corrupt_content(self):
        def member(name, mode=stat.S_IFREG | 0o755, uid=0, content=b"ok"):
            name = name.encode() + b"\0"
            fields = [1, mode, uid, 0, 1, 0, len(content), 0, 0, 0, 0, len(name), 0]
            header = b"070701" + b"".join(f"{field:08x}".encode() for field in fields)
            return header + name + bytes(-(len(header) + len(name)) % 4) + content + bytes(-len(content) % 4)
        expected = {"usr/bin/babel": (0o755, build.hashlib.sha256(b"ok").hexdigest())}
        trailer = member("TRAILER!!!", content=b"")
        build.validate_cpio(io.BytesIO(member("./usr/bin/babel") + trailer), expected)
        for value in [member("../outside"), member("/usr/bin/babel"),
                      member("usr/bin/babel", mode=stat.S_IFLNK | 0o755),
                      member("usr/bin/babel", uid=1000), member("usr/bin/babel", content=b"bad"),
                      b"", member("usr/bin/babel") * 2]:
            with self.subTest(value=value[:80]), self.assertRaises(ValueError):
                build.validate_cpio(io.BytesIO(value + trailer), expected)

    @unittest.skipUnless(shutil.which("systemd-analyze"), "systemd syntax validator required")
    def test_systemd_accepts_unit_syntax_without_installing_or_running_it(self):
        with tempfile.TemporaryDirectory() as directory:
            unit = Path(directory) / "org.babel.audio.service"
            # Verify parses/checks units; it never starts them. Substitute only
            # the executable because the package is deliberately not installed.
            unit.write_text((HERE / unit.name).read_text().replace("ExecStart=/usr/bin/babel-launch", "ExecStart=/bin/true"))
            # CI has no login/session manager or XDG runtime directory. Verify
            # remains offline with this private directory and requires no bus.
            build.run("systemd-analyze", "--user", "verify", "--man=no", unit,
                      env={"XDG_RUNTIME_DIR": directory})


@unittest.skipUnless(all(shutil.which(tool) for tool in ("dpkg-deb", "readelf", "rpm", "rpm2cpio", "rpmbuild")) and Path("/bin/true").is_file(), "Linux ELF/dpkg/RPM tools required")
class ArchiveTests(unittest.TestCase):
    def test_three_formats_contain_exact_binaries_modes_and_no_install_hooks(self):
        with tempfile.TemporaryDirectory(prefix="Babel packages é % ") as directory:
            root = Path(directory); binaries = root / "bin"; binaries.mkdir()
            for name in build.BINARIES:
                shutil.copyfile("/bin/true", binaries / name)
            # A harmless ELF trailer distinguishes hashes without invoking a
            # compiler or depending on another system executable's libraries.
            with (binaries / "babel-tray").open("ab") as stream:
                stream.write(b"Babel package fixture")
            # Fixtures are inspected/copied, never executed, and staged modes are explicit.
            report = build.build(binaries, root / "artifacts", "0.0.0-test.1")
            self.assertEqual(len(report["artifacts"]), 3)
            package = root / "artifacts/babel-audio_0.0.0~test.1_amd64.deb"
            fields = build.run("dpkg-deb", "--field", package).stdout
            self.assertIn(f"libc6 (>= {report['glibc_minimum']})", fields)
            self.assertIn("Suggests: pipewire-pulse | pulseaudio", fields)
            self.assertNotIn("Recommends:", fields)
            tar = root / "artifacts/babel-audio-0.0.0-test.1-linux-amd64.tar.gz"
            with tarfile.open(tar, "r:gz") as archive:
                prefix = "babel-audio-0.0.0-test.1-linux-amd64"
                for name in build.BINARIES:
                    member = archive.getmember(f"{prefix}/bin/{name}")
                    self.assertEqual(member.mode, 0o755)
                    self.assertEqual(archive.extractfile(member).read(), (binaries / name).read_bytes())
                manifest = json.load(archive.extractfile(f"{prefix}/share/doc/babel-audio/package-manifest.json"))
                self.assertEqual(manifest["files"]["bin/babel"]["sha256"], build.digest(binaries / "babel"))
                unit = archive.getmember(f"{prefix}/lib/systemd/user/org.babel.audio.service")
                self.assertEqual(archive.extractfile(unit).read(), (HERE / "org.babel.audio.service").read_bytes())
                self.assertFalse(any("/autostart/" in member.name or ".wants/" in member.name for member in archive))
            controls = root / "control"
            build.run("dpkg-deb", "--control", package, controls)
            self.assertEqual({file.name for file in controls.iterdir()}, {"control", "md5sums"})
            rpm = root / "artifacts/babel-audio-0.0.0~test.1-1.x86_64.rpm"
            self.assertIn("/usr/lib/systemd/user/org.babel.audio.service", build.run("rpm", "-qp", "--list", rpm).stdout.splitlines())
            requirements = build.run("rpm", "-qp", "--requires", rpm).stdout
            self.assertIn(f"glibc >= {report['glibc_minimum']}", requirements)
            self.assertIn("/usr/bin/pactl", requirements)
            self.assertEqual(build.run("rpm", "-qp", "--scripts", rpm).stdout.strip(), "")
            # Independent staging/build roots must yield byte-identical outputs.
            second = build.build(binaries, root / "second output", "0.0.0-test.1")
            self.assertEqual(report["artifacts"], second["artifacts"])


if __name__ == "__main__":
    unittest.main()
