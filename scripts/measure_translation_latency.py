#!/usr/bin/env python3
"""Measure Babel's real translation path using explicitly supplied synthetic speech.

Linux only; requires pactl, pacat and parec connected to the desktop audio server.
The application/provider under test remains the normal cross-platform Babel code.
Set BABEL_DASHBOARD_URL to its bare loopback URL and BABEL_DASHBOARD_TOKEN to its
dashboard token in the calling environment. API keys stay inside Babel. Example:

    python scripts/measure_translation_latency.py --speech synthetic.wav \
        --provider gemini --trials 3 --json /tmp/babel-latency.json

Use --routing-only first to measure the original software route without a cloud
request. Normal trials send the synthetic WAV to the selected existing provider
profile and may incur charges. Do not change settings or start sessions during
the test. Original settings are restored, preserving concurrent unrelated edits.
Only owned null sinks are used; defaults and physical devices are not changed.
Reports never contain keys, prompts, transcripts or the full configuration.

Timings use monotonic clocks. The engine observation is bounded by HTTP polling,
and monitor timestamps include Pulse client scheduling: these are software-path
measurements, not acoustic latency or model-internal inference measurements.
"""
from __future__ import annotations

import argparse
from array import array
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener
import uuid
import wave


class ProbeError(Exception):
    """A safe diagnostic that cannot contain remote response bodies or secrets."""


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ProbeError("The local dashboard unexpectedly redirected a request")


class Dashboard:
    def __init__(self):
        self.url = os.environ.get("BABEL_DASHBOARD_URL", "").rstrip("/")
        parsed = urlsplit(self.url)
        if (parsed.scheme != "http" or parsed.hostname not in {"127.0.0.1", "[::1]", "::1"}
                or parsed.username or parsed.password or parsed.path or parsed.query
                or parsed.fragment or not parsed.port):
            raise ProbeError("BABEL_DASHBOARD_URL must be a bare HTTP loopback URL with its actual port")
        self.token = os.environ.get("BABEL_DASHBOARD_TOKEN", "")
        if not self.token or any(ch.isspace() for ch in self.token):
            raise ProbeError("Set BABEL_DASHBOARD_TOKEN in the calling environment")
        self.opener = build_opener(ProxyHandler({}), NoRedirect)

    def api(self, path, method="GET", body=None, revision=None, timeout=12):
        headers = {"Authorization": "Bearer " + self.token, "Origin": self.url,
                   "Content-Type": "application/json"}
        if revision:
            headers["If-Match"] = revision
        request = Request(self.url + "/api/" + path, method=method, headers=headers,
                          data=json.dumps(body).encode() if body is not None else None)
        try:
            with self.opener.open(request, timeout=timeout) as response:
                data = response.read(4 * 1024 * 1024 + 1)
                if len(data) > 4 * 1024 * 1024:
                    raise ProbeError("The local dashboard response exceeded the limit")
                return json.loads(data), response.headers.get("ETag")
        except HTTPError as error:
            raise ProbeError(f"Dashboard {method} {path.split('?')[0]} returned HTTP {error.code}") from None
        except (URLError, TimeoutError, OSError, ValueError):
            raise ProbeError(f"Dashboard {method} {path.split('?')[0]} failed") from None

    def ensure_idle(self):
        status, _ = self.api("status")
        retained, _ = self.api("retention")
        if status["running"] or retained:
            raise ProbeError("Finish the active session and recover pending original files before testing")


def restore_patch(original, applied, current):
    """Revert only our changed leaves; preserve concurrent edits and report conflicts."""
    restored = copy.deepcopy(current)
    conflicts = []
    for key in original:
        if original[key] == applied[key]:
            continue
        if isinstance(original[key], dict) and isinstance(current.get(key), dict):
            restored[key], nested = restore_patch(original[key], applied[key], current[key])
            conflicts.extend(key + "." + name for name in nested)
        elif current.get(key) == applied[key]:
            restored[key] = copy.deepcopy(original[key])
        elif current.get(key) != original[key]:
            conflicts.append(key)
    return restored, conflicts


class AudioDevices:
    def __init__(self):
        self.modules = []
        self.processes = []
        self.defaults = None
        self.env = {key: value for key, value in os.environ.items()
                    if key not in {"BABEL_DASHBOARD_TOKEN", "BABEL_DASHBOARD_URL"}}
        self.env["LC_ALL"] = "C"
        suffix = uuid.uuid4().hex[:12]
        self.source = "babel_probe_in_" + suffix
        self.destination = "babel_probe_out_" + suffix
        self.speaker_source = "babel_probe_silent_in_" + suffix
        self.speaker_destination = "babel_probe_silent_out_" + suffix

    def pactl(self, *args):
        try:
            result = subprocess.run(["pactl", *args], env=self.env, capture_output=True,
                                    timeout=8, check=True)
            return result.stdout.decode().strip()
        except (subprocess.SubprocessError, OSError, UnicodeError):
            raise ProbeError("PulseAudio control failed for an owned probe device") from None

    def start(self):
        # Null sinks isolate all synthetic audio from speakers and microphones.
        self.defaults = (self.pactl("get-default-sink"), self.pactl("get-default-source"))
        # Sessions validate both routes, even when speaker translation is off.
        # Keep that route valid and silent without touching any real devices.
        for name in (self.source, self.destination, self.speaker_source, self.speaker_destination):
            module = self.pactl("load-module", "module-null-sink", "sink_name=" + name,
                                "rate=48000", "channels=1", "format=s16le")
            if not module.isdecimal():
                raise ProbeError("PulseAudio returned an invalid owned module identifier")
            self.modules.append((module, name))

    def spawn(self, device, capture):
        args = ["parec" if capture else "pacat", "--raw", "--format=s16le",
                "--rate=16000", "--channels=1", "--latency-msec=10",
                "--process-time-msec=5", "--device=" + device,
                "--client-name=Babel synthetic latency probe",
                "--property=node.dont-move=true", "--property=node.dont-fallback=true",
                "--property=node.dont-reconnect=true"]
        child = subprocess.Popen(args, env=self.env, stdin=subprocess.DEVNULL if capture else subprocess.PIPE,
                                 stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, bufsize=0)
        self.processes.append(child)
        return child

    @staticmethod
    def stop(child):
        if child.poll() is None:
            child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=3)
        for stream in (child.stdin, child.stdout):
            if stream:
                stream.close()

    def close(self):
        for child in reversed(self.processes):
            self.stop(child)
        failures = 0
        for module, name in reversed(self.modules):
            try:
                # Numeric module IDs may be reused if another process removes
                # our device. Never unload a replacement owned by another app.
                records = [line.split("\t", 3) for line in self.pactl("list", "short", "modules").splitlines()]
                current = next((record for record in records if record[0] == module), None)
                if current is None:
                    continue
                if len(current) < 3 or current[1] != "module-null-sink" or "sink_name=" + name not in current[2].split():
                    failures += 1
                    continue
                self.pactl("unload-module", module)
            except ProbeError:
                failures += 1
        if self.defaults is not None and self.defaults != (self.pactl("get-default-sink"), self.pactl("get-default-source")):
            raise ProbeError("System default devices changed during testing; external changes were not overwritten")
        if failures:
            raise ProbeError("Some owned probe devices could not be removed")


class Monitor:
    def __init__(self, child, threshold, output=None):
        self.child = child
        self.threshold = threshold
        self.lock = threading.Lock()
        self.first = None
        self.last = None
        self.started = False
        self.failed = False
        self.output = output
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()

    def read(self):
        pending = b""
        try:
            while chunk := self.child.stdout.read(320):
                now = time.monotonic()
                pending += chunk
                length = len(pending) - len(pending) % 2
                if not length:
                    continue
                block, pending = pending[:length], pending[length:]
                samples = array("h", block)
                if sys.byteorder != "little":
                    samples.byteswap()
                loud = [i for i, value in enumerate(samples) if abs(value) >= self.threshold]
                with self.lock:
                    if not self.started:
                        continue
                    if self.output:
                        self.output.writeframesraw(block)
                    if loud:
                        first = now - (len(samples) - loud[0]) / 16000
                        last = now - (len(samples) - loud[-1]) / 16000
                        self.first = first if self.first is None else self.first
                        self.last = last
        except (OSError, ValueError):
            self.failed = True

    def arm(self):
        with self.lock:
            self.started = True

    def times(self):
        with self.lock:
            return self.first, self.last


def read_fixture(path):
    try:
        with wave.open(str(path), "rb") as source:
            if (source.getnchannels(), source.getsampwidth(), source.getframerate(), source.getcomptype()) != (1, 2, 16000, "NONE"):
                raise ProbeError("The explicitly supplied synthetic WAV must be mono PCM16 at 16000 Hz")
            duration = source.getnframes() / 16000
            if not 1 <= duration <= 60:
                raise ProbeError("Synthetic speech must be between 1 and 60 seconds")
            pcm = source.readframes(source.getnframes())
            if len(pcm) != source.getnframes() * 2:
                raise ProbeError("The synthetic WAV is truncated")
    except (wave.Error, EOFError, OSError):
        raise ProbeError("The synthetic WAV could not be read") from None
    if not any(pcm):
        raise ProbeError("The fixture contains only digital silence")
    return pcm, duration


def play_paced(child, pcm, finished, errors):
    """Send silence plus speech at audio speed; pacat never receives a file burst."""
    try:
        data = bytes(16000) + pcm + bytes(16000)
        start = time.monotonic()
        for offset in range(0, len(data), 320):
            wait = start + offset / 32000 - time.monotonic()
            if wait > 0:
                time.sleep(wait)
            child.stdin.write(data[offset:offset + 320])
        child.stdin.close()
        child.wait(timeout=8)
        if child.returncode:
            errors.append("Synthetic audio playback failed")
    except (BrokenPipeError, OSError, ValueError, subprocess.TimeoutExpired):
        errors.append("Synthetic audio playback did not complete")
    finally:
        finished.set()


def elapsed(start, end):
    return round(end - start, 4) if start is not None and end is not None else None


def status_failure(status, phase):
    """Classify a known local failure without exporting raw diagnostic text."""
    messages = [status.get("last_error")]
    for route in ("microphone", "speaker"):
        messages.extend(status.get(route, {}).get(key) for key in ("device_error", "processing_error"))
    if any(isinstance(message, str) and "Playback queue is full." in message for message in messages):
        return "Translated playback capacity was exceeded; this trial is invalid"
    return f"Babel reported a {phase} error; inspect its dashboard locally"


def validate_local_bridge(endpoint, provider, credential_provider):
    """Only an explicitly selected local test bridge can receive another profile's key."""
    if not endpoint:
        if credential_provider:
            raise ProbeError("A credential profile override requires an explicit local test bridge")
        return
    try:
        parsed = urlsplit(endpoint)
        valid = (provider == "openai" and credential_provider in {"gemini", "openai"}
                 and parsed.scheme == "ws" and parsed.hostname in {"127.0.0.1", "::1"}
                 and parsed.port and parsed.path not in {"", "/"}
                 and not any((parsed.username, parsed.password, parsed.query, parsed.fragment)))
    except ValueError:
        valid = False
    if not valid:
        raise ProbeError("Use an explicit loopback WebSocket bridge, OpenAI transport and credential profile")


def run_trial(api, devices, pcm, args, index, revision, active):
    result = {"trial": index, "errors": []}
    monitors = []
    writer = None
    feeder = None
    children = []
    try:
        api.ensure_idle()
        _, observed_revision = api.api("config")
        if observed_revision != revision:
            raise ProbeError("Settings changed before the next trial; stopping instead of overriding them")
        agent, _ = api.api("agent")
        if agent["enabled"]:
            raise ProbeError("Voice commands were enabled outside the probe; stop testing before playing synthetic speech")
        if args.capture_output:
            path = args.capture_output / f"{args.provider}-{'routing' if args.routing_only else 'translation'}-{index}-{uuid.uuid4().hex[:8]}.wav"
            writer = wave.open(str(path), "wb")
            writer.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
            result["synthetic_output_file"] = str(path.resolve())
        for device, output in ((devices.source, None), (devices.destination, writer)):
            child = devices.spawn(device + ".monitor", True)
            children.append(child)
            monitors.append(Monitor(child, args.threshold, output))
        setup_start = time.monotonic()
        if not args.routing_only:
            active["name"] = "Synthetic latency probe " + uuid.uuid4().hex[:12]
            api.api("start", "POST", {"name": active["name"], "history_seconds": 0}, revision, timeout=40)
        deadline = setup_start + 40
        while True:
            status, _ = api.api("status")
            if active.get("name") and status.get("session_name") == active["name"]:
                active["id"] = status.get("session_id")
            wanted = "passthrough" if args.routing_only else "running"
            if status["microphone"]["state"] == wanted and status["routing_active"]:
                break
            if status.get("last_error") or status["microphone"].get("device_error"):
                raise ProbeError(status_failure(status, "setup"))
            if time.monotonic() >= deadline:
                raise ProbeError("The isolated microphone route did not become ready")
            time.sleep(0.05)
        result["setup_ready_seconds"] = elapsed(setup_start, time.monotonic())
        # The ready event and audio-device startup are asynchronous. Keep the
        # fixture silent while the isolated route and both monitors settle.
        time.sleep(0.25)
        status, _ = api.api("status")
        baseline = status["microphone"]["translated_samples"]
        initial_counters = {name: status["microphone"][name] for name in (
            "captured_frames", "dropped_frames", "processing_dropped_frames", "reconnects")}
        last_samples = baseline
        first_engine = None
        last_engine = None
        polls = []
        previous_poll = time.monotonic()
        for monitor in monitors:
            monitor.arm()
        finished = threading.Event()
        player = devices.spawn(devices.source, False)
        children.append(player)
        feeder = threading.Thread(target=play_paced, args=(player, pcm, finished, result["errors"]), daemon=True)
        feeder.start()
        deadline = time.monotonic() + len(pcm) / 32000 + args.tail_timeout + 3
        while time.monotonic() < deadline:
            polled = time.monotonic()
            status, _ = api.api("status")
            now = time.monotonic()
            polls.append(now - previous_poll)
            previous_poll = now
            if not args.routing_only and status.get("session_id") != active.get("id"):
                raise ProbeError("The probe session changed outside this test")
            if status.get("last_error") or status["microphone"].get("device_error") or status["microphone"].get("processing_error"):
                raise ProbeError(status_failure(status, "runtime"))
            count = status["microphone"]["translated_samples"]
            if count != last_samples:
                first_engine = first_engine if first_engine is not None else now
                last_engine = now
                last_samples = count
            _, output_last = monitors[1].times()
            # Live Translate keeps emitting silent PCM after speech. Sample
            # counters therefore cannot identify the end of an utterance.
            if finished.is_set() and output_last is not None and now - output_last >= args.quiet_tail:
                break
            time.sleep(max(0, args.poll_interval - (time.monotonic() - polled)))
        else:
            result["errors"].append("Output completion observation reached its bounded timeout")
        input_first, input_last = monitors[0].times()
        output_first, output_last = monitors[1].times()
        result.update({
            "input_to_engine_first_audio_seconds": elapsed(input_first, first_engine),
            "input_to_output_first_sound_seconds": elapsed(input_first, output_first),
            "engine_to_output_first_sound_seconds": elapsed(first_engine, output_first),
            "input_observed_duration_seconds": elapsed(input_first, input_last),
            "output_observed_duration_seconds": elapsed(output_first, output_last),
            "input_end_to_output_end_seconds": elapsed(input_last, output_last),
            "input_to_engine_last_audio_seconds": elapsed(input_first, last_engine),
            "translated_samples": last_samples - baseline,
            "status_poll_max_seconds": round(max(polls, default=0), 4),
            "status_poll_median_seconds": round(statistics.median(polls), 4) if polls else None,
            **{name: status["microphone"][name] - count for name, count in initial_counters.items()},
        })
        if input_first is None or output_first is None:
            result["errors"].append("A monitor did not observe speech above the selected threshold")
        elif output_first < input_first - 0.05:
            result["errors"].append("Output began before the supplied input; this is not a valid translation-onset measurement")
        if any(monitor.failed for monitor in monitors):
            result["errors"].append("A software monitor failed")
    except ProbeError as error:
        result["errors"].append(str(error))
    finally:
        try:
            stop_owned_session(api, active)
        finally:
            for child in reversed(children):
                devices.stop(child)
            if feeder:
                feeder.join(timeout=2)
            for monitor in monitors:
                monitor.thread.join(timeout=2)
            if writer:
                writer.close()
    return result


def stop_owned_session(api, active):
    if not active:
        return
    status, _ = api.api("status")
    owned = status.get("session_id") == active.get("id") if active.get("id") else status.get("session_name") == active.get("name")
    if status["running"] and owned:
        api.api("stop", "POST", timeout=45)
    active.clear()


def restore_agent(api, original, applied):
    current, revision = api.api("agent")
    restore, conflicts = restore_patch(original, applied, current)
    if restore != current:
        api.api("agent", "PUT", restore, revision, timeout=40)
    return not conflicts


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--speech", type=Path, required=True, help="Explicitly selected synthetic mono PCM16/16 kHz WAV")
    parser.add_argument("--provider", choices=("gemini", "openai"), required=True)
    parser.add_argument("--model", help="Temporarily override only the selected provider model")
    parser.add_argument("--local-bridge", help="Explicit local benchmark bridge; receives the selected credential in RAM")
    parser.add_argument("--credential-provider", choices=("gemini", "openai"),
                        help="Credential profile to pass to the explicit local benchmark bridge")
    parser.add_argument("--playback-queue-ms", type=int, help="Temporary translated playback capacity; not a startup prefill")
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--routing-only", action="store_true", help="Measure original routing; no cloud API request")
    parser.add_argument("--json", type=Path)
    parser.add_argument("--capture-output", type=Path, help="Optional directory for synthetic output WAVs")
    parser.add_argument("--poll-interval", type=float, default=0.02)
    parser.add_argument("--tail-timeout", type=float, default=30)
    parser.add_argument("--quiet-tail", type=float, default=3)
    parser.add_argument("--threshold-db", type=float, default=-45)
    args = parser.parse_args()
    if sys.platform != "linux":
        raise ProbeError("This software-monitor harness requires Linux; it does not validate native Windows/macOS audio")
    if not 1 <= args.trials <= 10 or not 0.01 <= args.poll_interval <= 1 or not 5 <= args.tail_timeout <= 90 or not 2 <= args.quiet_tail <= min(10, args.tail_timeout) or not -70 <= args.threshold_db <= -20:
        raise ProbeError("Use 1..10 trials, 10..1000 ms polling, 5..90 s timeout, 2..10 s quiet tail and -70..-20 dB threshold")
    for tool in ("pactl", "pacat", "parec"):
        if not shutil.which(tool):
            raise ProbeError(f"Required audio client tool is unavailable: {tool}")
    args.threshold = max(1, round(32768 * 10 ** (args.threshold_db / 20)))
    if args.model and (len(args.model) > 200 or not all(ch.isascii() and (ch.isalnum() or ch in "-._/") for ch in args.model)):
        raise ProbeError("Model names must be 1..200 ASCII letters, digits or -._/ characters")
    if args.playback_queue_ms is not None and not 100 <= args.playback_queue_ms <= 5_000:
        raise ProbeError("Playback capacity must be 100..5000 ms")
    validate_local_bridge(args.local_bridge, args.provider, args.credential_provider)
    pcm, duration = read_fixture(args.speech)
    api = Dashboard()
    api.ensure_idle()
    original, revision = api.api("config")
    if not revision:
        raise ProbeError("The dashboard does not support configuration revision checks")
    agent_original, agent_revision = api.api("agent")
    if not agent_revision:
        raise ProbeError("The dashboard does not support independent command configuration revisions")
    agent_applied = copy.deepcopy(agent_original)
    agent_applied["enabled"] = False
    if not args.routing_only:
        profile = original["providers"][args.credential_provider or args.provider]
        presence, _ = api.api("credentials?" + urlencode({"api_key_env": profile["api_key_env"]}))
        if not presence["configured"]:
            raise ProbeError("Enter this provider's API key through Babel settings before running the test")
    if args.capture_output:
        args.capture_output.mkdir(parents=True, exist_ok=True)
    report = {"provider": args.provider, "model": args.model or original["providers"][args.provider]["model"],
              "routing_only": args.routing_only, "fixture_seconds": duration,
              "fixture_pcm_sha256": hashlib.sha256(pcm).hexdigest(), "platform": "linux",
              "threshold_dbfs": args.threshold_db, "poll_interval_seconds": args.poll_interval,
              "completion_quiet_seconds": args.quiet_tail, "trials": [], "errors": [],
              "limitations": ["Software monitor timestamps include audio client scheduling, not acoustic latency.",
                              "Engine first audio is observed by HTTP polling, not an exact provider response timestamp.",
                              "Engine-to-output may appear negative within polling/scheduling error and is not an exact causal delay.",
                              "First sound can include model leading silence and need not complete a translated phrase.",
                              "Completion is inferred from a quiet tail; the dashboard exposes no translation turn-complete event.",
                              "Existing microphone languages, prompt, audio quality and provider profile are preserved."]}
    devices = AudioDevices()
    applied = copy.deepcopy(original)
    if args.model:
        applied["providers"][args.provider]["model"] = args.model
    if args.local_bridge:
        profile = applied["providers"][args.provider]
        profile["endpoint"] = args.local_bridge
        profile["api_key_env"] = original["providers"][args.credential_provider]["api_key_env"]
        report["local_test_bridge"] = True
    if args.playback_queue_ms is not None:
        applied["audio"]["playback_queue_ms"] = args.playback_queue_ms
    report["playback_queue_ms"] = applied["audio"]["playback_queue_ms"]
    report["audio_quality"] = applied["audio"]["quality"]
    applied["microphone"].update(enabled=not args.routing_only, provider=args.provider,
                                  capture_device=devices.source + ".monitor", playback_device=devices.destination)
    applied["speaker"].update(enabled=False, capture_device=devices.speaker_source + ".monitor",
                              playback_device=devices.speaker_destination)
    applied["audio"]["microphone_source"] = "physical_microphone"
    for key in ("transcription", "recording", "history", "agent"):
        applied[key]["enabled"] = False
    active = {}
    attempted = False
    agent_attempted = False
    try:
        # /config intentionally ignores agent settings. Its independent API is
        # required to ensure synthetic speech can never trigger an MCP tool.
        if agent_original["enabled"]:
            agent_attempted = True
            api.api("agent", "PUT", agent_applied, agent_revision, timeout=40)
        devices.start()
        api.ensure_idle()
        attempted = True
        _, revision = api.api("config", "PUT", applied, revision, timeout=40)
        if not revision:
            raise ProbeError("The dashboard omitted the updated configuration revision")
        for index in range(1, args.trials + 1):
            trial = run_trial(api, devices, pcm, args, index, revision, active)
            report["trials"].append(trial)
            if trial["errors"]:
                break
    except ProbeError as error:
        report["errors"].append(str(error))
    finally:
        try:
            stop_owned_session(api, active)
            if attempted:
                current, revision = api.api("config")
                restore, conflicts = restore_patch(original, applied, current)
                if restore != current:
                    api.api("config", "PUT", restore, revision, timeout=40)
                if conflicts:
                    report["errors"].append("Concurrent setting changes were preserved; review settings before resuming")
                report["configuration_restored"] = not conflicts
        except ProbeError as error:
            report["configuration_restored"] = False
            report["errors"].append("Configuration cleanup: " + str(error))
        try:
            devices.close()
        except ProbeError as error:
            report["errors"].append(str(error))
        if agent_attempted:
            try:
                if not restore_agent(api, agent_original, agent_applied):
                    report["configuration_restored"] = False
                    report["errors"].append("Concurrent voice command settings were preserved; review settings before resuming")
            except ProbeError as error:
                report["configuration_restored"] = False
                report["errors"].append("Voice command configuration cleanup: " + str(error))
    report["ok"] = bool(report["trials"]) and not report["errors"] and all(not trial["errors"] for trial in report["trials"])
    encoded = json.dumps(report, indent=2) + "\n"
    if args.json:
        args.json.write_text(encoded)
    print(encoded, end="")
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        sys.exit(main())
    except ProbeError as error:
        print("Latency probe: " + str(error), file=sys.stderr)
        sys.exit(2)
    except KeyboardInterrupt:
        print("Latency probe interrupted; cleanup was attempted", file=sys.stderr)
        sys.exit(130)
