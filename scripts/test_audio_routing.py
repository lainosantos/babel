#!/usr/bin/env python3
"""Linux routing regression using a private, hardware-free PipeWire session.

Requires pipewire, pipewire-pulse, WirePlumber >= 0.5, dbus-daemon and PulseAudio
client tools. No system defaults, user config, physical devices or API keys are
used. The temporary audio server and every child process are stopped on exit.

    python scripts/test_audio_routing.py
    python scripts/test_audio_routing.py --babel target/debug/babel

The optional Babel executable adds end-to-end device-selection, in-memory
history and recording session checks. Without it, only stream pinning is tested.
"""
from __future__ import annotations

import argparse
from array import array
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from urllib.request import Request, urlopen
import wave

PROJECT = Path(__file__).resolve().parent.parent
OWNER = "org.babel.audio.v1"


def wait_for(check, description, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(f"Timed out: {description}")


class AudioSandbox:
    def __init__(self, directory, tools):
        self.root = Path(directory)
        self.processes = []
        self.handles = []
        self.env = dict(
            os.environ,
            XDG_RUNTIME_DIR=directory,
            PIPEWIRE_RUNTIME_DIR=directory,
            XDG_CONFIG_HOME=str(self.root / "config"),
            XDG_STATE_HOME=str(self.root / "state"),
            XDG_DATA_HOME=str(self.root / "data"),
            PULSE_SERVER="unix:" + str(self.root / "pulse/native"),
            PIPEWIRE_REMOTE="pipewire-0",
            PATH=str(tools) + os.pathsep + os.environ.get("PATH", ""),
            LC_ALL="C.UTF-8",
        )
        # Do not inherit config-file overrides pointing at the real desktop.
        for name in ["PIPEWIRE_CONFIG_DIR", "PIPEWIRE_CONFIG_NAME", "WIREPLUMBER_CONFIG_DIR"]:
            self.env.pop(name, None)
        self.env["PULSE_CLIENTCONFIG"] = str(self.root / "client.conf")
        (self.root / "client.conf").write_text("autospawn = no\n")

    def spawn(self, args, **kwargs):
        log = (self.root / f"process-{len(self.processes)}.log").open("wb")
        self.handles.append(log)
        child = subprocess.Popen(
            list(map(str, args)), env=self.env, cwd=self.root,
            stderr=log, **kwargs,
        )
        self.processes.append(child)
        return child

    def run(self, args, check=True):
        return subprocess.run(
            list(map(str, args)), env=self.env, cwd=self.root,
            capture_output=True, text=True, timeout=8, check=check,
        )

    def pactl(self, *args, check=True):
        return self.run(["pactl", *args], check=check)

    def listing(self, kind):
        return json.loads(self.pactl("--format=json", "list", kind).stdout)

    def start(self):
        bus = self.spawn(
            ["dbus-daemon", "--session", "--nofork", "--print-address=1"],
            stdout=subprocess.PIPE, text=True,
        )
        self.env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
        self.spawn(["pipewire"], stdout=subprocess.DEVNULL)
        wait_for(lambda: (self.root / "pipewire-0").exists(), "private PipeWire socket")
        self.spawn(["pipewire-pulse"], stdout=subprocess.DEVNULL)
        wait_for(lambda: (self.root / "pulse/native").exists(), "private Pulse socket")
        # This profile loads policy only: no ALSA, Bluetooth or camera monitors.
        self.spawn(["wireplumber", "--profile", "policy"], stdout=subprocess.DEVNULL)
        self.pactl("load-module", "module-null-sink", "sink_name=babel_test_first")
        self.pactl("load-module", "module-null-sink", "sink_name=babel_test_other")

    def stream(self, device, *, record=False, pinned=False, fixed_target=False, name="Babel routing test", pcm=None):
        args = [
            "pacat", "--record" if record else "--playback", "--raw",
            "--format=s16le", "--channels=1", "--rate=48000", "--device=" + device,
            "--client-name=" + name,
        ]
        if pinned:
            args.extend("--property=" + value for value in [
                "application.id=org.babel.audio", "babel.owner=" + OWNER,
                "babel.target=" + device, "node.dont-move=true",
                "node.dont-reconnect=true", "node.dont-fallback=true",
            ])
        elif fixed_target:
            # Simulate an app retaining its explicit input selection when
            # WirePlumber moves ordinary default-following capture streams.
            # It remains an external consumer, without Babel ownership tags.
            args.append("--property=node.dont-move=true")
        if record:
            source = subprocess.DEVNULL
        else:
            source = open(pcm if pcm is not None else "/dev/zero", "rb")
            self.handles.append(source)
        return self.spawn(args, stdin=source, stdout=subprocess.DEVNULL)

    def own_streams(self):
        return [
            stream for kind in ["sink-inputs", "source-outputs"]
            for stream in self.listing(kind)
            if stream.get("properties", {}).get("babel.owner") == OWNER
        ]

    @staticmethod
    def stop(child, graceful=False):
        if child.poll() is None:
            child.send_signal(signal.SIGINT if graceful else signal.SIGTERM)
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=3)

    def close(self):
        for child in reversed(self.processes):
            self.stop(child)
        for handle in self.handles:
            handle.close()

    def logs(self):
        for path in sorted(self.root.glob("process-*.log")):
            text = path.read_text(errors="replace")[-3000:]
            if text:
                print(f"{path.name}:\n{text}")


def check_pinning(sandbox):
    playback = sandbox.stream("babel_test_first", pinned=True)
    capture = sandbox.stream("babel_test_first.monitor", record=True, pinned=True)
    first_sink = next(item["index"] for item in sandbox.listing("sinks") if item["name"] == "babel_test_first")
    first_source = next(item["index"] for item in sandbox.listing("sources") if item["name"] == "babel_test_first.monitor")
    streams = []
    for kind, key, expected, target in [
        ("sink-input", "sink", first_sink, "babel_test_other"),
        ("source-output", "source", first_source, "babel_test_other.monitor"),
    ]:
        stream = wait_for(
            lambda: next((s for s in sandbox.listing(kind + "s") if s[key] == expected), None),
            f"pinned {kind} connected",
        )
        assert stream["properties"]["babel.owner"] == OWNER
        moved = sandbox.pactl("move-" + kind, str(stream["index"]), target, check=False)
        assert moved.returncode != 0, f"Pinned {kind} unexpectedly moved"
        streams.append((kind, key, expected))
    sandbox.pactl("set-default-sink", "babel_test_other")
    sandbox.pactl("set-default-source", "babel_test_other.monitor")
    time.sleep(0.3)
    for kind, key, expected in streams:
        assert sandbox.listing(kind + "s")[0][key] == expected
    sandbox.stop(playback)
    sandbox.stop(capture)
    wait_for(lambda: not sandbox.own_streams(), "pinning test streams stopped")
    print("PASS: capture/playback reject moves and retain targets after default changes.")


class Babel:
    def __init__(self, sandbox, executable):
        self.sandbox = sandbox
        config = sandbox.root / "babel.toml"
        config.write_text('''[agent]
enabled = false
[microphone]
enabled = false
capture_device = "babel_test_first.monitor"
playback_device = "babel_mic_bus"
[speaker]
enabled = false
capture_device = "babel_speaker.monitor"
playback_device = "babel_test_other"
[transcription]
enabled = false
[recording]
enabled = false
directory = "recordings"
''')
        sandbox.run([executable, "--config", config, "setup"])
        self.output = sandbox.root / "babel.stdout"
        output = self.output.open("wb")
        sandbox.handles.append(output)
        self.process = sandbox.spawn(
            [executable, "--config", config, "serve", "--no-tray", "--port", "0"],
            stdout=output,
        )
        match = wait_for(
            lambda: re.search(r"http://127\.0\.0\.1:(\d+)/#token=([a-f0-9]{64})", self.output.read_text()),
            "Babel authenticated dashboard", timeout=15,
        )
        self.url = "http://127.0.0.1:" + match[1]
        self.token = match[2]

    def api(self, path, method="GET", body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = Request(self.url + "/api/" + path, data=data, method=method, headers={
            "Authorization": "Bearer " + self.token,
            "Content-Type": "application/json",
        })
        with urlopen(request, timeout=8) as response:
            return json.load(response)

    def expect_routes(self, targets):
        targets = set(targets)
        def expected():
            streams = self.sandbox.own_streams()
            actual = {s["properties"].get("babel.target") for s in streams}
            return actual == targets and len(streams) == len(targets)
        try:
            wait_for(expected, f"Babel streams target only {sorted(targets)}", timeout=15)
        except AssertionError as error:
            actual = [s["properties"].get("babel.target") for s in self.sandbox.own_streams()]
            raise AssertionError(
                f"{error}; actual targets={actual}; status={self.api('status')}"
            ) from error
        # Catch a restarted worker briefly matching before it opens an unwanted route.
        time.sleep(0.25)
        assert expected()


def check_babel(sandbox, executable, snapshot):
    babel = Babel(sandbox, executable)
    if snapshot:
        snapshot.write_text(sandbox.pactl("--format=json", "list").stdout)
    mic_targets = {"babel_test_first.monitor", "babel_mic_bus"}
    speaker_targets = {"babel_speaker.monitor", "babel_test_other"}
    babel.expect_routes(set())
    status = babel.api("status")
    assert not status["routing_active"] and not status["running"]
    assert status["history"]["enabled"] and status["history"]["capacity_secs"] == 600
    assert status["history"]["available_secs"] == 0
    assert status["history"]["buffered_bytes"] == 0
    assert not (sandbox.root / "recordings").exists()
    assert not (sandbox.root / "transcripts").exists()

    application = sandbox.stream("babel_speaker", name="External playback test")
    babel.expect_routes(speaker_targets)
    wait_for(lambda: babel.api("status")["history"]["speaker_secs"] >= 0.3,
             "speaker originals retained without a session")
    status = babel.api("status")
    assert not status["running"] and status["history"]["microphone_secs"] == 0
    assert status["history"]["available_secs"] >= 0.3
    assert not (sandbox.root / "recordings").exists()
    assert not (sandbox.root / "transcripts").exists()
    app_stream = wait_for(lambda: next((s for s in sandbox.listing("sink-inputs") if s["properties"].get("application.name") == "External playback test"), None), "external playback stream")
    sandbox.pactl("set-default-sink", "babel_test_other")
    sandbox.pactl("move-sink-input", str(app_stream["index"]), "babel_test_other")
    babel.expect_routes(set())
    retained = babel.api("status")["history"]["speaker_secs"]
    time.sleep(0.3)
    assert babel.api("status")["history"]["speaker_secs"] == retained, \
        "History captured new speaker PCM after the application left Babel"
    sandbox.pactl("move-sink-input", str(app_stream["index"]), "babel_speaker")
    babel.expect_routes(speaker_targets)
    microphone_app = sandbox.stream("babel_microphone", record=True, name="External capture test")
    babel.expect_routes(speaker_targets | mic_targets)
    sandbox.stop(application)
    babel.expect_routes(mic_targets)
    sandbox.stop(microphone_app)
    babel.expect_routes(set())
    print("PASS: independent routes follow external clients; selecting physical output releases Babel.")
    assert not (sandbox.root / "recordings").exists()
    assert not (sandbox.root / "transcripts").exists()
    print("PASS: idle history retains only selected originals in RAM and creates no session files.")
    check_default_microphone(sandbox, babel, mic_targets)

    config = babel.api("config")
    config["recording"]["enabled"] = True
    babel.api("config", "PUT", config)
    babel.api("start", "POST", {"name": "Selection regression"})
    status = babel.api("status")
    session_id = status["session_id"]
    assert session_id
    assert status["history_included_secs"] == 0, "Ordinary Start unexpectedly included retained audio"
    babel.expect_routes(set())
    application = sandbox.stream("babel_speaker", name="External recording test")
    babel.expect_routes(speaker_targets)
    time.sleep(0.35)
    first_pids = {s["properties"]["application.process.id"] for s in sandbox.own_streams()}
    sandbox.stop(application)
    babel.expect_routes(set())
    status = babel.api("status")
    assert status["running"] and status["session_id"] == session_id
    application = sandbox.stream("babel_speaker", name="External resumed test")
    babel.expect_routes(speaker_targets)
    next_pids = {s["properties"]["application.process.id"] for s in sandbox.own_streams()}
    assert first_pids.isdisjoint(next_pids), "Old server queues survived deactivation"
    time.sleep(0.35)
    assert babel.api("status")["session_id"] == session_id
    sandbox.stop(application)
    babel.expect_routes(set())
    babel.api("stop", "POST")
    recordings = list((sandbox.root / "recordings").glob("*.wav"))
    assert len(recordings) == 1, "Session must retain one merged audio file"
    with wave.open(str(recordings[0])) as recording:
        assert recording.getnchannels() == 1 and recording.getnframes() > 0
    print("PASS: recording keeps its session/file across deactivation; resumed routes use fresh processes.")
    check_history_recording(sandbox, babel, speaker_targets, recordings[0])
    sandbox.stop(babel.process, graceful=True)


def check_default_microphone(sandbox, babel, mic_targets):
    """A default virtual mic activates routing without requiring a capture app."""
    sandbox.pactl("set-default-source", "babel_microphone")
    babel.expect_routes(mic_targets)
    wait_for(lambda: babel.api("agent/status")["microphone_active"],
             "default microphone feeds the command tap without a capture app")
    status = babel.api("status")
    assert not status["running"]
    assert status["microphone"]["state"] == "passthrough"
    assert status["speaker"]["state"] == "waiting_for_app"
    original_pids = {s["properties"]["application.process.id"] for s in sandbox.own_streams()}

    microphone_app = sandbox.stream("babel_microphone", record=True, name="Default mic capture test")
    babel.expect_routes(mic_targets)
    sandbox.stop(microphone_app)
    babel.expect_routes(mic_targets)
    assert babel.api("agent/status")["microphone_active"]
    assert original_pids == {s["properties"]["application.process.id"] for s in sandbox.own_streams()}, \
        "Closing a capture app interrupted the still-selected default microphone"

    microphone_app = sandbox.stream("babel_microphone", record=True, fixed_target=True, name="Explicit mic selection test")
    babel.expect_routes(mic_targets)
    sandbox.pactl("set-default-source", "babel_test_other.monitor")
    babel.expect_routes(mic_targets)
    assert babel.api("agent/status")["microphone_active"], \
        "Changing the default interrupted an app explicitly using Babel"
    sandbox.stop(microphone_app)
    babel.expect_routes(set())
    wait_for(lambda: not babel.api("agent/status")["microphone_active"],
             "physical default and no Babel capture app release the command tap")
    retained = babel.api("status")["history"]["microphone_secs"]
    time.sleep(0.3)
    assert babel.api("status")["history"]["microphone_secs"] == retained
    assert not (sandbox.root / "recordings").exists()
    assert not (sandbox.root / "transcripts").exists()
    print("PASS: system-default mic routes and activates the command tap without an app; physical selection releases it unless explicitly used.")


def check_history_recording(sandbox, babel, speaker_targets, previous_recording):
    """Use a known historical signal and live silence to verify the saved prefix."""
    config = babel.api("config")
    assert not config["transcription"]["enabled"]
    assert not config["microphone"]["enabled"] and not config["speaker"]["enabled"]
    assert config["recording"]["microphone"] and config["recording"]["speaker"]
    assert config["history"]["enabled"] and config["history"]["duration_secs"] == 600
    config["history"]["enabled"] = False
    babel.api("config", "PUT", config)
    status = babel.api("status")
    assert not status["history"]["enabled"] and status["history"]["buffered_bytes"] == 0

    # A finite generated PCM fixture stays inside this private audio server. It
    # lasts longer than the test, so pacat remains a normal external consumer.
    signal_pcm = array("h", (round(12000 * math.sin(2 * math.pi * 997 * n / 48000))
                             for n in range(48000)))
    if sys.byteorder != "little":
        signal_pcm.byteswap()
    tone_path = sandbox.root / "history-tone.pcm"
    tone_path.write_bytes(signal_pcm.tobytes() * 30)
    application = sandbox.stream("babel_speaker", name="External disabled history test", pcm=tone_path)
    babel.expect_routes(speaker_targets)
    time.sleep(0.3)
    assert babel.api("status")["history"]["buffered_bytes"] == 0
    sandbox.stop(application)
    babel.expect_routes(set())

    # Re-enabling starts with empty RAM. A shorter configurable capacity keeps
    # the entire regression bounded without changing the default assertion.
    config = babel.api("config")
    config["history"]["enabled"] = True
    config["history"]["duration_secs"] = 10
    babel.api("config", "PUT", config)
    assert babel.api("status")["history"]["available_secs"] == 0
    application = sandbox.stream("babel_speaker", name="External historical tone", pcm=tone_path)
    babel.expect_routes(speaker_targets)
    wait_for(lambda: babel.api("status")["history"]["speaker_secs"] >= 2.3,
             "known original tone retained for a two-second prefix")
    status = babel.api("status")
    assert not status["running"] and status["history"]["microphone_secs"] == 0
    assert list((sandbox.root / "recordings").glob("*.wav")) == [previous_recording], \
        "Memory history wrote an audio file before session Start"
    assert not (sandbox.root / "transcripts").exists()

    started = time.monotonic()
    babel.api("start", "POST", {"name": "History prefix regression", "history_seconds": 2})
    status = babel.api("status")
    assert status["running"] and status["session_id"]
    included = status["history_included_secs"]
    assert 1.8 <= included <= 2.01, f"Unexpected historical prefix duration: {included}"
    assert not status["history_transcription_pending"]
    session_id = status["session_id"]
    # End the tone immediately after Start, so the later part of the prefix
    # cannot accidentally be satisfied by newly captured live tone.
    sandbox.stop(application)
    babel.expect_routes(set())
    application = sandbox.stream("babel_speaker", name="External live silence")
    babel.expect_routes(speaker_targets)
    time.sleep(0.8)
    assert babel.api("status")["session_id"] == session_id
    babel.api("stop", "POST")
    elapsed = time.monotonic() - started
    status = babel.api("status")
    assert not status["running"] and not status["last_error"], status
    sandbox.stop(application)
    babel.expect_routes(set())

    recordings = list((sandbox.root / "recordings").glob("*.wav"))
    assert len(recordings) == 2, "History and live capture must produce one file per session"
    path = next(path for path in recordings if path != previous_recording)
    with wave.open(str(path)) as recording:
        assert recording.getnchannels() == 1 and recording.getsampwidth() == 2
        rate = recording.getframerate()
        frames = recording.getnframes()
        assert rate == 16000
        samples = array("h", recording.readframes(frames))
    if sys.byteorder != "little":
        samples.byteswap()
    assert path.stat().st_size == 44 + frames * 2, "Historical WAV was not finalized"
    duration = frames / rate
    assert included + 0.7 <= duration <= included + elapsed + 2, \
        f"WAV does not contain its historical prefix plus live audio: {duration:.3f}s"
    prefix = samples[rate // 4:rate]
    late_prefix = samples[rate * 5 // 4:rate * 7 // 4]
    tail = samples[-rate // 4:]
    rms = lambda pcm: math.sqrt(sum(sample * sample for sample in pcm) / len(pcm))
    assert rms(prefix) > 1000, "Saved WAV lost the original historical tone"
    assert rms(late_prefix) > 1000, "Saved WAV truncated the historical prefix"
    assert rms(tail) < 100, "Saved WAV did not append the live silence after the historical tone"
    print("PASS: opt-in history saves a tone prefix plus live audio in one finalized WAV; default Start excludes it.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--babel", type=Path, help="Babel executable to test end-to-end")
    parser.add_argument("--pulse-tools", type=Path, help="Directory containing pactl/pacat/parec")
    parser.add_argument("--snapshot", type=Path, help="Write isolated pactl JSON snapshot for diagnostics")
    args = parser.parse_args()
    for program in ["pipewire", "pipewire-pulse", "wireplumber", "dbus-daemon"]:
        if not shutil.which(program):
            parser.error(f"Missing prerequisite: {program}")
    tools = args.pulse_tools
    if tools is None:
        installed = shutil.which("pactl")
        tools = Path(installed).parent if installed else PROJECT / ".tools/pulse/usr/bin"
    tools = tools.resolve()
    if not all((tools / command).exists() for command in ["pactl", "pacat", "parec"]):
        parser.error("PulseAudio client tools unavailable; use --pulse-tools")
    executable = args.babel.resolve() if args.babel else None
    if executable and not executable.is_file():
        parser.error("Babel executable does not exist")
    with tempfile.TemporaryDirectory(prefix="babel-routing-test-") as directory:
        sandbox = AudioSandbox(directory, tools)
        try:
            sandbox.start()
            check_pinning(sandbox)
            if executable:
                check_babel(sandbox, executable, args.snapshot)
            elif args.snapshot:
                args.snapshot.write_text(sandbox.pactl("--format=json", "list").stdout)
            print("All checks passed; the real desktop audio session was untouched.")
        except BaseException:
            sandbox.logs()
            raise
        finally:
            sandbox.close()


if __name__ == "__main__":
    main()
