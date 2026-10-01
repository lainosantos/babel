"""Synthetic cascade safety/protocol checks without cloud requests or audio devices."""
import asyncio
import base64
from contextlib import suppress
import io
import json
import logging
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import wave

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gemini_cascade_bridge as bridge


class Response:
    def __init__(self, chunks):
        self.chunks = chunks

    async def aiter_bytes(self, chunk_size):
        for chunk in self.chunks:
            yield chunk


def event(kind, **fields):
    return {"event_type": kind, **fields}


class BridgeTests(unittest.TestCase):
    def test_network_child_debug_cannot_log_synthetic_authorization(self):
        captured = io.StringIO()
        handler = logging.StreamHandler(captured)
        child = logging.getLogger("websockets.server")
        saved = (child.level, child.disabled, child.propagate, list(child.handlers),
                 logging.root.manager.disable)
        child.disabled = False
        child.propagate = False
        child.setLevel(logging.DEBUG)
        child.handlers = [handler]
        logging.disable(logging.NOTSET)
        try:
            child.debug("before-private-scope")
            with bridge.private_network_logs() as logger:
                child.debug("< Authorization: Bearer synthetic-regression-credential")
                # A child's explicit DEBUG setting must not bypass suppression.
                child.setLevel(logging.DEBUG)
                child.error("handshake rejected: %s", "synthetic-regression-credential")
                logger.debug("< Authorization: Bearer synthetic-regression-credential")
            child.debug("after-private-scope")
            self.assertEqual(captured.getvalue(), "before-private-scope\nafter-private-scope\n")
            self.assertEqual(logging.root.manager.disable, logging.NOTSET)
        finally:
            child.setLevel(saved[0])
            child.disabled, child.propagate, child.handlers = saved[1:4]
            logging.disable(saved[4])

    def test_segmentation_preserves_interior_silence_and_onset_prefix(self):
        segmenter = bridge.Segmenter()
        quiet = b"\0\0" * 240
        speech = b"\xb0\x04" * 240
        result = []
        source = [quiet] * 10 + [speech] * 20 + [quiet] * 12 + [speech] * 20 + [quiet] * 40
        for index, frame in enumerate(source):
            result.extend(segmenter.feed(frame, index / 100))
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0].pcm, b"".join(source))
        self.assertEqual(result[0].reason, "silence")
        self.assertEqual(result[0].first_speech_at, 0.1)
        self.assertFalse(segmenter.active_speech)

    def test_fixed_windows_preserve_samples_and_order(self):
        segmenter = bridge.Segmenter(2)
        speech = b"\xb0\x04" * 240
        quiet = b"\0\0" * 240
        source = [speech] * 230 + [quiet] * 40
        result = []
        for index, frame in enumerate(source):
            result.extend(segmenter.feed(frame, index / 100))
        self.assertEqual([segment.index for segment in result], [1, 2])
        self.assertEqual([segment.reason for segment in result], ["fixed_window", "silence"])
        self.assertEqual(b"".join(segment.pcm for segment in result), b"".join(source))
        self.assertEqual(len(result[0].pcm), 2 * 24000 * 2)

    def test_idle_silence_does_not_create_cloud_jobs(self):
        segmenter = bridge.Segmenter(2)
        self.assertEqual(segmenter.feed(b"\0\0" * 24000, 1), [])
        self.assertIsNone(segmenter.first_speech_at)
        self.assertIsNone(segmenter.finish(2))

    def test_input_decode_checks_format_and_bounds_without_raw_diagnostics(self):
        self.assertEqual(bridge.decode_input({"type": "input_audio_buffer.append", "audio": "AQI="}), b"\1\2")
        for value in ({"type": "invalid-secret"},
                      {"type": "input_audio_buffer.append", "audio": "secret?"},
                      {"type": "input_audio_buffer.append", "audio": "AQ=="},
                      {"type": "input_audio_buffer.append", "audio": "A" * 70000}):
            with self.assertRaises(bridge.BridgeError) as error:
                bridge.decode_input(value)
            self.assertNotIn("secret", str(error.exception))

    def test_translation_uses_actual_audio_and_minimal_thinking(self):
        pcm = b"\1\0" * 240
        request = bridge.text_request(bridge.Segment(1, pcm, 0, 1, "silence"))
        self.assertEqual(request["generationConfig"]["thinkingConfig"]["thinkingLevel"], "minimal")
        payload = request["contents"][0]["parts"][0]["inlineData"]
        self.assertEqual(payload["mimeType"], "audio/wav")
        with wave.open(io.BytesIO(base64.b64decode(payload["data"]))) as wav:
            self.assertEqual(wav.getframerate(), 24000)
            self.assertEqual(wav.readframes(1000), pcm)
        self.assertNotIn("Bom dia", json.dumps(request))

    def test_tts_request_matches_documented_interactions_audio_schema(self):
        body = bridge.tts_request("Good morning.")
        self.assertEqual(body["input"], [{"type": "user_input", "content": [{"type": "text", "text": "Good morning."}]}])
        self.assertEqual(body["response_format"], {"type": "audio", "mime_type": "audio/l16", "sample_rate": 24000})
        self.assertEqual(body["generation_config"]["speech_config"], [{"voice": "Kore"}])
        self.assertTrue(body["stream"])
        self.assertFalse(body["store"])

    def test_text_fragments_ignore_thought_and_require_complete_generation(self):
        payload = {"candidates": [{"content": {"parts": [{"text": "secret reasoning", "thought": True}, {"text": "Hello"}]}, "finishReason": "STOP"}]}
        self.assertEqual(bridge.text_fragments(payload), (["Hello"], True))
        with self.assertRaises(bridge.BridgeError):
            bridge.text_fragments({"candidates": [{"finishReason": "MAX_TOKENS"}]})

    def test_tts_audio_accepts_documented_pcm_but_rejects_encoded_files(self):
        value = event("step.delta", delta={"type": "audio", "data": "AQI=", "mime_type": "audio/l16"})
        self.assertEqual(bridge.audio_delta(value), b"\1\2")
        for data in (b"RIFFabcd", b"OggSabcd", b"ID3abc"):
            value["delta"]["data"] = base64.b64encode(data).decode()
            with self.assertRaises(bridge.BridgeError):
                bridge.audio_delta(value)
        value["delta"] = {"type": "audio", "data": "AQI=", "mime_type": "audio/mpeg"}
        with self.assertRaises(bridge.BridgeError):
            bridge.audio_delta(value)

    def test_report_files_have_private_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            bridge.private_json(path, {"status": "complete"})
            self.assertEqual(json.loads(path.read_text()), {"status": "complete"})
            if os.name == "posix":
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)


class AsyncBridgeTests(unittest.IsolatedAsyncioTestCase):
    async def test_real_loopback_handshake_does_not_log_synthetic_authorization(self):
        try:
            from websockets.asyncio.client import connect
        except ImportError:
            self.skipTest("Install benchmark websockets dependency for loopback handshake test")
        captured = io.StringIO()
        handler = logging.StreamHandler(captured)
        child = logging.getLogger("websockets.server")
        saved = (child.level, child.disabled, child.propagate, list(child.handlers),
                 logging.root.manager.disable)
        child.disabled = False
        child.propagate = False
        child.setLevel(logging.DEBUG)
        child.handlers = [handler]
        logging.disable(logging.NOTSET)
        async def synthetic_complete(cascade):
            self.assertEqual(cascade.key, "synthetic-regression-credential")
            # Exercise the configured child logger even though serve receives
            # its own silent logger. No provider or HTTP client is invoked.
            child.debug("< Authorization: Bearer %s", cascade.key)
        task = None
        try:
            with tempfile.TemporaryDirectory() as directory:
                args = SimpleNamespace(segment_seconds=0, ready_file=Path(directory) / "ready.json",
                                       report_file=Path(directory) / "report.json")
                instance = bridge.Bridge(args)
                loop = asyncio.get_running_loop()
                with patch.object(bridge.Cascade, "run", synthetic_complete), \
                        patch.object(loop, "add_signal_handler"):
                    task = asyncio.create_task(instance.run())
                    async with asyncio.timeout(5):
                        while not args.ready_file.exists():
                            if task.done():
                                await task
                            await asyncio.sleep(.01)
                        endpoint = json.loads(args.ready_file.read_text())["endpoint"]
                        async with connect(endpoint, additional_headers={
                                "Authorization": "Bearer synthetic-regression-credential"}) as socket:
                            await socket.wait_closed()
                    task.cancel()
                    with suppress(asyncio.CancelledError):
                        await task
                self.assertEqual(instance.report["connections"][0]["status"], "closed")
                self.assertEqual(captured.getvalue(), "")
                self.assertNotIn("synthetic-regression-credential", args.report_file.read_text())
                self.assertEqual(logging.root.manager.disable, logging.NOTSET)
        finally:
            if task and not task.done():
                task.cancel()
                with suppress(asyncio.CancelledError):
                    await task
            child.setLevel(saved[0])
            child.disabled, child.propagate, child.handlers = saved[1:4]
            logging.disable(saved[4])

    async def test_generated_text_is_only_saved_with_explicit_synthetic_opt_in(self):
        for include_text in (False, True):
            cascade = bridge.Cascade(None, "secret", 0, {"segments": []}, lambda: None,
                                     include_synthetic_text=include_text)
            cascade.segmenter.first_speech_at = 1
            async def request(*args):
                yield {"candidates": [{"content": {"parts": [{"text": "Good morning."}]},
                                       "finishReason": "STOP"}]}
            cascade.request = request
            record = {}
            text = await cascade.translated_text(None, bridge.Segment(1, b"\1\0", 1, 2, "silence"), record)
            self.assertEqual(text, "Good morning.")
            self.assertEqual("synthetic_translation" in record, include_text)
            self.assertNotIn("secret", json.dumps(record))

    async def test_sse_parser_handles_partial_multiline_and_done(self):
        response = Response([b': keepalive\ndata: {"ca', b'ndidates":\ndata: []}\n\ndata: [DONE]\n\n'])
        self.assertEqual([value async for value in bridge.sse_events(response)], [{"candidates": []}])

    async def test_sse_parser_rejects_truncated_error_and_oversized_data(self):
        for chunks in ([b'data: {"private":1}'], [b'data: {"error":"secret"}\n\n'], [b'data: ' + b'X' * 300]):
            with patch.object(bridge, "MAX_EVENT_BYTES", 128), self.assertRaises(bridge.BridgeError) as error:
                _ = [value async for value in bridge.sse_events(Response(chunks))]
            self.assertNotIn("secret", str(error.exception))

    async def test_only_fixed_official_endpoints_receive_credentials(self):
        cascade = bridge.Cascade(None, "secret-key", 0, {"segments": []}, lambda: None)
        with self.assertRaises(bridge.BridgeError) as error:
            _ = [event async for event in cascade.request(None, "https://evil.invalid", {})]
        self.assertNotIn("secret-key", str(error.exception))

    async def test_forward_first_frame_immediate_and_later_frames_paced(self):
        class Socket:
            def __init__(self):
                self.sent = []
            async def send(self, value):
                self.sent.append(json.loads(value))
        socket = Socket()
        cascade = bridge.Cascade(socket, "secret", 0, {"segments": []}, lambda: None)
        cascade.segmenter.first_speech_at = 100
        clock = [100.0]
        sleeps = []
        async def sleep(duration):
            sleeps.append(duration)
            clock[0] += duration
        with patch.object(bridge.time, "monotonic", side_effect=lambda: clock[0]), patch.object(bridge.asyncio, "sleep", side_effect=sleep):
            record = {}
            await cascade.forward(b"\1\0" * 1440, record)
        self.assertEqual(len(socket.sent), 3)
        self.assertEqual(len(sleeps), 2)
        self.assertAlmostEqual(sum(sleeps), .04)
        self.assertEqual(record["first_forwarded_pcm_seconds"], 0)
        self.assertNotIn("secret", json.dumps(socket.sent))

    async def test_pacing_does_not_accumulate_sleep_overhead(self):
        class Socket:
            async def send(self, value):
                pass
        cascade = bridge.Cascade(Socket(), "secret", 0, {"segments": []}, lambda: None)
        cascade.segmenter.first_speech_at = 100
        clock = [100.0]
        sleeps = []
        async def sleep(duration):
            sleeps.append(duration)
            clock[0] += duration + .002
        with patch.object(bridge.time, "monotonic", side_effect=lambda: clock[0]), patch.object(bridge.asyncio, "sleep", side_effect=sleep):
            await cascade.forward(b"\1\0" * (480 * 100), {})
        self.assertEqual(len(sleeps), 99)
        self.assertAlmostEqual(cascade.playback_deadline, 102.0)
        self.assertAlmostEqual(clock[0], 101.982)

    async def test_completed_work_survives_abrupt_client_disconnect(self):
        try:
            from websockets.exceptions import ConnectionClosedError
        except ImportError:
            self.skipTest("Install benchmark websockets dependency for disconnect classification")
        instance = bridge.Bridge(SimpleNamespace(segment_seconds=0))
        instance.persist = lambda: None
        socket = SimpleNamespace(request=SimpleNamespace(headers={"Authorization": "Bearer secret-key"}), close_code=1006)
        async def disconnected(cascade):
            cascade.report.update(captured_segments=1, completed_segments=1,
                                  pending_segments_on_disconnect=0,
                                  unfinished_speech_on_disconnect=False)
            raise ConnectionClosedError(None, None)
        with patch.object(bridge.Cascade, "run", disconnected):
            await instance.connection(socket)
        record = instance.report["connections"][0]
        self.assertEqual(record["status"], "client_disconnected")
        self.assertEqual(record["close_code"], 1006)
        self.assertEqual(record["completed_segments"], 1)
        self.assertNotIn("secret", json.dumps(instance.report))
        self.assertFalse(instance.active)

    async def test_http_warmup_precedes_ready_and_reuses_long_lived_client(self):
        try:
            import httpx
        except ImportError:
            self.skipTest("Install benchmark HTTP dependency for client setup test")
        events = []
        class Client:
            async def __aenter__(self):
                return self
            async def __aexit__(self, *args):
                events.append("client_closed")
        class Socket:
            async def send(self, payload):
                events.append(json.loads(payload)["type"])
            async def recv(self):
                return json.dumps({"type": "session.update"})
        client = Client()
        cascade = bridge.Cascade(Socket(), "secret", 0, {"segments": []}, lambda: None, warm_http=True)
        async def warm(actual):
            self.assertIs(actual, client)
            events.append("warm")
        async def process(actual):
            self.assertIs(actual, client)
            events.append("process")
        async def receive():
            await asyncio.Future()
        cascade.warm_connection = warm
        cascade.process = process
        cascade.receive = receive
        with patch.object(httpx, "AsyncClient", return_value=client) as factory:
            await cascade.run()
        self.assertLess(events.index("warm"), events.index("session.updated"))
        self.assertLess(events.index("session.updated"), events.index("process"))
        self.assertEqual(factory.call_args.kwargs["limits"].keepalive_expiry, 120)
        self.assertFalse(factory.call_args.kwargs["follow_redirects"])
        self.assertFalse(factory.call_args.kwargs["trust_env"])
        self.assertEqual(cascade.key, "")

    async def test_http_warmup_uses_fixed_get_and_bounded_metadata(self):
        class Metadata(Response):
            status_code = 200
            async def __aenter__(self):
                return self
            async def __aexit__(self, *args):
                pass
        class Client:
            def __init__(self, size):
                self.size = size
            def stream(self, method, endpoint, headers):
                self.request = method, endpoint, headers
                return Metadata([b"X" * self.size])
        cascade = bridge.Cascade(None, "secret", 0, {"segments": []}, lambda: None, warm_http=True)
        client = Client(100)
        await cascade.warm_connection(client)
        self.assertEqual(client.request[0], "GET")
        self.assertEqual(client.request[1], bridge.WARM_ENDPOINT)
        self.assertIn("http_warmup_seconds", cascade.report)
        with self.assertRaises(bridge.BridgeError):
            await cascade.warm_connection(Client(256 * 1024 + 1))
        self.assertNotIn("secret", json.dumps(cascade.report))

    async def test_completed_event_required_after_synthesis(self):
        cascade = bridge.Cascade(None, "secret", 0, {"segments": []}, lambda: None)
        cascade.segmenter.first_speech_at = 1
        async def request(*args):
            yield event("step.delta", delta={"type": "audio", "data": "AQI="})
            yield event("interaction.completed")
        async def forward(pcm, record):
            record["first_forwarded_pcm_seconds"] = 1
        cascade.request = request
        cascade.forward = forward
        record = {}
        await cascade.synthesize(None, "Hello", record)
        self.assertIn("first_pcm_seconds", record)
        self.assertGreater(record["output_duration_seconds"], 0)

    async def test_guard_requires_private_path_and_one_bearer(self):
        class Headers:
            def __init__(self, values):
                self.values = values
            def get_all(self, name):
                return self.values
        class Connection:
            def respond(self, status, body):
                return int(status), body
        instance = bridge.Bridge(SimpleNamespace(segment_seconds=0))
        for path, values in (("/wrong", ["Bearer secret-key"]), (instance.path, []), (instance.path, ["Bearer secret-key"] * 2), (instance.path, ["Bearer secret key"])):
            result = await instance.guard(Connection(), SimpleNamespace(path=path, headers=Headers(values)))
            self.assertIn(result[0], (401, 404))
            self.assertNotIn("secret", result[1])
        self.assertIsNone(await instance.guard(Connection(), SimpleNamespace(path=instance.path, headers=Headers(["Bearer secret-key"]))))


if __name__ == "__main__":
    unittest.main()
