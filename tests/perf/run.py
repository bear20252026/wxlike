#!/usr/bin/env python3
"""Capacity check with measured latency, throughput and error rate.

Starts a real server on a temporary database, drives concurrent sends, and
fails when the recorded percentiles exceed the checked-in budgets. Evidence
contains the observed numbers, so a budget change must be justified by data.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "e2e"))
from harness import Client, Suite, TransportError, make_name, password, write_evidence  # noqa: E402


def free_port(kind: str = "tcp") -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM if kind == "tcp" else socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(round(fraction * (len(ordered) - 1)))))
    return ordered[index]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-bin", required=True)
    parser.add_argument("--budgets", type=Path, default=Path("ci/jobs/performance/budgets.json"))
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--concurrency", type=int, default=16)
    parser.add_argument("--requests", type=int, default=400)
    parser.add_argument("--health-samples", type=int, default=100)
    parser.add_argument("--history-samples", type=int, default=50)
    args = parser.parse_args()

    budgets = json.loads(args.budgets.read_text(encoding="utf-8"))
    binary = Path(args.server_bin)
    if not binary.is_file():
        raise SystemExit(f"server binary not found: {binary}")

    http_port = free_port()
    quic_port = free_port("udp")
    base = f"http://127.0.0.1:{http_port}"
    admin = "perf-" + uuid.uuid4().hex
    tmp = tempfile.TemporaryDirectory()
    log_path = Path(tmp.name, "server.log")
    env = dict(os.environ)
    env.update(
        {
            "WXLIKE_DB": str(Path(tmp.name, "wxlike.db")),
            "WXLIKE_MEDIA_DIR": str(Path(tmp.name, "media")),
            "WXLIKE_HTTP": f"127.0.0.1:{http_port}",
            "WXLIKE_QUIC": f"127.0.0.1:{quic_port}",
            "WXLIKE_MQ": "memory",
            "WXLIKE_DEV_INSECURE": "1",
            "WXLIKE_DEV_FAUCET": "1",
            "WXLIKE_ADMIN_TOKEN": admin,
            "WXLIKE_LIVE_INGEST_SECRET": "perf-ingest-" + uuid.uuid4().hex,
            "WXLIKE_LOG": "warn",
        }
    )
    log = log_path.open("w", encoding="utf-8")
    process = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=subprocess.STDOUT)
    suite = Suite("performance", base, admin_token=admin, timeout=30.0)
    client = Client(base, 30.0, suite)
    results: dict = {
        "budgets": budgets,
        "config": {
            "concurrency": args.concurrency,
            "requests": args.requests,
            "health_samples": args.health_samples,
            "history_samples": args.history_samples,
            "budgets_file": str(args.budgets),
            "server_bin": str(binary),
        },
    }
    failed = False

    try:
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            try:
                if client.request("GET", "/health", timeout=2).status == 200:
                    break
            except TransportError:
                pass
            if process.poll() is not None:
                raise SystemExit(f"server exited during startup:\n{log_path.read_text(encoding='utf-8')[-2000:]}")
            time.sleep(0.2)
        else:
            raise SystemExit("server did not become healthy")

        alice = client.register(make_name("perf_a"), password(), "perf-device-a")
        bob = client.register(make_name("perf_b"), password(), "perf-device-b")
        conv = f"d-{min(alice['user_id'], bob['user_id'])}~{max(alice['user_id'], bob['user_id'])}"

        health_latencies = []
        for _ in range(args.health_samples):
            started = time.perf_counter()
            client.expect("GET", "/health", 200)
            health_latencies.append((time.perf_counter() - started) * 1000)

        send_latencies: list[float] = []
        errors: list[str] = []
        msg_ids: list[str] = []
        lock = threading.Lock()
        per_worker = max(1, args.requests // args.concurrency)

        def worker(worker_id: int) -> None:
            local_client = Client(base, 30.0, suite)
            for index in range(per_worker):
                key = f"perf-{worker_id}-{index}-{uuid.uuid4().hex[:6]}"
                started = time.perf_counter()
                try:
                    reply = local_client.send(alice["token"], conv, f"load {worker_id}/{index}", key)
                    elapsed = (time.perf_counter() - started) * 1000
                    if not reply.get("msg_id"):
                        raise RuntimeError(f"send reply: {reply}")
                    with lock:
                        send_latencies.append(elapsed)
                        msg_ids.append(reply["msg_id"])
                except (TransportError, AssertionError, RuntimeError) as exc:
                    with lock:
                        errors.append(f"worker {worker_id}: {exc}")

        started = time.perf_counter()
        threads = [threading.Thread(target=worker, args=(i,)) for i in range(args.concurrency)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        wall = time.perf_counter() - started
        total = len(send_latencies) + len(errors)

        history_latencies = []
        for _ in range(args.history_samples):
            started = time.perf_counter()
            client.expect("GET", f"/api/history/{conv}?limit=50", 200, token=alice["token"])
            history_latencies.append((time.perf_counter() - started) * 1000)

        reconcile = client.expect("POST", "/api/admin/reconcile", 200, admin_token=admin).body or {}
        if not reconcile.get("ok"):
            errors.append(f"reconcile: {reconcile}")

        observed = {
            "health_p95_ms": round(percentile(health_latencies, 0.95), 2),
            "send_p95_ms": round(percentile(send_latencies, 0.95), 2),
            "send_p99_ms": round(percentile(send_latencies, 0.99), 2),
            "history_p95_ms": round(percentile(history_latencies, 0.95), 2),
            "error_rate": round(len(errors) / total, 6) if total else 1.0,
            "throughput_rps": round(total / wall, 2) if wall else 0.0,
            "requests": total,
            "unique_msg_ids": len(set(msg_ids)),
        }
        results["observed"] = observed
        results["errors"] = errors[:20]

        checks = [
            ("health_p95_ms", observed["health_p95_ms"] <= budgets["health_p95_ms"]),
            ("send_p95_ms", observed["send_p95_ms"] <= budgets["send_p95_ms"]),
            ("history_p95_ms", observed["history_p95_ms"] <= budgets["history_p95_ms"]),
            ("error_rate", observed["error_rate"] <= budgets["max_error_rate"]),
            ("throughput_rps", observed["throughput_rps"] >= budgets["min_throughput_rps"]),
            ("unique_msg_ids", observed["unique_msg_ids"] == len(msg_ids)),
        ]
        failed = not all(ok for _, ok in checks)
        results["checks"] = [{"name": name, "ok": ok} for name, ok in checks]
        for name, ok in checks:
            print(f"  [{'pass' if ok else 'fail'}] {name}")
        print(json.dumps(observed, indent=2))
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
        log.close()
        tmp.cleanup()

    write_evidence(args.evidence_dir / "performance.json", results)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
