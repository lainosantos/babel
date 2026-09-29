#!/usr/bin/env python3
"""Babel's loopback-only adapter for the official cactus-needle Python API.

This is a Babel protocol, not a built-in Needle HTTP API. It calls complete(),
never run(): tool execution belongs exclusively to Babel's validated MCP client.
"""
import argparse
import hmac
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import BoundedSemaphore, Lock

# Set before importing Needle so its optional usage telemetry stays disabled.
os.environ["NEEDLE_TELEMETRY"] = "0"
os.environ["DO_NOT_TRACK"] = "1"
MAX_BODY = 256 * 1024
MAX_RESPONSE = 256 * 1024


class Planner:
    def __init__(self, weights=None, factory=None):
        self.weights = weights
        self.factory = factory
        self.agent = None
        self.catalog = None
        self.lock = Lock()

    def complete(self, payload):
        if not isinstance(payload, dict):
            raise ValueError("request must be an object")
        text, tools = payload.get("text"), payload.get("tools")
        if not isinstance(text, str) or not text.strip() or len(text.encode()) > 8192:
            raise ValueError("text must contain 1..8192 UTF-8 bytes")
        if not isinstance(tools, list) or not 1 <= len(tools) <= 128:
            raise ValueError("tools must contain 1..128 definitions")
        names = set()
        clean_tools = []
        for tool in tools:
            if not isinstance(tool, dict):
                raise ValueError("tool must be an object")
            name = tool.get("name")
            if (not isinstance(name, str) or not name or len(name) > 128
                    or not all(c.isascii() and (c.isalnum() or c == "_") for c in name)
                    or name in names or not isinstance(tool.get("parameters"), dict)):
                raise ValueError("invalid tool name/schema or duplicate identity")
            names.add(name)
            description = tool.get("description", "")
            if not isinstance(description, str) or len(description) > 4096:
                raise ValueError("invalid tool description")
            # Do not forward triggers: the SDK permits triggers to bypass its
            # confidence gate. Keep only the MCP schema and descriptive metadata.
            clean_tools.append({"name": name, "description": description,
                                "parameters": tool["parameters"]})
        system = payload.get("system", "")
        if not isinstance(system, str) or len(system) > 512:
            raise ValueError("invalid environment facts")
        maximum = payload.get("max_new_tokens", 512)
        if type(maximum) is not int or not 32 <= maximum <= 512:
            raise ValueError("max_new_tokens must be 32..512")
        catalog = json.dumps([clean_tools, system], sort_keys=True, ensure_ascii=False)
        if self.agent is None or self.catalog != catalog:
            if self.factory is None:
                from needle import Needle
                self.factory = Needle
            kwargs = {"tools": clean_tools, "system": system}
            if self.weights:
                kwargs["weights"] = self.weights
            replacement = self.factory(**kwargs)
            previous = self.agent
            self.agent = replacement
            self.catalog = catalog
            if previous is not None:
                previous.close()
        # Each wake activation is a new command; previous users/tool results must
        # never leak into arguments for a later activation.
        self.agent.reset()
        result = self.agent.complete(text, max_new_tokens=maximum)
        if not isinstance(result, dict):
            raise RuntimeError("Needle returned a non-object response")
        return result


def make_handler(planner, api_key=""):
    class Handler(BaseHTTPRequestHandler):
        server_version = "BabelNeedle/1"
        protocol_version = "HTTP/1.0"

        def log_message(self, *_args):
            pass  # No spoken commands, tool schemas, arguments, or keys in logs.

        def reply(self, status, value):
            body = json.dumps(value, ensure_ascii=False, allow_nan=False).encode()
            if len(body) > MAX_RESPONSE:
                status, body = 502, b'{"error":"Needle response exceeds the memory limit"}'
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def do_GET(self):
            self.connection.settimeout(5)
            if self.path != "/health":
                return self.reply(404, {"error": "not found"})
            if self.headers.get("Origin") or self.headers.get("Sec-Fetch-Site"):
                return self.reply(403, {"error": "browser requests are not accepted"})
            expected = ("Bearer " + api_key).encode()
            received = self.headers.get("Authorization", "").encode()
            if api_key and not hmac.compare_digest(expected, received):
                return self.reply(401, {"error": "unauthorized"})
            # A health probe never imports the SDK, downloads a model, or calls
            # complete(). Successful inference must be checked separately.
            return self.reply(200, {"status": "ok", "service": "babel-needle",
                                    "model_loaded": planner.agent is not None})

        def do_POST(self):
            self.connection.settimeout(5)
            if self.path != "/complete":
                return self.reply(404, {"error": "not found"})
            if self.headers.get("Origin") or self.headers.get("Sec-Fetch-Site"):
                return self.reply(403, {"error": "browser requests are not accepted"})
            expected = ("Bearer " + api_key).encode()
            received = self.headers.get("Authorization", "").encode()
            if api_key and not hmac.compare_digest(expected, received):
                return self.reply(401, {"error": "unauthorized"})
            if self.headers.get("Content-Type", "").split(";")[0].strip() != "application/json":
                return self.reply(415, {"error": "application/json required"})
            if self.headers.get("Transfer-Encoding"):
                return self.reply(400, {"error": "chunked requests are not accepted"})
            try:
                length = int(self.headers.get("Content-Length", "0"))
            except ValueError:
                return self.reply(400, {"error": "invalid content length"})
            if not 0 < length <= MAX_BODY:
                return self.reply(413, {"error": "request body exceeds the limit"})
            if not planner.lock.acquire(blocking=False):
                return self.reply(503, {"error": "Needle is processing another command"})
            try:
                raw = self.rfile.read(length)
                if len(raw) != length:
                    return self.reply(400, {"error": "incomplete request"})
                payload = json.loads(raw)
                result = planner.complete(payload)
                self.reply(200, result)
            except (ValueError, UnicodeError):
                self.reply(400, {"error": "invalid command request"})
            except ImportError:
                self.reply(503, {"error": "install the official cactus-needle package in this Python environment"})
            except Exception:
                self.reply(502, {"error": "Needle inference failed; verify the installed runtime and model"})
            finally:
                planner.lock.release()

    return Handler


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 4

    def __init__(self, *args, **kwargs):
        self.connections = BoundedSemaphore(8)
        super().__init__(*args, **kwargs)

    def process_request(self, request, client_address):
        if not self.connections.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            request.settimeout(5)
            super().process_request(request, client_address)
        except Exception:
            self.connections.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.connections.release()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=0, help="loopback port; 0 asks the OS for an available port")
    parser.add_argument("--weights", help="optional local Needle 3 .cact model")
    parser.add_argument("--api-key-env", default="", help="environment variable holding an optional bearer key")
    args = parser.parse_args()
    if not 0 <= args.port <= 65535:
        parser.error("port must be 0..65535")
    key = os.environ.get(args.api_key_env, "") if args.api_key_env else ""
    if args.api_key_env and not key:
        parser.error("the configured API key environment variable is absent or empty")
    server = Server(("127.0.0.1", args.port), make_handler(Planner(args.weights), key))
    port = server.server_address[1]
    endpoint = f"http://127.0.0.1:{port}/complete"
    # Bind before announcing readiness: the selected port remains reserved by
    # this server throughout startup and operation. No probe/release/rebind race.
    print("BABEL_SERVICE_READY " + json.dumps({"service": "babel-needle",
          "endpoint": endpoint, "pid": os.getpid(), "port": port}), flush=True)
    print(f"Babel Needle adapter: {endpoint}", file=sys.stderr, flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
