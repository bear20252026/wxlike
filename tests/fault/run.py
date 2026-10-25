#!/usr/bin/env python3
"""Fault and privilege scenarios that need more than one HTTP call.

Scenarios either run for real or are recorded as ``blocked`` with the observed
reason and the open gap id. ``blocked`` never counts as a pass, so a release
cannot claim coverage for a scenario that was not executed.

    python tests/fault/run.py --base-url http://127.0.0.1:8099 \
        --admin-token "$TOKEN" --evidence-dir /tmp/fault \
        --restart-cmd "docker restart wxlike-smoke"

Optional:
    --partition-on-cmd / --partition-off-cmd   real network isolation
    --server-bin path/to/wxlike-server          migration failure drill
"""

from __future__ import annotations

import argparse
import os
import shlex
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "e2e"))
from harness import (  # noqa: E402
    Blocked,
    CaseFailure,
    Client,
    Suite,
    TransportError,
    make_name,
    password,
    wait_http,
    write_evidence,
)


def free_port(kind: str = "tcp") -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM if kind == "tcp" else socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def run_cmd(command: str, timeout: float = 90.0) -> str:
    argv = shlex.split(command)
    if not argv:
        raise CaseFailure("empty command")
    result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise CaseFailure(f"command failed ({result.returncode}): {argv[0]}\n{result.stderr.strip()[:400]}")
    return (result.stdout + result.stderr).strip()


def suite_restart(args) -> Suite:
    suite = Suite("restart-during-send", args.base_url, args.admin_token, args.timeout)
    client = suite.client()

    def restart_with_idempotency():
        if not args.restart_cmd:
            raise Blocked("no --restart-cmd; operator must supply the service restart command")
        alice_name, alice_pw = make_name("fault_a"), password()
        bob_name, bob_pw = make_name("fault_b"), password()
        alice = client.register(alice_name, alice_pw, "fault-device-a")
        bob = client.register(bob_name, bob_pw, "fault-device-b")
        conv = f"d-{min(alice['user_id'], bob['user_id'])}~{max(alice['user_id'], bob['user_id'])}"
        key = "fault-" + uuid.uuid4().hex
        first = client.send(alice["token"], conv, "sent before restart", key)
        run_cmd(args.restart_cmd)
        wait_http(args.base_url, timeout=90)
        replayed = client.send(alice["token"], conv, "retry after restart", key)
        if replayed.get("msg_id") != first.get("msg_id"):
            raise CaseFailure(f"retry after restart produced {replayed.get('msg_id')} != {first.get('msg_id')}")
        messages = [m for m in client.history(alice["token"], conv) if m.get("msg_id") == first["msg_id"]]
        if len(messages) != 1:
            raise CaseFailure(f"restart replay stored {len(messages)} rows")
        return f"restart at {args.base_url}; one effect {first['msg_id']}"

    def partition_round_trip():
        if not (args.partition_on_cmd and args.partition_off_cmd):
            raise Blocked("no --partition-on-cmd/--partition-off-cmd; runner cannot isolate the network")
        run_cmd(args.partition_on_cmd)
        try:
            try:
                client.request("GET", "/health", timeout=3)
            except TransportError as exc:
                observed = exc.kind
            else:
                raise CaseFailure("server still answers while the network is partitioned")
        finally:
            run_cmd(args.partition_off_cmd)
        wait_http(args.base_url, timeout=60)
        return f"partition observed as {observed}, recovered"

    suite.case("idempotent send across restart", restart_with_idempotency)
    suite.case("network partition and recovery", partition_round_trip)
    return suite


def suite_privileges(args) -> Suite:
    suite = Suite("privileges", args.base_url, args.admin_token, args.timeout)
    client = suite.client()

    def community_channel_privilege():
        alice = client.register(make_name("owner"), password(), "priv-device-a")
        bob = client.register(make_name("outsider"), password(), "priv-device-b")
        community = client.expect(
            "POST", "/api/communities", 201, body={"name": make_name("c"), "member_limit": 10}, token=alice["token"]
        )
        community_id = (community.body or {}).get("community_id")
        if not community_id:
            raise CaseFailure(f"community reply: {community.body}")
        channel = client.expect(
            "POST",
            f"/api/communities/{community_id}/channels",
            201,
            body={"name": "general", "kind": "text"},
            token=alice["token"],
        )
        channel_id = (channel.body or {}).get("channel_id")
        if not channel_id:
            raise CaseFailure(f"channel reply: {channel.body}")
        sent = client.expect(
            "POST", f"/api/channels/{channel_id}/messages", 200, body={"text": "owner message"}, token=alice["token"]
        )
        if not (sent.body or {}).get("msg_id"):
            raise CaseFailure(f"owner send reply: {sent.body}")
        outsider_send = client.request(
            "POST", f"/api/channels/{channel_id}/messages", body={"text": "intrude"}, token=bob["token"]
        )
        if outsider_send.status != 403:
            raise CaseFailure(f"non-member send -> {outsider_send.status}")
        outsider_read = client.request("GET", f"/api/channels/{channel_id}/messages", token=bob["token"])
        if outsider_read.status != 403:
            raise CaseFailure(f"non-member read -> {outsider_read.status}")
        escalate = client.request(
            "POST",
            f"/api/channel/{channel_id}/roles",
            body={"user_id": bob["user_id"], "role": "moderator"},
            token=bob["token"],
        )
        if escalate.status not in {403, 404}:
            raise CaseFailure(f"outsider role escalation -> {escalate.status}")
        return "owner can post; non-member send/read/role-change refused"

    def payment_callback_replay():
        # The contract has no provider callback endpoint yet; prove that
        # observation instead of pretending the replay passed.
        probe = client.request("POST", "/api/payment/callback", body={"operation_id": "probe"})
        if probe.status != 404:
            raise CaseFailure(
                f"payment callback unexpectedly exists ({probe.status}); implement the replay scenario before closing PAYMENT-CALLBACK-REPLAY"
            )
        raise Blocked("POST /api/payment/callback -> 404; PAYMENT-CALLBACK-REPLAY (G06) is open")

    def rtc_weak_network():
        probe = client.request("GET", "/api/turn/credentials")
        if probe.status not in {404, 401}:
            raise CaseFailure(f"unexpected TURN behaviour without credentials: {probe.status}")
        raise Blocked(
            f"/api/turn/credentials -> {probe.status}; no TURN/SFU provider is attached, RTC-WEAK-NET (G09) is open"
        )

    suite.case("community/channel privileges", community_channel_privilege)
    suite.case("payment callback replay", payment_callback_replay)
    suite.case("RTC weak network", rtc_weak_network)
    return suite


def suite_migration(args) -> Suite:
    suite = Suite("migration-failure", args.base_url, args.admin_token, args.timeout)

    def corrupt_db_refuses_to_start():
        if not args.server_bin:
            raise Blocked("no --server-bin supplied; migration drill not executed")
        binary = Path(args.server_bin)
        if not binary.is_file():
            raise CaseFailure(f"server binary not found: {binary}")
        with tempfile.TemporaryDirectory() as tmp:
            db = Path(tmp, "corrupt.db")
            db.write_bytes(b"this is not a sqlite database\n" * 64)
            env = dict(os.environ)
            env.update(
                {
                    "WXLIKE_DB": str(db),
                    "WXLIKE_MEDIA_DIR": str(Path(tmp, "media")),
                    "WXLIKE_HTTP": f"127.0.0.1:{free_port()}",
                    "WXLIKE_QUIC": f"127.0.0.1:{free_port('udp')}",
                    "WXLIKE_MQ": "memory",
                    "WXLIKE_DEV_INSECURE": "1",
                }
            )
            try:
                result = subprocess.run([str(binary)], env=env, capture_output=True, text=True, timeout=20)
            except subprocess.TimeoutExpired:
                raise CaseFailure("server kept running with a corrupt database; migrations do not fail closed")
            output = (result.stdout + result.stderr).strip()
            if result.returncode == 0:
                raise CaseFailure(f"server exited 0 on a corrupt database:\n{output[:500]}")
            return f"exit {result.returncode} with corrupt db"

    suite.case("corrupt database refuses startup", corrupt_db_refuses_to_start)
    return suite


SUITES = {
    "restart": suite_restart,
    "privileges": suite_privileges,
    "migration": suite_migration,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:8099")
    parser.add_argument("--suite", choices=sorted(SUITES) + ["all"], default="all")
    parser.add_argument("--admin-token", default="")
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--restart-cmd", default="")
    parser.add_argument("--partition-on-cmd", default="")
    parser.add_argument("--partition-off-cmd", default="")
    parser.add_argument("--server-bin", default="")
    parser.add_argument("--release", action="store_true", help="treat tracked-gap scenarios as release blockers")
    args = parser.parse_args()

    names = sorted(SUITES) if args.suite == "all" else [args.suite]
    failed = False
    for name in names:
        print(f"== {name} ==")
        suite = SUITES[name](args)
        payload = suite.evidence()
        path = args.evidence_dir / f"{name}.json"
        write_evidence(path, payload)
        blocked = [case["name"] for case in payload["cases"] if case["status"] == "blocked"]
        tracked_only = all(case["status"] in {"pass", "blocked"} for case in payload["cases"])
        print(f"-> {'PASS' if payload['ok'] else 'FAIL'} ({path})" + (f"; blocked: {blocked}" if blocked else ""))
        if not payload["ok"]:
            if tracked_only and not args.release:
                print("   tracked gaps recorded; daily gate continues (release gate still blocks)")
            else:
                failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
