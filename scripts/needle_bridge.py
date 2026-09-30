#!/usr/bin/env python3
"""Babel's loopback-only adapter for the official cactus-needle Python API.

This is a Babel protocol, not a built-in Needle HTTP API. It calls complete(),
never run(): tool execution belongs exclusively to Babel's validated MCP client.
"""
import argparse
import hmac
import json
import os
import queue
import select
import socket
import subprocess
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from socketserver import TCPServer
from threading import BoundedSemaphore, Event, Lock, Thread

# Set before importing Needle so its optional usage telemetry stays disabled.
os.environ["NEEDLE_TELEMETRY"] = "0"
os.environ["DO_NOT_TRACK"] = "1"
MAX_BODY = 256 * 1024
MAX_RESPONSE = 256 * 1024


class ModelWorker:
    """SDK globals retain native weights after Needle.close(). Own their process
    so idle unload really returns that memory to the OS on all three platforms.
    """
    def __init__(self, deadline, cancelled=None, **kwargs):
        self.deadline = deadline
        self.cancelled = cancelled
        self.responses = queue.Queue(maxsize=1)
        self.process = subprocess.Popen(
            [sys.executable, os.path.abspath(__file__), "--model-worker"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        )
        output = self.process.stdout

        def receive():
            while True:
                try:
                    raw = output.readline(MAX_RESPONSE + 1)
                    self.responses.put_nowait(raw)
                except (OSError, ValueError, queue.Full):
                    return
                if not raw or not raw.endswith(b"\n"):
                    return

        self.reader = Thread(target=receive, daemon=True)
        self.reader.start()
        try:
            self._exchange({"operation": "load", "kwargs": kwargs})
        except BaseException:
            self.close()
            raise

    def _exchange(self, request):
        body = json.dumps(request, ensure_ascii=False, allow_nan=False,
                          separators=(",", ":")).encode() + b"\n"
        if len(body) > MAX_BODY:
            raise ValueError("Needle worker request exceeds the limit")
        finished, interrupted = Event(), Event()
        process = self.process

        def guard():
            # A model load schema may exceed pipe capacity. Cover the write as
            # well as reading the response; killing the reader unblocks a full
            # pipe without trying to acquire the writer's buffered-I/O lock.
            while not finished.is_set():
                remaining = self.deadline - time.monotonic()
                if self.cancelled is not None and self.cancelled.is_set() or remaining <= 0:
                    interrupted.set()
                    try:
                        process.kill()
                    except OSError:
                        pass
                    return
                finished.wait(min(0.1, remaining))

        watchdog = Thread(target=guard, daemon=True)
        watchdog.start()
        try:
            self.process.stdin.write(body)
            self.process.stdin.flush()
            while True:
                if interrupted.is_set():
                    raise RuntimeError("Needle worker request cancelled or timed out")
                try:
                    raw = self.responses.get(timeout=min(0.1, max(0.001, self.deadline - time.monotonic())))
                    break
                except queue.Empty:
                    continue
            if not raw.endswith(b"\n") or len(raw) > MAX_RESPONSE:
                if interrupted.is_set():
                    raise RuntimeError("Needle worker request cancelled or timed out")
                raise RuntimeError("Needle worker exited or exceeded the response limit")
            value = json.loads(raw)
        except (OSError, ValueError) as error:
            if interrupted.is_set():
                raise RuntimeError("Needle worker request cancelled or timed out") from error
            raise RuntimeError("Needle worker communication failed") from error
        finally:
            finished.set()
            watchdog.join(timeout=1)
        if value.get("error") == "import":
            raise ImportError("Needle SDK unavailable")
        if "error" in value:
            raise RuntimeError("Needle worker inference failed")
        return value.get("result")

    def reset(self):
        self._exchange({"operation": "reset"})

    def complete(self, text, max_new_tokens):
        return self._exchange({"operation": "complete", "text": text,
                               "max_new_tokens": max_new_tokens})

    def close(self):
        process, self.process = self.process, None
        if process is None:
            return
        # EOF also lets the child release memory if the owning HTTP bridge dies.
        try:
            process.stdin.close()
        except OSError:
            pass
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        finally:
            process.stdout.close()
            self.reader.join(timeout=1)


def model_worker():
    # Keep the protocol separate from native runtime stdout and never log speech.
    output = os.fdopen(os.dup(sys.stdout.fileno()), "wb", buffering=0)
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    requests = queue.Queue(maxsize=1)

    def receive():
        while True:
            raw = sys.stdin.buffer.readline(MAX_BODY + 1)
            if not raw:
                # An SDK call may be in flight when Babel exits. A pipe monitor
                # avoids leaving native inference alive after its owner dies.
                os._exit(0)
            if not raw.endswith(b"\n") or len(raw) > MAX_BODY:
                os._exit(1)
            try:
                requests.put(json.loads(raw))
            except ValueError:
                os._exit(1)

    Thread(target=receive, daemon=True).start()
    agent = None
    while True:
        request = requests.get()
        try:
            operation = request.get("operation")
            if operation == "load" and agent is None:
                from needle import Needle
                agent = Needle(**request["kwargs"])
                result = None
            elif operation == "reset" and agent is not None:
                agent.reset()
                result = None
            elif operation == "complete" and agent is not None:
                result = agent.complete(request["text"], max_new_tokens=request["max_new_tokens"])
            else:
                raise ValueError("invalid worker operation")
            response = {"result": result}
        except ImportError:
            response = {"error": "import"}
        except Exception:
            response = {"error": "inference"}
        try:
            encoded = json.dumps(response, ensure_ascii=False, allow_nan=False,
                                 separators=(",", ":")).encode() + b"\n"
            if len(encoded) > MAX_RESPONSE:
                encoded = b'{"error":"limit"}\n'
            output.write(encoded)
        except (ValueError, OSError):
            return


class Planner:
    def __init__(self, weights=None, factory=None, idle_unload_secs=60,
                 timeout_secs=20, clock=time.monotonic):
        self.weights = weights
        self.factory = factory
        self.agent = None
        self.catalog = None
        self.lock = Lock()
        self.idle_unload_secs = idle_unload_secs
        self.clock = clock
        self.last_used = None
        self.timeout_secs = timeout_secs

    def close(self):
        previous, self.agent = self.agent, None
        self.catalog = None
        self.last_used = None
        if previous is not None:
            previous.close()

    def unload_idle(self):
        # The HTTP request owns this same lock throughout a completion. An idle
        # check never interrupts inference or waits behind a slow command.
        if not self.lock.acquire(blocking=False):
            return
        try:
            if (self.agent is not None and self.last_used is not None
                    and self.clock() - self.last_used >= self.idle_unload_secs):
                self.close()
        finally:
            self.lock.release()

    def complete(self, payload, cancelled=None):
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
        deadline = time.monotonic() + self.timeout_secs
        if self.agent is None or self.catalog != catalog:
            # Close before opening the replacement to bound peak native memory.
            self.close()
            kwargs = {"tools": clean_tools, "system": system}
            if self.weights:
                kwargs["weights"] = self.weights
            self.agent = (self.factory(**kwargs) if self.factory is not None
                          else ModelWorker(deadline=deadline, cancelled=cancelled, **kwargs))
            self.catalog = catalog
        # Each wake activation is a new command; previous users/tool results must
        # never leak into arguments for a later activation.
        if isinstance(self.agent, ModelWorker):
            self.agent.deadline = deadline
            self.agent.cancelled = cancelled
        try:
            self.agent.reset()
            result = self.agent.complete(text, max_new_tokens=maximum)
        except BaseException:
            # A timeout or invalid worker must never poison the next activation.
            self.close()
            raise
        finally:
            self.last_used = self.clock()
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
            try:
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Cache-Control", "no-store")
                self.send_header("X-Content-Type-Options", "nosniff")
                self.end_headers()
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
            cancelled = Event()
            finished = Event()
            monitor = None
            try:
                raw = self.rfile.read(length)
                if len(raw) != length:
                    return self.reply(400, {"error": "incomplete request"})
                payload = json.loads(raw)
                # Rust cancels inference by closing the HTTP request. Watch only
                # while this request is in progress so cancellation also frees
                # native work instead of waiting for a model timeout.
                def monitor_disconnect():
                    while not finished.wait(0.1):
                        try:
                            readable, _, _ = select.select([self.connection], [], [], 0)
                            if readable and not self.connection.recv(1, socket.MSG_PEEK):
                                cancelled.set()
                                return
                        except OSError:
                            cancelled.set()
                            return
                monitor = Thread(target=monitor_disconnect, daemon=True)
                monitor.start()
                result = planner.complete(payload, cancelled=cancelled)
                self.reply(200, result)
            except (ValueError, UnicodeError):
                self.reply(400, {"error": "invalid command request"})
            except ImportError:
                self.reply(503, {"error": "install the official cactus-needle package in this Python environment"})
            except Exception:
                self.reply(502, {"error": "Needle inference failed; verify the installed runtime and model"})
            finally:
                finished.set()
                if monitor is not None:
                    monitor.join(timeout=1)
                planner.lock.release()

    return Handler


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 4

    def __init__(self, *args, **kwargs):
        self.connections = BoundedSemaphore(8)
        self.planner = kwargs.pop("planner", None)
        super().__init__(*args, **kwargs)

    def service_actions(self):
        if self.planner is not None:
            self.planner.unload_idle()

    def server_close(self):
        super().server_close()
        if self.planner is not None:
            with self.planner.lock:
                self.planner.close()

    def server_bind(self):
        # HTTPServer normally resolves the bound address with getfqdn(). This
        # loopback-only helper has no use for a DNS name, and a slow system
        # resolver must not delay its readiness announcement or health probes.
        TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]

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
    parser.add_argument("--idle-unload-secs", type=int, default=60,
                        help="release native model memory after 1..3600 idle seconds")
    parser.add_argument("--timeout-secs", type=int, default=20,
                        help="maximum duration of a model request, including loading (1..120)")
    args = parser.parse_args()
    if not 0 <= args.port <= 65535:
        parser.error("port must be 0..65535")
    if not 1 <= args.idle_unload_secs <= 3600:
        parser.error("idle-unload-secs must be 1..3600")
    if not 1 <= args.timeout_secs <= 120:
        parser.error("timeout-secs must be 1..120")
    key = os.environ.get(args.api_key_env, "") if args.api_key_env else ""
    if args.api_key_env and not key:
        parser.error("the configured API key environment variable is absent or empty")
    planner = Planner(args.weights, idle_unload_secs=args.idle_unload_secs,
                      timeout_secs=args.timeout_secs)
    server = Server(("127.0.0.1", args.port), make_handler(planner, key), planner=planner)
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
    if sys.argv[1:] == ["--model-worker"]:
        model_worker()
    else:
        main()
