import json
from pathlib import Path
import queue
import subprocess
import sys
import threading
import unittest
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import Request, urlopen

from needle_bridge import Planner, Server, make_handler


class FakeNeedle:
    instances = []
    def __init__(self, **kwargs):
        self.kwargs = kwargs
        self.resets = 0
        self.calls = []
        self.instances.append(self)
    def close(self):
        pass
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

    def test_rejects_malformed_and_duplicate_tools(self):
        bad = payload()
        bad["tools"].append(bad["tools"][0])
        with self.assertRaises(ValueError):
            self.planner.complete(bad)
        with self.assertRaises(ValueError):
            self.planner.complete({"text": "x", "tools": []})
        self.assertFalse(FakeNeedle.instances)

    def test_http_auth_origin_and_busy_guards_and_real_json_contract(self):
        server = Server(("127.0.0.1", 0), make_handler(self.planner, "local-test"))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = "http://127.0.0.1:%d/complete" % server.server_port
        def request(extra=None):
            headers = {"Content-Type": "application/json", "Authorization": "Bearer local-test"}
            headers.update(extra or {})
            return urlopen(Request(url, json.dumps(payload()).encode(), headers), timeout=3)
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
            return urlopen(Request(path, headers=headers), timeout=3)
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
                with urlopen(f"http://127.0.0.1:{endpoint.port}/health", timeout=3) as response:
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
