"""Minimal MCP 1.6 client for ssh-mcp v3 integration tests.

Two transports are supported:

- :class:`HttpTransport` — rmcp 1.6 streamable-HTTP. Uses POST for unary
  requests; the SSE channel returned alongside ``initialize`` is used to feed
  ``notifications/*`` into a queue consumed by ``McpClient.receive_notification``.
- :class:`StdioTransport` — line-delimited JSON-RPC over the stdio of a
  spawned ``ssh-mcp-stdio`` binary.

The client always sends ``initialize`` first (rmcp requires it). For HTTP the
session id is captured from the ``Mcp-Session-Id`` response header; stdio
keeps no session id (the process IS the session).
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import threading
import time
import uuid
from typing import Any, Iterable

try:
    import requests  # type: ignore
except ImportError:  # pragma: no cover - HTTP is optional for stdio-only callers
    requests = None  # type: ignore[assignment]


# ---------------------------------------------------------------------------
# Transport base
# ---------------------------------------------------------------------------


class Transport:
    """Abstract transport. Implementations send/receive JSON-RPC frames."""

    def send_request(self, method: str, params: dict | None, *, timeout: float) -> dict:
        raise NotImplementedError

    def send_notification(self, method: str, params: dict | None) -> None:
        raise NotImplementedError

    def receive_notification(self, *, timeout: float) -> dict | None:
        raise NotImplementedError

    def close(self) -> None:
        raise NotImplementedError


# ---------------------------------------------------------------------------
# HTTP transport (rmcp 1.6 streamable-http-server)
# ---------------------------------------------------------------------------


class HttpTransport(Transport):
    """rmcp 1.6 streamable-HTTP transport.

    Each unary request is a POST that may return either ``application/json``
    or ``text/event-stream`` (rmcp picks per request). After ``initialize`` we
    open a long-running GET to receive ``notifications/*`` from the server
    and push them into ``self._notifications``.
    """

    def __init__(self, base_url: str = "http://127.0.0.1:8000", *, request_timeout: float = 60.0) -> None:
        if requests is None:  # pragma: no cover - module-level guard
            raise ImportError(
                "HttpTransport requires the `requests` package. "
                "Install with `pip install requests` or use StdioTransport."
            )
        self.base_url = base_url.rstrip("/") + "/"
        self.session_id: str | None = None
        self._notifications: queue.Queue[dict] = queue.Queue()
        self._sse_thread: threading.Thread | None = None
        self._sse_stop = threading.Event()
        self._sse_response: requests.Response | None = None
        self._request_timeout = request_timeout
        self._lock = threading.Lock()
        self._request_id = 0

    # -- internal helpers ----------------------------------------------------

    def _next_id(self) -> int:
        with self._lock:
            self._request_id += 1
            return self._request_id

    def _headers(self, *, accept_sse: bool = True) -> dict:
        accept = "application/json, text/event-stream" if accept_sse else "application/json"
        headers = {"Content-Type": "application/json", "Accept": accept}
        if self.session_id:
            headers["Mcp-Session-Id"] = self.session_id
        return headers

    def _post(self, body: dict, *, timeout: float | None = None) -> requests.Response:
        return requests.post(
            self.base_url,
            data=json.dumps(body),
            headers=self._headers(),
            timeout=timeout if timeout is not None else self._request_timeout,
            stream=True,
        )

    def _capture_session_header(self, resp: requests.Response) -> None:
        if self.session_id is None:
            sid = resp.headers.get("Mcp-Session-Id") or resp.headers.get("mcp-session-id")
            if sid:
                self.session_id = sid

    @staticmethod
    def _parse_sse_payloads(text: str) -> list[dict]:
        out: list[dict] = []
        for line in text.splitlines():
            if line.startswith("data: "):
                try:
                    out.append(json.loads(line[6:]))
                except json.JSONDecodeError:
                    continue
        return out

    def _route_payload(self, payload: dict, want_id: int) -> dict | None:
        if not isinstance(payload, dict):
            return None
        # Server-initiated notification?
        if "method" in payload and "id" not in payload:
            self._notifications.put(payload)
            return None
        if payload.get("id") == want_id:
            return payload
        return None

    def _read_response(self, resp: requests.Response, want_id: int) -> dict:
        content_type = resp.headers.get("content-type", "")
        if "text/event-stream" in content_type:
            # Drain until we observe the matching JSON-RPC response. Notifications
            # encountered along the way are routed to the inbound queue.
            buffer: list[str] = []
            for raw in resp.iter_lines(decode_unicode=True):
                if raw is None:
                    continue
                if raw == "":
                    payload_text = "\n".join(buffer)
                    buffer = []
                    for parsed in self._parse_sse_payloads(payload_text):
                        match = self._route_payload(parsed, want_id)
                        if match is not None:
                            return match
                    continue
                buffer.append(raw)
            # Stream closed with no match.
            for parsed in self._parse_sse_payloads("\n".join(buffer)):
                match = self._route_payload(parsed, want_id)
                if match is not None:
                    return match
            raise RuntimeError(f"SSE stream closed before id={want_id} arrived")
        body = resp.text
        try:
            parsed = json.loads(body)
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"unparseable HTTP body: {body[:200]}") from exc
        if isinstance(parsed, list):
            for entry in parsed:
                if isinstance(entry, dict) and entry.get("id") == want_id:
                    return entry
            raise RuntimeError(f"batch response missing id={want_id}")
        return parsed

    # -- public API ----------------------------------------------------------

    def send_request(self, method: str, params: dict | None, *, timeout: float) -> dict:
        rid = self._next_id()
        body = {"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}}
        resp = self._post(body, timeout=timeout)
        try:
            if resp.status_code != 200:
                detail = resp.text[:200]
                raise RuntimeError(f"HTTP {resp.status_code}: {detail}")
            self._capture_session_header(resp)
            return self._read_response(resp, rid)
        finally:
            resp.close()

    def send_notification(self, method: str, params: dict | None) -> None:
        body = {"jsonrpc": "2.0", "method": method, "params": params or {}}
        resp = self._post(body, timeout=self._request_timeout)
        try:
            self._capture_session_header(resp)
        finally:
            resp.close()

    def start_notification_stream(self) -> None:
        """Open a GET on ``/`` to receive server-initiated notifications.

        rmcp's streamable-http-server sends notifications on the SSE channel
        attached to the long-running GET request. We drain that stream in a
        background thread and push every notification into ``self._notifications``.
        """
        if self._sse_thread is not None:
            return
        if self.session_id is None:
            raise RuntimeError("start_notification_stream requires a session id; call initialize first")

        headers = {"Accept": "text/event-stream", "Mcp-Session-Id": self.session_id}

        def _loop() -> None:
            try:
                resp = requests.get(self.base_url, headers=headers, stream=True, timeout=None)
            except requests.RequestException:
                return
            self._sse_response = resp
            try:
                buffer: list[str] = []
                for raw in resp.iter_lines(decode_unicode=True):
                    if self._sse_stop.is_set():
                        return
                    if raw is None:
                        continue
                    if raw == "":
                        text = "\n".join(buffer)
                        buffer = []
                        for parsed in self._parse_sse_payloads(text):
                            if isinstance(parsed, dict) and "method" in parsed:
                                self._notifications.put(parsed)
                        continue
                    buffer.append(raw)
            except Exception:
                # SSE stream torn down (server stop, transport close, etc.) — exit quietly.
                return
            finally:
                try:
                    resp.close()
                except Exception:
                    pass

        thread = threading.Thread(target=_loop, name="mcp-sse-listener", daemon=True)
        thread.start()
        self._sse_thread = thread

    def receive_notification(self, *, timeout: float) -> dict | None:
        try:
            return self._notifications.get(timeout=timeout)
        except queue.Empty:
            return None

    def close(self) -> None:
        self._sse_stop.set()
        if self._sse_response is not None:
            try:
                self._sse_response.close()
            except Exception:
                pass
        if self._sse_thread is not None:
            self._sse_thread.join(timeout=1.0)
            self._sse_thread = None


# ---------------------------------------------------------------------------
# Stdio transport
# ---------------------------------------------------------------------------


class StdioTransport(Transport):
    """Line-delimited JSON-RPC over stdio.

    Spawns the binary, writes one JSON object per line, and reads the same
    way. Notifications received during a request are routed to ``self._notifications``.
    """

    def __init__(self, cmd: Iterable[str], env: dict | None = None) -> None:
        self.cmd = list(cmd)
        self.env = {**os.environ, **(env or {})}
        self._proc: subprocess.Popen | None = None
        self._stdout_thread: threading.Thread | None = None
        self._stop = threading.Event()
        self._notifications: queue.Queue[dict] = queue.Queue()
        self._responses: dict[int, dict] = {}
        self._cv = threading.Condition()
        self._next_request_id = 0
        self._id_lock = threading.Lock()

    # -- lifecycle -----------------------------------------------------------

    def start(self) -> None:
        if self._proc is not None:
            return
        self._proc = subprocess.Popen(
            self.cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=self.env,
            bufsize=1,
            text=True,
        )
        self._stdout_thread = threading.Thread(
            target=self._reader_loop, name="stdio-reader", daemon=True
        )
        self._stdout_thread.start()

    def _reader_loop(self) -> None:
        proc = self._proc
        if proc is None or proc.stdout is None:
            return
        try:
            for line in proc.stdout:
                if self._stop.is_set():
                    return
                line = line.strip()
                if not line:
                    continue
                try:
                    payload = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if isinstance(payload, dict) and "method" in payload and "id" not in payload:
                    self._notifications.put(payload)
                    continue
                if isinstance(payload, dict) and "id" in payload:
                    with self._cv:
                        self._responses[payload["id"]] = payload
                        self._cv.notify_all()
        except (ValueError, OSError):
            return

    def _next_id(self) -> int:
        with self._id_lock:
            self._next_request_id += 1
            return self._next_request_id

    def _write(self, payload: dict) -> None:
        if self._proc is None or self._proc.stdin is None:
            raise RuntimeError("stdio transport not started")
        body = json.dumps(payload) + "\n"
        self._proc.stdin.write(body)
        self._proc.stdin.flush()

    # -- public API ----------------------------------------------------------

    def send_request(self, method: str, params: dict | None, *, timeout: float) -> dict:
        rid = self._next_id()
        self._write({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})
        deadline = time.monotonic() + timeout
        with self._cv:
            while True:
                if rid in self._responses:
                    return self._responses.pop(rid)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"stdio request timed out: {method}")
                self._cv.wait(timeout=min(remaining, 0.5))

    def send_notification(self, method: str, params: dict | None) -> None:
        self._write({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def receive_notification(self, *, timeout: float) -> dict | None:
        try:
            return self._notifications.get(timeout=timeout)
        except queue.Empty:
            return None

    def close(self) -> None:
        self._stop.set()
        if self._proc is not None:
            try:
                if self._proc.stdin is not None:
                    self._proc.stdin.close()
            except Exception:
                pass
            try:
                self._proc.terminate()
                self._proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self._proc.kill()
            except Exception:
                pass


# ---------------------------------------------------------------------------
# McpClient
# ---------------------------------------------------------------------------


class McpClient:
    """Thin wrapper that pairs a :class:`Transport` with the MCP method set
    needed by ssh-mcp tests.
    """

    PROTOCOL_VERSION = "2025-06-18"

    def __init__(self, transport: HttpTransport | StdioTransport) -> None:
        self.transport = transport
        self.server_info: dict | None = None

    # -- lifecycle ----------------------------------------------------------

    def initialize(self, *, client_name: str = "ssh-mcp-tests", client_version: str = "3.0.0") -> dict:
        if isinstance(self.transport, StdioTransport) and self.transport._proc is None:
            self.transport.start()
        params = {
            "protocolVersion": self.PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": client_version},
        }
        resp = self.transport.send_request("initialize", params, timeout=30)
        if "error" in resp:
            raise RuntimeError(f"initialize failed: {resp['error']}")
        self.server_info = resp.get("result", {})
        # Send `notifications/initialized` (required by MCP).
        self.transport.send_notification("notifications/initialized", {})
        # Open SSE listener for HTTP transport.
        if isinstance(self.transport, HttpTransport):
            self.transport.start_notification_stream()
        return self.server_info or {}

    def close(self) -> None:
        try:
            self.transport.close()
        except Exception:
            pass

    # -- tools --------------------------------------------------------------

    def list_tools(self) -> list[dict]:
        resp = self.transport.send_request("tools/list", {}, timeout=15)
        return resp.get("result", {}).get("tools", []) or []

    def call_tool(self, name: str, arguments: dict, *, timeout: float = 30.0) -> dict:
        params = {"name": name, "arguments": arguments}
        resp = self.transport.send_request("tools/call", params, timeout=timeout)
        if "error" in resp:
            return {"_rpc_error": resp["error"]}
        return resp.get("result", {})

    # -- resources ----------------------------------------------------------

    def list_resources(self) -> list[dict]:
        resp = self.transport.send_request("resources/list", {}, timeout=15)
        return resp.get("result", {}).get("resources", []) or []

    def read_resource(self, uri: str) -> dict:
        resp = self.transport.send_request("resources/read", {"uri": uri}, timeout=15)
        if "error" in resp:
            return {"_rpc_error": resp["error"]}
        return resp.get("result", {})

    def subscribe(self, uri: str) -> None:
        resp = self.transport.send_request("resources/subscribe", {"uri": uri}, timeout=15)
        if "error" in resp:
            raise RuntimeError(f"subscribe failed: {resp['error']}")

    def unsubscribe(self, uri: str) -> None:
        resp = self.transport.send_request("resources/unsubscribe", {"uri": uri}, timeout=15)
        if "error" in resp:
            raise RuntimeError(f"unsubscribe failed: {resp['error']}")

    # -- notifications ------------------------------------------------------

    def cancel(self, request_id: str | int, *, reason: str = "") -> None:
        params = {"requestId": request_id}
        if reason:
            params["reason"] = reason
        self.transport.send_notification("notifications/cancelled", params)

    def receive_notification(self, *, timeout: float = 5.0) -> dict | None:
        return self.transport.receive_notification(timeout=timeout)

    def drain_notifications(self) -> list[dict]:
        out: list[dict] = []
        while True:
            n = self.transport.receive_notification(timeout=0.05)
            if n is None:
                return out
            out.append(n)


# ---------------------------------------------------------------------------
# Convenience helpers used by the test suites
# ---------------------------------------------------------------------------


def call_tool_text(client: McpClient, name: str, arguments: dict, *, timeout: float = 30.0) -> str:
    """Run a tool and return the raw markdown text. Empty string on failure."""
    result = client.call_tool(name, arguments, timeout=timeout)
    if "_rpc_error" in result:
        return ""
    content = result.get("content") or []
    if not content:
        return ""
    first = content[0]
    if isinstance(first, dict) and first.get("type") == "text":
        return first.get("text", "") or ""
    return ""


def make_session_id() -> str:
    """Generate a random uuid for use as a fake session id in error tests."""
    return str(uuid.uuid4())
