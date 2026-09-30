import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import ProxyHandler, Request, build_opener

from needle_bridge import ModelWorker, Planner, Server, make_handler

# These tests target sockets that they create on loopback. Never send their
# authorization headers through environment or macOS system proxy settings.
LOCAL_HTTP = build_opener(ProxyHandler({}))


class FakeNeedle:
    instances = []
    def __init__(self, **kwargs):
        self.kwargs = kwargs
        self.resets = 0
        self.calls = []
        self.closed = False
        self.instances.append(self)
    def close(self):
        self.closed = True
    def reset(self):
        self.resets += 1
    def complete(self, text, **kwargs):
        self.calls.append((text, kwargs))
        return {"success": True, "type": "call", "confidence": 0.99,
                "function_calls": [{"name": self.kwargs["tools"][0]["name"], "arguments": {}}]}
    def run(self, *_args, **_kwargs):
        raise AssertionError("adapter must never execute tools via Needle.run")


def payload():
    return {"text": "turn on the lights", "tools": [{"name": "lights_on", "parameters": {"type": "object"}, "triggers": [".*"]}]}


class BridgeTests(unittest.TestCase):
    def setUp(self):
        FakeNeedle.instances.clear()
        self.planner = Planner(factory=FakeNeedle)

    def test_reuses_model_but_resets_every_command_and_strips_triggers(self):
        self.planner.complete(payload())
        self.planner.complete(payload())
        self.assertEqual(len(FakeNeedle.instances), 1)
        self.assertEqual(FakeNeedle.instances[0].resets, 2)
        self.assertNotIn("triggers", FakeNeedle.instances[0].kwargs["tools"][0])
        changed = payload()
        changed["tools"][0]["name"] = "other"
        self.planner.complete(changed)
        self.assertEqual(len(FakeNeedle.instances), 2)
        self.assertTrue(FakeNeedle.instances[0].closed)

    def test_replacement_closes_previous_before_allocating_and_failure_does_not_reuse_it(self):
        self.planner.complete(payload())
        original = self.planner.agent
        def failing_factory(**_kwargs):
            self.assertTrue(original.closed)
            raise RuntimeError("no capacity")
        self.planner.factory = failing_factory
        changed = payload()
        changed["tools"][0]["name"] = "other"
        with self.assertRaises(RuntimeError):
            self.planner.complete(changed)
        self.assertIsNone(self.planner.agent)

    def test_idle_unload_observes_completed_work_and_does_not_interrupt_a_command(self):
        now = [0]
        planner = Planner(factory=FakeNeedle, clock=lambda: now[0])
        planner.complete(payload())
        previous = planner.agent
        now[0] = 59
        planner.unload_idle()
        self.assertIs(planner.agent, previous)
        planner.lock.acquire()
        try:
            now[0] = 90
            planner.unload_idle()
            self.assertFalse(previous.closed)
        finally:
            planner.lock.release()
        planner.complete(payload())
        now[0] = 149
        planner.unload_idle()
        self.assertIs(planner.agent, previous)
        now[0] = 150
        planner.unload_idle()
        self.assertTrue(previous.closed)
        self.assertIsNone(planner.agent)
        planner.complete(payload())
        self.assertIsNot(planner.agent, previous)

    def fake_worker_sdk(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        Path(directory.name, "needle.py").write_text('''
import os
import time
class Needle:
    def __init__(self, **kwargs):
        pass
    def reset(self):
        pass
    def complete(self, text, **kwargs):
        if text == "slow":
            time.sleep(10)
        return {"success": True, "worker_pid": os.getpid()}
''')
        environment = patch.dict(os.environ, {"PYTHONPATH": directory.name})
        environment.start()
        self.addCleanup(environment.stop)

    def test_http_idle_reaps_real_worker_keeps_socket_and_reloads_on_next_command(self):
        self.fake_worker_sdk()
        now = [0]
        planner = Planner(clock=lambda: now[0])
        server = Server(("127.0.0.1", 0), make_handler(planner), planner=planner)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = "http://127.0.0.1:%d" % server.server_port
        def complete():
            with LOCAL_HTTP.open(Request(url + "/complete", json.dumps(payload()).encode(),
                                         {"Content-Type": "application/json"}), timeout=5) as response:
                return json.load(response)
        def health():
            with LOCAL_HTTP.open(url + "/health", timeout=3) as response:
                return json.load(response)
        try:
            self.assertFalse(health()["model_loaded"])
            first = complete()
            worker = planner.agent.process
            self.assertEqual(first["worker_pid"], worker.pid)
            self.assertNotEqual(worker.pid, os.getpid())
            self.assertEqual(complete()["worker_pid"], worker.pid)
            now[0] = 61
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and health()["model_loaded"]:
                time.sleep(0.05)
            self.assertFalse(health()["model_loaded"])
            self.assertIsNotNone(worker.poll())
            self.assertNotEqual(complete()["worker_pid"], worker.pid)
            latest_worker = planner.agent.process
        finally:
            server.shutdown()
            server.server_close()
            thread.join(1)
        self.assertIsNotNone(latest_worker.poll())

    def test_worker_timeout_and_cancellation_reap_child_and_next_command_recovers(self):
        self.fake_worker_sdk()
        for cancel in [False, True]:
            with self.subTest(cancel=cancel):
                # Process startup is unrelated to the deadline under test and
                # can take longer when CI compiles Rust in parallel.
                planner = Planner(timeout_secs=10)
                planner.complete(payload())
                worker = planner.agent.process
                planner.timeout_secs = 10 if cancel else 0.2
                cancelled = threading.Event()
                slow = payload()
                slow["text"] = "slow"
                if cancel:
                    timer = threading.Timer(0.1, cancelled.set)
                    timer.start()
                try:
                    with self.assertRaisesRegex(RuntimeError, "cancelled or timed out"):
                        planner.complete(slow, cancelled=cancelled)
                    self.assertIsNone(planner.agent)
                    self.assertIsNotNone(worker.poll())
                    planner.timeout_secs = 10
                    self.assertTrue(planner.complete(payload())["success"])
                finally:
                    if cancel:
                        timer.join(1)
                    planner.close()

    def test_timeout_and_cancel_also_reap_a_worker_that_never_reads_large_load_schema(self):
        original_popen = subprocess.Popen
        for cancel in [False, True]:
            with self.subTest(cancel=cancel):
                spawned = []
                def non_reading_worker(_command, **kwargs):
                    child = original_popen([sys.executable, "-c", "import time; time.sleep(10)"], **kwargs)
                    spawned.append(child)
                    return child
                cancelled = threading.Event()
                if cancel:
                    timer = threading.Timer(0.1, cancelled.set)
                    timer.start()
                started = time.monotonic()
                try:
                    with patch("needle_bridge.subprocess.Popen", side_effect=non_reading_worker):
                        with self.assertRaisesRegex(RuntimeError, "cancelled or timed out"):
                            ModelWorker(deadline=time.monotonic() + (5 if cancel else 0.2),
                                        cancelled=cancelled, tools=[{"description": "x" * 128_000}])
                    self.assertLess(time.monotonic() - started, 4)
                    self.assertEqual(len(spawned), 1)
                    self.assertIsNotNone(spawned[0].poll())
                finally:
                    if cancel:
                        timer.join(1)
                    for child in spawned:
                        if child.poll() is None:
                            child.kill()
                            child.wait(timeout=3)

    def test_bridge_exit_reaps_inflight_worker_via_pipe_eof(self):
        self.fake_worker_sdk()
        planner = Planner(timeout_secs=5)
        planner.complete(payload())
        worker = planner.agent.process
        # Losing the owner's pipe, as with Rust killing the bridge on shutdown,
        # must also stop a model in the middle of native inference.
        worker.stdin.write(json.dumps({"operation": "complete", "text": "slow",
                                       "max_new_tokens": 32}).encode() + b"\n")
        worker.stdin.flush()
        worker.stdin.close()
        try:
            worker.wait(timeout=3)
        finally:
            planner.close()

    def test_http_disconnect_cancels_native_work_without_waiting_for_inference_timeout(self):
        self.fake_worker_sdk()
        planner = Planner(timeout_secs=20)
        planner.complete(payload())
        worker = planner.agent.process
        server = Server(("127.0.0.1", 0), make_handler(planner), planner=planner)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        connection = socket.create_connection(server.server_address, timeout=3)
        slow = payload()
        slow["text"] = "slow"
        body = json.dumps(slow).encode()
        try:
            connection.sendall(("POST /complete HTTP/1.0\r\nContent-Type: application/json\r\n"
                                f"Content-Length: {len(body)}\r\n\r\n").encode() + body)
            deadline = time.monotonic() + 3
            while not planner.lock.locked() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(planner.lock.locked())
            connection.close()
            worker.wait(timeout=3)
            deadline = time.monotonic() + 3
            while planner.lock.locked() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertFalse(planner.lock.locked())
            self.assertIsNone(planner.agent)
            with LOCAL_HTTP.open(f"http://127.0.0.1:{server.server_port}/health", timeout=3) as response:
                self.assertFalse(json.load(response)["model_loaded"])
        finally:
            connection.close()
            server.shutdown()
            server.server_close()
            thread.join(1)

    def test_rejects_malformed_and_duplicate_tools(self):
        bad = payload()
        bad["tools"].append(bad["tools"][0])
        with self.assertRaises(ValueError):
            self.planner.complete(bad)
        with self.assertRaises(ValueError):
            self.planner.complete({"text": "x", "tools": []})
        self.assertFalse(FakeNeedle.instances)

    def test_loopback_bind_never_waits_for_reverse_dns(self):
        with patch("socket.getfqdn", side_effect=AssertionError("loopback bind must not use DNS")):
            server = Server(("127.0.0.1", 0), make_handler(self.planner))
        try:
            self.assertEqual(server.server_name, "127.0.0.1")
            self.assertEqual(server.server_port, server.socket.getsockname()[1])
            self.assertGreater(server.server_port, 0)
        finally:
            server.server_close()

    def test_http_auth_origin_and_busy_guards_and_real_json_contract(self):
        server = Server(("127.0.0.1", 0), make_handler(self.planner, "local-test"))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = "http://127.0.0.1:%d/complete" % server.server_port
        def request(extra=None):
            headers = {"Content-Type": "application/json", "Authorization": "Bearer local-test"}
            headers.update(extra or {})
            return LOCAL_HTTP.open(Request(url, json.dumps(payload()).encode(), headers), timeout=3)
        try:
            with request() as response:
                self.assertTrue(json.load(response)["success"])
            for headers, status in [({"Authorization": "Bearer wrong"}, 401), ({"Origin": "https://evil.example"}, 403), ({"Content-Type": "text/plain"}, 415)]:
                with self.assertRaises(HTTPError) as raised:
                    request(headers)
                self.assertEqual(raised.exception.code, status)
                raised.exception.close()
            self.planner.lock.acquire()
            try:
                with self.assertRaises(HTTPError) as raised:
                    request()
                self.assertEqual(raised.exception.code, 503)
                raised.exception.close()
            finally:
                self.planner.lock.release()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(1)

    def test_health_reports_loading_without_inference_and_preserves_access_guards(self):
        server = Server(("127.0.0.1", 0), make_handler(self.planner, "local-test"))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = "http://127.0.0.1:%d/health" % server.server_port
        def health(extra=None, path=url):
            headers = {"Authorization": "Bearer local-test"}
            headers.update(extra or {})
            return LOCAL_HTTP.open(Request(path, headers=headers), timeout=3)
        try:
            with health() as response:
                self.assertEqual(json.load(response), {"status": "ok", "service": "babel-needle",
                                                       "model_loaded": False})
            self.assertFalse(FakeNeedle.instances)
            for headers, status in [({"Authorization": ""}, 401),
                                    ({"Origin": "http://127.0.0.1"}, 403),
                                    ({"Sec-Fetch-Site": "same-origin"}, 403)]:
                with self.assertRaises(HTTPError) as raised:
                    health(headers)
                self.assertEqual(raised.exception.code, status)
                raised.exception.close()
            with self.assertRaises(HTTPError) as raised:
                health(path=url + "/other")
            self.assertEqual(raised.exception.code, 404)
            raised.exception.close()
            self.planner.complete(payload())
            with health() as response:
                self.assertTrue(json.load(response)["model_loaded"])
            self.assertEqual(len(FakeNeedle.instances[0].calls), 1)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(1)

    def test_cli_default_and_explicit_zero_announce_distinct_bound_ports(self):
        processes = []
        endpoints = []
        try:
            for arguments in [[], ["--port", "0"]]:
                process = subprocess.Popen(
                    [sys.executable, str(Path(__file__).with_name("needle_bridge.py")), *arguments],
                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    text=True,
                )
                processes.append(process)
                ready = queue.Queue(maxsize=1)
                threading.Thread(target=lambda p=process, q=ready: q.put(p.stdout.readline()),
                                 daemon=True).start()
                line = ready.get(timeout=5)
                self.assertTrue(line.startswith("BABEL_SERVICE_READY "), line)
                result = json.loads(line.removeprefix("BABEL_SERVICE_READY "))
                self.assertEqual(result["service"], "babel-needle")
                self.assertEqual(result["pid"], process.pid)
                endpoint = urlsplit(result["endpoint"])
                self.assertEqual(endpoint.scheme, "http")
                self.assertEqual(endpoint.hostname, "127.0.0.1")
                self.assertEqual(endpoint.path, "/complete")
                self.assertEqual(endpoint.port, result["port"])
                self.assertGreater(endpoint.port, 0)
                endpoints.append(result["endpoint"])
                with LOCAL_HTTP.open(f"http://127.0.0.1:{endpoint.port}/health", timeout=3) as response:
                    self.assertEqual(json.load(response), {"status": "ok", "service": "babel-needle",
                                                           "model_loaded": False})
            self.assertEqual(len(set(endpoints)), 2)
        finally:
            for process in processes:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                process.stdout.close()
                process.stderr.close()


if __name__ == "__main__":
    unittest.main()
