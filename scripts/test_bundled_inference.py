#!/usr/bin/env python3
"""Exercise packaged native inference with synthetic input, never a microphone.

Runs on the same OS/architecture as --runtime-dir. Model downloads are pinned by
src/local_runtime/models.json and reused from --model-cache (Babel's cache layout).
No Python packages or external inference servers are needed by this CI test.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import wave

from build_local_runtime import host
from local_runtime_packaging import validate

ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "src/local_runtime/models.json"
MAX_LINE = 8192
MAX_RESPONSE = 1024 * 1024


def checked_model(asset, cache):
    cache.mkdir(parents=True, exist_ok=True)
    path = cache / f"{asset['sha256'][:16]}-{asset['name']}"

    def matches(candidate):
        if candidate.stat().st_size != asset["size"]:
            return False
        with candidate.open("rb") as stream:
            return hashlib.file_digest(stream, "sha256").hexdigest() == asset["sha256"]

    if path.exists():
        if not matches(path):
            raise ValueError(f"Cached model does not match its pinned hash: {asset['name']}")
        return path.resolve()
    print(f"Downloading pinned model: {asset['name']} ({asset['size']} bytes)", flush=True)
    with tempfile.NamedTemporaryFile(prefix=".inference-smoke-", dir=cache, delete=False) as target:
        temporary = Path(target.name)
        try:
            request = urllib.request.Request(asset["url"], headers={"User-Agent": "Babel-inference-CI/1"})
            with urllib.request.urlopen(request, timeout=90) as response:
                if urllib.parse.urlsplit(response.url).scheme != "https":
                    raise ValueError("Model download redirected to a non-HTTPS address")
                received = 0
                while chunk := response.read(1024 * 1024):
                    received += len(chunk)
                    if received > asset["size"]:
                        raise ValueError("Model download exceeded its pinned size")
                    target.write(chunk)
            target.close()
            if not matches(temporary):
                raise ValueError(f"Model download hash mismatch: {asset['name']}")
            temporary.replace(path)
        finally:
            target.close()
            temporary.unlink(missing_ok=True)
    return path.resolve()


class Child:
    """Bounded stdout lines with portable timeout support, including Windows."""

    def __init__(self, args, timeout):
        self.timeout = timeout
        self.lines = queue.Queue(maxsize=32)
        self.stop_reader = threading.Event()
        self.process = subprocess.Popen(
            [str(item) for item in args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW if host()[0] == "windows" else 0,
        )
        self.reader = threading.Thread(target=self.read_lines, daemon=True)
        self.reader.start()

    def read_lines(self):
        try:
            while not self.stop_reader.is_set():
                line = self.process.stdout.readline(MAX_LINE + 1)
                if len(line) > MAX_LINE:
                    item = ValueError("Native service exceeded the stdout line limit")
                elif not line:
                    item = EOFError("Native service ended its readiness/output stream")
                else:
                    item = line.rstrip(b"\r\n")
                while not self.stop_reader.is_set():
                    try:
                        self.lines.put(item, timeout=0.1)
                        break
                    except queue.Full:
                        continue
                if isinstance(item, Exception):
                    break
        except (OSError, ValueError) as error:
            if not self.stop_reader.is_set():
                try:
                    self.lines.put(error, timeout=0.2)
                except queue.Full:
                    pass

    def line(self, deadline):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("Native service response timed out")
        try:
            line = self.lines.get(timeout=remaining)
        except queue.Empty as error:
            raise TimeoutError("Native service response timed out") from error
        if isinstance(line, Exception):
            raise line
        return line

    def readiness(self, prefix):
        deadline = time.monotonic() + self.timeout
        received = 0
        while True:
            line = self.line(deadline)
            received += len(line)
            if received > MAX_RESPONSE:
                raise ValueError("Native startup output exceeded the limit")
            if line.startswith(prefix):
                body = json.loads(line[len(prefix):])
                if self.process.poll() is not None:
                    raise RuntimeError("Native service exited after announcing readiness")
                return body

    def write_json(self, value):
        data = json.dumps(value, ensure_ascii=False).encode("utf-8") + b"\n"
        if len(data) > MAX_LINE:
            raise ValueError("Synthetic Piper input exceeded the test bound")
        self.process.stdin.write(data)
        self.process.stdin.flush()

    def close(self):
        self.stop_reader.set()
        if self.process.poll() is None:
            self.process.kill()
        try:
            self.process.wait(timeout=10)
        finally:
            for stream in (self.process.stdin, self.process.stdout):
                if stream:
                    stream.close()
            self.reader.join(timeout=2)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def ready_http(child, service, path):
    result = child.readiness(b"BABEL_SERVICE_READY ")
    endpoint = result.get("endpoint", "")
    address = urllib.parse.urlsplit(endpoint)
    port = result.get("port")
    if (result.get("service") != service or address.scheme != "http"
            or address.hostname != "127.0.0.1" or type(port) is not int
            or not 0 < port <= 65535 or address.port != port or address.path != path
            or address.username or address.password or address.query or address.fragment):
        raise ValueError("Native helper announced an invalid service or endpoint")
    # Passing --port 0 and consuming the child's already-bound socket is the
    # invariant: a port number alone cannot prove how a socket was allocated.
    print(f"Ready: {service}, dynamically bound port {port}, pid {child.process.pid}", flush=True)
    return endpoint


def request_json(endpoint, body, content_type, timeout):
    request = urllib.request.Request(endpoint, data=body, headers={"Content-Type": content_type})
    # CI proxy settings must never send synthetic loopback traffic elsewhere.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=timeout) as response:
        data = response.read(MAX_RESPONSE + 1)
        if len(data) > MAX_RESPONSE:
            raise ValueError("Inference response exceeded the test bound")
        return json.loads(data)


def silent_wav():
    output = io.BytesIO()
    with wave.open(output, "wb") as audio:
        audio.setnchannels(1)
        audio.setsampwidth(2)
        audio.setframerate(16000)
        audio.writeframes(b"\0\0" * 16000)
    return output.getvalue()


def whisper_multipart():
    boundary = "BabelSyntheticInferenceSmokeBoundary"
    chunks = []
    for name, value in (("language", "en"), ("translate", "false"), ("response_format", "json")):
        chunks.append(f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode())
    chunks.append(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="silence.wav"\r\nContent-Type: audio/wav\r\n\r\n'.encode())
    chunks.extend((silent_wav(), f"\r\n--{boundary}--\r\n".encode()))
    return b"".join(chunks), f"multipart/form-data; boundary={boundary}"


def validate_wave(path):
    if not path.is_file() or not 44 <= path.stat().st_size <= 32 * 1024 * 1024:
        raise ValueError("Piper did not create a bounded WAV at the requested path")
    data = path.read_bytes()
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError("Piper output is not RIFF/WAVE")
    riff_size = struct.unpack_from("<I", data, 4)[0]
    # Piper's pinned CLI writes its documented streaming placeholder even to
    # files. Accept only that exact header, with real samples bounded below.
    streaming = riff_size == 0x7FFFF024
    if not streaming and riff_size + 8 != len(data):
        raise ValueError("Piper WAV size does not match its header")
    cursor, audio_format, audio = 12, None, None
    while cursor + 8 <= len(data):
        tag, size = struct.unpack_from("<4sI", data, cursor)
        cursor += 8
        if streaming and tag == b"data" and size == 0x7FFFF000:
            size = len(data) - cursor
        elif cursor + size > len(data):
            raise ValueError("Piper WAV contains a truncated chunk")
        chunk = data[cursor:cursor + size]
        if tag == b"fmt " and size >= 16:
            audio_format = struct.unpack_from("<HHIIHH", chunk)
        elif tag == b"data":
            audio = chunk
        cursor += size + size % 2
    if audio_format is None or audio is None or not audio:
        raise ValueError("Piper WAV is missing its format or samples")
    encoding, channels, rate, byte_rate, block_align, bits = audio_format
    if (encoding, channels, bits, block_align) != (3, 1, 32, 4) or not 8000 <= rate <= 192000 or byte_rate != rate * 4:
        raise ValueError("Piper WAV must contain mono 32-bit float samples")
    if len(audio) % 4 or len(audio) // 4 > rate * 60:
        raise ValueError("Piper returned misaligned or excessively long audio")
    audible = False
    for (sample,) in struct.iter_unpack("<f", audio):
        if not math.isfinite(sample):
            raise ValueError("Piper returned a non-finite sample")
        audible |= abs(sample) > 0.0001
    if not audible:
        raise ValueError("Piper returned silent audio")
    return rate, len(audio) // 4


def main(args):
    system, arch = host()
    runtime = args.runtime_dir.resolve()
    manifest = validate(runtime, system, arch)
    catalog = json.loads(CATALOG.read_text())
    cache = args.model_cache.expanduser().resolve()
    tiny = checked_model(catalog["whisper"]["tiny"], cache)
    qwen = checked_model(catalog["translation"]["qwen3-0.6b"], cache)
    voice = catalog["voices"]["en_US-lessac-medium"]
    piper_model = checked_model(voice["model"], cache)
    piper_config = checked_model(voice["config"], cache)
    checked_model(voice["license"], cache)
    executable = lambda name: runtime / manifest["services"][name]["executable"]

    with Child([executable("whisper"), "--host", "127.0.0.1", "--port", "0", "-t", "2", "-m", tiny], args.timeout) as child:
        endpoint = ready_http(child, "babel-whisper", "/inference")
        body, content_type = whisper_multipart()
        result = request_json(endpoint, body, content_type, args.timeout)
        if not isinstance(result, dict) or not isinstance(result.get("text"), str):
            raise ValueError("Whisper did not return a JSON transcript for synthetic audio")
        print("PASS Whisper: one second of synthetic PCM16/16k silence, JSON transcript", flush=True)

    with Child([executable("llama"), "--host", "127.0.0.1", "--port", "0", "--threads", "2", "--ctx-size", "2048", "--parallel", "1", "--n-gpu-layers", "0", "--alias", "qwen3-0.6b", "--model", qwen], args.timeout) as child:
        endpoint = ready_http(child, "babel-llama", "/v1/chat/completions")
        payload = {"model": "qwen3-0.6b", "stream": False, "temperature": 0, "max_tokens": 64,
                   "chat_template_kwargs": {"enable_thinking": False},
                   "messages": [{"role": "system", "content": "Translate the user's Portuguese sentence into English. Return only the translation."}, {"role": "user", "content": "Bom dia."}]}
        result = request_json(endpoint, json.dumps(payload).encode(), "application/json", args.timeout)
        choices = result.get("choices", []) if isinstance(result, dict) else []
        if (len(choices) != 1 or choices[0].get("finish_reason") != "stop"
                or not isinstance(choices[0].get("message", {}).get("content"), str)
                or not choices[0]["message"]["content"].strip()):
            raise ValueError("Qwen did not finish its synthetic translation normally")
        print("PASS Qwen: bounded chat completion with thinking disabled", flush=True)

    data = runtime / manifest["services"]["piper"]["data"]
    with tempfile.TemporaryDirectory(prefix="babel-inference-ação-") as temporary:
        with Child([executable("piper"), "-m", piper_model, "-c", piper_config, "--espeak-data", data, "--json-input"], args.timeout) as child:
            ready = child.readiness(b"BABEL_PIPER_READY ")
            rate = ready.get("sample_rate")
            if type(rate) is not int or not 8000 <= rate <= 192000:
                raise ValueError("Piper readiness sample rate is invalid")
            for index, text in enumerate(("This is a synthetic Babel test.", "The same voice process is still available.")):
                path = (Path(temporary) / f"fala-{index}-áudio.wav").resolve()
                child.write_json({"text": text, "output_file": str(path)})
                announced = child.line(time.monotonic() + args.timeout).decode("utf-8")
                if announced != str(path):
                    raise ValueError("Piper did not return the exact requested Unicode output path")
                actual_rate, samples = validate_wave(path)
                if actual_rate != rate or child.process.poll() is not None:
                    raise ValueError("Piper format changed or its persistent process exited")
                print(f"PASS Piper: persistent request {index + 1}, {samples} float samples at {rate} Hz, pid {child.process.pid}", flush=True)
    print(f"Bundled inference passed on {system}-{arch}; no microphone or installed AI service was used.", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime-dir", type=Path, required=True)
    parser.add_argument("--model-cache", type=Path, default=ROOT / ".tools/inference-test-models")
    parser.add_argument("--timeout", type=int, default=180)
    arguments = parser.parse_args()
    if not 10 <= arguments.timeout <= 600:
        parser.error("--timeout must be 10..600 seconds")
    try:
        main(arguments)
    except (OSError, ValueError, RuntimeError, EOFError, TimeoutError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Bundled inference smoke failed: {error}\n")
