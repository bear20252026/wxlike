#!/usr/bin/env python3
"""HTTP end-to-end harness for a running wxlike-server.

Only the standard library is used so the same file runs against a container,
a locally built binary, or the smoke stack. Every case records a real HTTP
status and response body excerpt; tokens are redacted before anything is
written to evidence.
"""

from __future__ import annotations

import json
import os
import socket
import time
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

DEFAULT_TIMEOUT = 10.0


def now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:12]}"


class TransportError(RuntimeError):
    def __init__(self, kind: str, detail: str):
        super().__init__(f"{kind}: {detail}")
        self.kind = kind
        self.detail = detail


class CaseFailure(AssertionError):
    pass


class Blocked(Exception):
    """A scenario that could not run because an upstream gap is still open."""

    def __init__(self, reason: str):
        super().__init__(reason)
        self.reason = reason


@dataclass
class Response:
    status: int
    body: Any
    text: str
    headers: dict[str, str]
    elapsed_ms: int


@dataclass
class Suite:
    name: str
    base_url: str
    admin_token: str | None = None
    timeout: float = DEFAULT_TIMEOUT
    cases: list[dict] = field(default_factory=list)
    secret_values: set[str] = field(default_factory=set)

    def client(self) -> "Client":
        return Client(self.base_url, self.timeout, self)

    def redact(self, text: str) -> str:
        for secret in self.secret_values:
            if secret:
                text = text.replace(secret, "<redacted>")
        return text

    def remember_secret(self, value: str) -> None:
        if value:
            self.secret_values.add(value)

    def case(self, name: str, fn: Callable[[], Any]) -> bool:
        started = time.monotonic()
        try:
            detail = fn()
            entry = {"name": name, "status": "pass", "detail": detail or ""}
        except Blocked as exc:
            entry = {"name": name, "status": "blocked", "detail": self.redact(exc.reason)}
        except CaseFailure as exc:
            entry = {"name": name, "status": "fail", "detail": self.redact(str(exc))}
        except TransportError as exc:
            entry = {"name": name, "status": "fail", "detail": f"transport {exc.kind}: {exc.detail}"}
        except Exception as exc:  # noqa: BLE001 - evidence must record unexpected failures
            entry = {"name": name, "status": "fail", "detail": self.redact(f"{type(exc).__name__}: {exc}")}
        entry["duration_ms"] = int((time.monotonic() - started) * 1000)
        self.cases.append(entry)
        print(f"  [{entry['status']}] {name}" + (f" — {entry['detail']}" if entry["detail"] else ""))
        return entry["status"] == "pass"

    def blocked(self, name: str, reason: str) -> None:
        self.cases.append({"name": name, "status": "blocked", "detail": reason, "duration_ms": 0})
        print(f"  [blocked] {name} — {reason}")

    def ok(self) -> bool:
        return bool(self.cases) and all(case["status"] == "pass" for case in self.cases)

    def evidence(self, extra: dict | None = None) -> dict:
        payload = {
            "suite": self.name,
            "base_url": self.base_url,
            "finished_at": now_iso(),
            "ok": self.ok(),
            "cases": self.cases,
        }
        if extra:
            payload["extra"] = extra
        return payload


class Client:
    def __init__(self, base_url: str, timeout: float, suite: Suite):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.suite = suite
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(
        self,
        method: str,
        path: str,
        body: Any | None = None,
        token: str | None = None,
        admin_token: str | None = None,
        timeout: float | None = None,
    ) -> Response:
        data = None
        headers = {"Accept": "application/json", "Connection": "close"}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if token:
            headers["Authorization"] = f"Bearer {token}"
            self.suite.remember_secret(token)
        if admin_token:
            headers["x-admin-token"] = admin_token
            self.suite.remember_secret(admin_token)
        req = urllib.request.Request(self.base_url + path, data=data, headers=headers, method=method)
        started = time.monotonic()
        try:
            with self.opener.open(req, timeout=timeout or self.timeout) as resp:
                raw = resp.read().decode("utf-8", "replace")
                status = resp.status
                resp_headers = {k.lower(): v for k, v in resp.headers.items()}
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode("utf-8", "replace")
            status = exc.code
            resp_headers = {k.lower(): v for k, v in exc.headers.items()}
        except (socket.timeout, TimeoutError) as exc:
            raise TransportError("timeout", str(exc)) from exc
        except urllib.error.URLError as exc:
            kind = "timeout" if isinstance(exc.reason, (socket.timeout, TimeoutError)) else "network"
            raise TransportError(kind, str(exc.reason)) from exc
        except ConnectionError as exc:
            raise TransportError("network", str(exc)) from exc
        elapsed = int((time.monotonic() - started) * 1000)
        try:
            parsed = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            parsed = None
        return Response(status=status, body=parsed, text=raw, headers=resp_headers, elapsed_ms=elapsed)

    def expect(self, method: str, path: str, status: int | tuple[int, ...], **kwargs) -> Response:
        result = self.request(method, path, **kwargs)
        wanted = (status,) if isinstance(status, int) else status
        if result.status not in wanted:
            raise CaseFailure(f"{method} {path} -> {result.status}, wanted {wanted}: {self.suite.redact(result.text[:300])}")
        return result

    # -- domain helpers ---------------------------------------------------
    def register(self, name: str, password: str, device_id: str, device_name: str = "") -> dict:
        result = self.expect(
            "POST",
            "/api/register",
            200,
            body={"name": name, "password": password, "device_id": device_id, "device_name": device_name or device_id},
        )
        reply = result.body or {}
        if not reply.get("token") or not reply.get("user_id"):
            raise CaseFailure(f"register reply lacks user_id/token: {reply}")
        self.suite.remember_secret(reply["token"])
        return reply

    def login(self, name: str, password: str, device_id: str) -> dict:
        result = self.expect(
            "POST",
            "/api/login",
            200,
            body={"name": name, "password": password, "device_id": device_id, "device_name": device_id},
        )
        reply = result.body or {}
        if not reply.get("token"):
            raise CaseFailure(f"login reply lacks token: {reply}")
        self.suite.remember_secret(reply["token"])
        return reply

    def send(self, token: str, conv: str, text: str, idempotency_key: str | None = None) -> dict:
        body: dict[str, Any] = {"text": text}
        if idempotency_key:
            body["idempotency_key"] = idempotency_key
        result = self.expect("POST", f"/api/conv/{conv}/send", 200, body=body, token=token)
        return result.body or {}

    def history(self, token: str, conv: str, limit: int = 50) -> list[dict]:
        result = self.expect("GET", f"/api/history/{conv}?limit={limit}", 200, token=token)
        return (result.body or {}).get("messages", [])

    def wait_for_message(self, token: str, conv: str, msg_id: str, timeout: float = 5.0) -> dict:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for message in self.history(token, conv):
                if message.get("msg_id") == msg_id:
                    return message
            time.sleep(0.1)
        raise CaseFailure(f"message {msg_id} not present in history of {conv} after {timeout}s")


def make_name(prefix: str) -> str:
    # 3-32 chars, letters/digits/underscore/dash per validate::name convention.
    return unique(prefix).replace("-", "_")


def password() -> str:
    return "E2e-" + uuid.uuid4().hex


def write_evidence(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8", newline="\n")


def wait_http(base_url: str, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        try:
            result = Client(base_url, 2.0, Suite(name="probe", base_url=base_url)).request("GET", "/health")
            if result.status == 200:
                return
            last = f"status {result.status}"
        except TransportError as exc:
            last = f"{exc.kind}: {exc.detail}"
        time.sleep(0.25)
    raise SystemExit(f"server at {base_url} did not become healthy: {last}")


def env(name: str, default: str = "") -> str:
    return os.environ.get(name, default)
