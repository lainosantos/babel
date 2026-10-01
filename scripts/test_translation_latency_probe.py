"""Latency harness safety tests; no audio device, network or model is used."""
import copy
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import wave

sys.path.insert(0, str(Path(__file__).resolve().parent))
import measure_translation_latency as probe


class LatencyProbeTests(unittest.TestCase):
    def test_restore_preserves_unrelated_and_conflicting_user_edits(self):
        original = {"microphone": {"capture": "user-mic", "enabled": False},
                    "interface": {"language": "system"}}
        applied = copy.deepcopy(original)
        applied["microphone"].update(capture="probe-monitor", enabled=True)
        current = copy.deepcopy(applied)
        current["microphone"]["capture"] = "new-user-mic"
        current["interface"]["language"] = "pt"
        restored, conflicts = probe.restore_patch(original, applied, current)
        self.assertEqual(conflicts, ["microphone.capture"])
        self.assertEqual(restored["microphone"], {"capture": "new-user-mic", "enabled": False})
        self.assertEqual(restored["interface"]["language"], "pt")
        self.assertEqual(current["microphone"]["enabled"], True)

    def test_ambiguous_config_failure_can_restore_only_applied_leaves(self):
        original = {"audio": {"source": "physical"}, "providers": {"gemini": {"model": "original"}}}
        applied = copy.deepcopy(original)
        applied["providers"]["gemini"]["model"] = "comparison"
        self.assertEqual(probe.restore_patch(original, applied, original), (original, []))
        self.assertEqual(probe.restore_patch(original, applied, applied), (original, []))

    def test_dashboard_rejects_remote_or_credential_bearing_urls(self):
        for url in ("https://example.com:8080", "http://127.0.0.1:9/#token=secret",
                    "http://user:secret@127.0.0.1:9", "http://127.0.0.1:9?key=secret",
                    "http://127.0.0.1", "http://127.0.0.1:9/settings"):
            with self.subTest(url=url), patch.dict(probe.os.environ, {"BABEL_DASHBOARD_URL": url,
                                                                  "BABEL_DASHBOARD_TOKEN": "private"}):
                with self.assertRaises(probe.ProbeError) as failure:
                    probe.Dashboard()
                self.assertNotIn("secret", str(failure.exception))
                self.assertNotIn("private", str(failure.exception))

    def test_child_process_environment_does_not_receive_dashboard_token(self):
        with patch.dict(probe.os.environ, {"BABEL_DASHBOARD_TOKEN": "private", "BABEL_DASHBOARD_URL": "http://127.0.0.1:12345"}):
            devices = probe.AudioDevices()
        self.assertNotIn("BABEL_DASHBOARD_TOKEN", devices.env)
        self.assertNotIn("BABEL_DASHBOARD_URL", devices.env)
        self.assertEqual(len({devices.source, devices.destination,
                              devices.speaker_source, devices.speaker_destination}), 4)

    def test_diagnostics_never_export_raw_error_contents(self):
        status = {"last_error": "private URL or provider response"}
        self.assertEqual(probe.status_failure(status, "runtime"),
                         "Babel reported a runtime error; inspect its dashboard locally")
        status["last_error"] = "Playback queue is full. private URL or provider response"
        self.assertEqual(probe.status_failure(status, "runtime"),
                         "Translated playback capacity was exceeded; this trial is invalid")

    def test_credential_handoff_requires_explicit_loopback_test_bridge(self):
        probe.validate_local_bridge(None, "gemini", None)
        probe.validate_local_bridge("ws://127.0.0.1:12345/unique-test", "openai", "gemini")
        for endpoint, provider, credential in [
            (None, "openai", "gemini"),
            ("wss://example.com/test", "openai", "gemini"),
            ("ws://127.0.0.1:12345/test", "gemini", "gemini"),
            ("ws://127.0.0.1:12345/test", "openai", None),
            ("ws://127.0.0.1:12345/", "openai", "gemini"),
            ("ws://secret@127.0.0.1:12345/test", "openai", "gemini"),
            ("ws://127.0.0.1:12345/test?secret=value", "openai", "gemini"),
            ("ws://127.0.0.1:bad/test", "openai", "gemini"),
        ]:
            with self.subTest(endpoint=endpoint), self.assertRaises(probe.ProbeError) as failure:
                probe.validate_local_bridge(endpoint, provider, credential)
            self.assertNotIn("secret", str(failure.exception))

    def test_cleanup_never_stops_an_unrelated_session(self):
        class API:
            def __init__(self):
                self.calls = []

            def api(self, path, *args, **kwargs):
                self.calls.append(path)
                return {"running": True, "session_id": "other-id", "session_name": "Another session"}, None

        api = API()
        active = {"name": "Synthetic latency probe", "id": "our-id"}
        probe.stop_owned_session(api, active)
        self.assertEqual(api.calls, ["status"])
        self.assertEqual(active, {})

    def test_cleanup_handles_start_response_timeout_by_owned_session_name(self):
        class API:
            def __init__(self):
                self.calls = []

            def api(self, path, *args, **kwargs):
                self.calls.append(path)
                return {"running": True, "session_id": "started", "session_name": "probe-unique"}, None

        api = API()
        probe.stop_owned_session(api, {"name": "probe-unique"})
        self.assertEqual(api.calls, ["status", "stop"])

    def test_commands_restore_uses_independent_revision_and_preserves_new_settings(self):
        class API:
            def __init__(self):
                self.calls = []

            def api(self, *args, **kwargs):
                self.calls.append((args, kwargs))
                return {"enabled": False, "wake_name": "New user name"}, '"8"'

        api = API()
        restored = probe.restore_agent(api, {"enabled": True, "wake_name": "Babel"},
                                       {"enabled": False, "wake_name": "Babel"})
        self.assertTrue(restored)
        self.assertEqual(api.calls[1][0], ("agent", "PUT", {"enabled": True, "wake_name": "New user name"}, '"8"'))

    def test_fixture_requires_explicit_bounded_pcm_format(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "synthetic.wav"
            with wave.open(str(fixture), "wb") as output:
                output.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
                output.writeframes(b"\x01\x00" * 16000)
            pcm, duration = probe.read_fixture(fixture)
            self.assertEqual((len(pcm), duration), (32000, 1))
            with wave.open(str(fixture), "wb") as output:
                output.setparams((2, 2, 16000, 0, "NONE", "not compressed"))
                output.writeframes(b"\x01\x00" * 32000)
            with self.assertRaises(probe.ProbeError):
                probe.read_fixture(fixture)


if __name__ == "__main__":
    unittest.main()
