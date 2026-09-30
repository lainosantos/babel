"""Synthetic smoke validation contracts; no models or network required."""
import hashlib
from pathlib import Path
import struct
import sys
import tempfile
import time
import unittest

import test_bundled_inference as smoke


def wav(streaming=False, samples=(0.1, -0.25, 0.5)):
    data = b"".join(struct.pack("<f", sample) for sample in samples)
    return (b"RIFF" + struct.pack("<I", 0x7FFFF024 if streaming else 36 + len(data))
            + b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 3, 1, 22050, 88200, 4, 32)
            + b"data" + struct.pack("<I", 0x7FFFF000 if streaming else len(data)) + data)


class InferenceContracts(unittest.TestCase):
    def test_native_wav_accepts_exact_streaming_header_and_finite_actual_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "audio.wav"
            for streaming in (False, True):
                path.write_bytes(wav(streaming))
                self.assertEqual(smoke.validate_wave(path), (22050, 3))
            for invalid in [wav(True)[:-1], wav(samples=(float("nan"),)), wav(samples=(0., 0.))]:
                path.write_bytes(invalid)
                with self.assertRaises(ValueError):
                    smoke.validate_wave(path)
            broken = bytearray(wav(True))
            struct.pack_into("<I", broken, 40, 0x7FFFF001)
            path.write_bytes(broken)
            with self.assertRaises(ValueError):
                smoke.validate_wave(path)

    def test_cache_uses_babel_hash_name_and_preserves_mismatched_existing_models(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            data = b"pinned model"
            asset = {"name": "test.bin", "sha256": hashlib.sha256(data).hexdigest(), "size": len(data)}
            path = cache / f"{asset['sha256'][:16]}-test.bin"
            path.write_bytes(data)
            self.assertEqual(smoke.checked_model(asset, cache), path.resolve())
            path.write_bytes(b"corrupted")
            with self.assertRaisesRegex(ValueError, "pinned hash"):
                smoke.checked_model(asset, cache)
            self.assertEqual(path.read_bytes(), b"corrupted")

    def test_child_readiness_is_bounded_and_context_always_stops_the_process(self):
        command = [sys.executable, "-u", "-c", 'import time; print("BABEL_SERVICE_READY {}", flush=True); time.sleep(30)']
        with smoke.Child(command, 5) as child:
            self.assertEqual(child.readiness(b"BABEL_SERVICE_READY "), {})
            process = child.process
        self.assertIsNotNone(process.poll())
        oversized = [sys.executable, "-u", "-c", 'import time; print("x"*9000, flush=True); time.sleep(30)']
        with smoke.Child(oversized, 5) as child:
            with self.assertRaisesRegex(ValueError, "line limit"):
                child.readiness(b"BABEL_SERVICE_READY ")

    def test_readiness_cannot_redirect_synthetic_requests_away_from_owned_loopback(self):
        class FakeChild:
            process = type("Process", (), {"pid": 123})()
            def readiness(self, _):
                return {"service": "babel-whisper", "endpoint": "http://remote.invalid:123/inference", "port": 123}
        with self.assertRaisesRegex(ValueError, "endpoint"):
            smoke.ready_http(FakeChild(), "babel-whisper", "/inference")

    def test_startup_failure_reports_native_diagnostic_and_exit_code(self):
        command = [sys.executable, "-u", "-c",
                   'import sys; print("synthetic model load failure", file=sys.stderr); sys.exit(7)']
        with smoke.Child(command, 5) as child:
            with self.assertRaises(EOFError) as failed:
                child.readiness(b"BABEL_SERVICE_READY ")
            self.assertIn(Path(sys.executable).name, str(failed.exception))
            self.assertIn("exit code 7", str(failed.exception))
            self.assertIn("synthetic model load failure", str(failed.exception))
        self.assertFalse(child.stderr_reader.is_alive())

    def test_stderr_flood_is_drained_without_unbounded_failure_output(self):
        command = [sys.executable, "-u", "-c",
                   'import sys; sys.stderr.write("discard this prefix\\n" + "x" * 262144 + "\\nfinal native error\\n"); sys.exit(9)']
        with smoke.Child(command, 5) as child:
            with self.assertRaises(EOFError) as failed:
                child.readiness(b"BABEL_SERVICE_READY ")
            message = str(failed.exception)
            self.assertIn("exit code 9", message)
            self.assertIn("final native error", message)
            self.assertNotIn("discard this prefix", message)
            self.assertLessEqual(len(child.stderr_tail), smoke.MAX_STDERR)
            self.assertLess(len(message), smoke.MAX_STDERR + 512)
        self.assertFalse(child.stderr_reader.is_alive())

    def test_timed_out_helper_reports_its_identity_and_is_stopped(self):
        command = [sys.executable, "-u", "-c",
                   'import time; print("fixture is running", flush=True); time.sleep(30)']
        with smoke.Child(command, 5) as child:
            self.assertEqual(child.line(time.monotonic() + 5), b"fixture is running")
            with self.assertRaises(TimeoutError) as failed:
                child.line(time.monotonic() + 0.05)
            self.assertIn(Path(sys.executable).name, str(failed.exception))
            self.assertIn("still running", str(failed.exception))
        self.assertIsNotNone(child.process.poll())


if __name__ == "__main__":
    unittest.main()
