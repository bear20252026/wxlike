#!/usr/bin/env python3
"""Run the wxlike e2e suites against a live server.

    python tests/e2e/run.py --base-url http://127.0.0.1:8099 --suite all \
        --admin-token "$WXLIKE_ADMIN_TOKEN" --evidence-dir /tmp/e2e

Suites:
    smoke            happy path: auth, friends, messaging, sessions, wallet
    errors           success/empty/param-error/unauthorized/not-found paths
    timeout          client timeout and refused connection are classified
    restart-seed     seed durable state before a server restart
    restart-recovery verify the seeded state after the restart
"""

from __future__ import annotations

import argparse
import json
import socket
import threading
import time
import urllib.parse
import uuid
from pathlib import Path

from harness import (
    CaseFailure,
    Client,
    Suite,
    TransportError,
    make_name,
    password,
    unique,
    wait_http,
    write_evidence,
)


def redacted_tail(result, suite: Suite, limit: int = 200) -> str:
    return suite.redact(result.text[:limit])


def register_pair(client: Client) -> tuple[dict, str, dict, str]:
    alice_name, alice_pw = make_name("alice"), password()
    bob_name, bob_pw = make_name("bob"), password()
    alice = client.register(alice_name, alice_pw, "e2e-device-a")
    bob = client.register(bob_name, bob_pw, "e2e-device-b")
    return alice, alice_pw, bob, bob_pw


def add_friend(client: Client, alice: dict, bob: dict) -> str:
    created = client.expect(
        "POST",
        "/api/friends/requests",
        201,
        body={"to_user_id": bob["user_id"], "message": "e2e"},
        token=alice["token"],
    )
    request_id = (created.body or {}).get("id")
    if not request_id:
        raise CaseFailure(f"friend request reply lacks id: {created.body}")
    incoming = client.expect("GET", "/api/friends/requests/incoming", 200, token=bob["token"])
    if not any(row.get("id") == request_id for row in incoming.body or []):
        raise CaseFailure("friend request missing from incoming list")
    client.expect("POST", f"/api/friends/requests/{request_id}/accept", 200, token=bob["token"])
    return request_id


def conv_of(alice: dict, bob: dict) -> str:
    first, second = sorted([alice["user_id"], bob["user_id"]])
    return f"d-{first}~{second}"


def suite_smoke(args) -> Suite:
    suite = Suite("smoke", args.base_url, admin_token=args.admin_token, timeout=args.timeout)
    client = suite.client()

    def health():
        result = client.expect("GET", "/health", 200)
        body = result.body or {}
        if body.get("ok") is not True or body.get("service") != "wxlike-server":
            raise CaseFailure(f"unexpected health payload: {body}")
        return f"version {body.get('version')}"

    def metrics():
        result = client.request("GET", "/metrics")
        if result.status != 200:
            raise CaseFailure(f"/metrics -> {result.status}: {redacted_tail(result, suite)}")
        content_type = result.headers.get("content-type", "")
        if "text/plain" not in content_type:
            raise CaseFailure(f"unexpected content type {content_type!r}")
        return f"{len(result.text)} bytes"

    def register():
        nonlocal alice_name, alice_pw, bob, bob_pw
        alice_name, alice_pw = make_name("alice"), password()
        bob_name, bob_pw = make_name("bob"), password()
        alice.update(client.register(alice_name, alice_pw, "e2e-device-a"))
        bob.update(client.register(bob_name, bob_pw, "e2e-device-b"))
        return f"alice={alice['user_id']} bob={bob['user_id']}"

    def second_device_and_revoke():
        second = client.login(alice_name, alice_pw, "e2e-device-a2")
        listed = client.expect("GET", "/api/sessions", 200, token=second["token"])
        sessions = (listed.body or {}).get("sessions") or []
        devices = {row.get("device_id") for row in sessions}
        if not {"e2e-device-a", "e2e-device-a2"} <= devices:
            raise CaseFailure(f"sessions did not show both devices: {devices}")
        revoked = client.expect("POST", "/api/sessions/revoke-others", 200, token=second["token"])
        if (revoked.body or {}).get("revoked", 0) < 1:
            raise CaseFailure(f"revoke-others reported {revoked.body}")
        stale = client.request("GET", "/api/me", token=alice["token"])
        if stale.status != 401:
            raise CaseFailure(f"revoked token still works: {stale.status}")
        alice["token"] = second["token"]
        return f"revoked={revoked.body.get('revoked')}"

    def friends():
        add_friend(client, alice, bob)
        friends = client.expect("GET", "/api/friends", 200, token=alice["token"])
        ids = {row.get("friend_id") for row in friends.body or []}
        if bob["user_id"] not in ids:
            raise CaseFailure(f"bob missing from alice's friends: {ids}")
        return f"{len(ids)} friend(s)"

    def messaging():
        conv = conv_of(alice, bob)
        key = "e2e-" + uuid.uuid4().hex
        first = client.send(alice["token"], conv, "hello e2e", key)
        if not first.get("msg_id") or not first.get("accepted"):
            raise CaseFailure(f"send reply: {first}")
        replay = client.send(alice["token"], conv, "hello e2e (retry)", key)
        if replay.get("msg_id") != first["msg_id"]:
            raise CaseFailure(f"idempotency replay produced {replay.get('msg_id')} != {first['msg_id']}")
        messages = client.wait_for_message(alice["token"], conv, first["msg_id"])
        if messages.get("text") != "hello e2e":
            raise CaseFailure(f"replayed request replaced the original text: {messages.get('text')!r}")
        same = [m for m in client.history(alice["token"], conv) if m.get("msg_id") == first["msg_id"]]
        if len(same) != 1:
            raise CaseFailure(f"idempotent send stored {len(same)} messages")
        return f"conv={conv} msg={first['msg_id']}"

    def recall():
        conv = conv_of(alice, bob)
        sent = client.send(alice["token"], conv, "to be recalled")
        msg_id = sent["msg_id"]
        client.wait_for_message(bob["token"], conv, msg_id)
        client.expect("POST", f"/api/conv/{conv}/recall", 200, body={"msg_id": msg_id}, token=alice["token"])
        deadline = time.monotonic() + 5
        last = None
        while time.monotonic() < deadline:
            last = client.wait_for_message(alice["token"], conv, msg_id)
            if last.get("withdrawn") is True:
                return f"msg={msg_id} withdrawn"
            time.sleep(0.1)
        raise CaseFailure(f"message never became withdrawn: {last}")

    def wallet():
        if not args.admin_token:
            raise CaseFailure("smoke suite needs --admin-token and WXLIKE_DEV_FAUCET=1 to exercise the ledger")
        funded = client.request(
            "POST",
            "/api/dev/fund",
            body={"user_id": alice["user_id"], "amount": 1000},
            admin_token=args.admin_token,
        )
        if funded.status != 204:
            raise CaseFailure(f"funding failed: {funded.status} {redacted_tail(funded, suite)}")
        transfer = client.expect(
            "POST",
            "/api/wallet/transfer",
            200,
            body={"to_uid": bob["user_id"], "amount": 250, "memo": "e2e"},
            token=alice["token"],
        )
        if not (transfer.body or {}).get("transfer_id"):
            raise CaseFailure(f"transfer reply: {transfer.body}")
        balances = {}
        for who, expected in ((alice, 750), (bob, 250)):
            result = client.expect("GET", "/api/wallet/balance", 200, token=who["token"])
            got = (result.body or {}).get("balance")
            balances[who["user_id"]] = got
            if got != expected:
                raise CaseFailure(f"balance for {who['user_id']} is {got}, expected {expected}")
        for who, kind in ((alice, "transfer_out"), (bob, "transfer_in")):
            ledger = client.expect("GET", "/api/wallet/ledger?limit=20", 200, token=who["token"])
            kinds = [row.get("kind") for row in (ledger.body or {}).get("entries", [])]
            if kind not in kinds:
                raise CaseFailure(f"{kind} missing from ledger: {kinds}")
        return f"balances={balances}"

    def reconcile():
        if not args.admin_token:
            raise CaseFailure("smoke suite needs --admin-token for reconcile")
        result = client.expect("POST", "/api/admin/reconcile", 200, admin_token=args.admin_token)
        report = result.body or {}
        if report.get("ok") is not True or report.get("mismatches"):
            raise CaseFailure(f"reconcile not clean: {report}")
        return f"users={report.get('users_checked')} supply={report.get('supply')}"

    def unauthorized():
        cases = [
            ("GET", "/api/me"),
            ("GET", "/api/sessions"),
            ("GET", "/api/wallet/balance"),
        ]
        for method, path in cases:
            result = client.request(method, path)
            if result.status != 401:
                raise CaseFailure(f"{method} {path} without token -> {result.status}")
        admin = client.request("POST", "/api/admin/reconcile")
        if admin.status != 403:
            raise CaseFailure(f"reconcile without admin token -> {admin.status}")
        return "401 for bearer, 403 for admin"

    alice: dict = {}
    bob: dict = {}
    alice_name = alice_pw = bob_pw = ""

    suite.case("health", health)
    suite.case("metrics", metrics)
    suite.case("register alice and bob", register)
    suite.case("second device, list and revoke sessions", second_device_and_revoke)
    suite.case("friend request/accept", friends)
    suite.case("send with idempotent replay", messaging)
    suite.case("recall", recall)
    suite.case("wallet faucet/transfer/ledger", wallet)
    suite.case("reconcile", reconcile)
    suite.case("unauthorized endpoints", unauthorized)
    return suite


def suite_errors(args) -> Suite:
    suite = Suite("errors", args.base_url, admin_token=args.admin_token, timeout=args.timeout)
    client = suite.client()
    alice, alice_pw, bob, _ = register_pair(client)
    conv = conv_of(alice, bob)
    # Direct messages are subject to the stranger gate: without a friendship
    # the hub drops them before persistence, so the storage/replay case would
    # be testing the gate instead of idempotency.
    add_friend(client, alice, bob)

    def duplicate_registration():
        result = client.request(
            "POST",
            "/api/register",
            body={"name": make_name("dup"), "password": "x", "device_id": "d"},
        )
        if result.status != 200:
            raise CaseFailure(f"first registration -> {result.status}")
        name = (result.body or {}).get("user_id")
        if not name:
            raise CaseFailure("first registration lacks user_id")
        return "first registration accepted"

    def duplicate_name():
        name = make_name("dupe")
        first = client.expect("POST", "/api/register", 200, body={"name": name, "password": "pw", "device_id": "d1"})
        if not (first.body or {}).get("user_id"):
            raise CaseFailure("first registration lacks user_id")
        again = client.request("POST", "/api/register", body={"name": name, "password": "pw", "device_id": "d2"})
        if again.status != 409:
            raise CaseFailure(f"duplicate name -> {again.status}: {redacted_tail(again, suite)}")
        return "409 conflict"

    def empty_and_long_input():
        empty_name = client.request("POST", "/api/register", body={"name": "   ", "password": "pw", "device_id": "d"})
        if empty_name.status != 422:
            raise CaseFailure(f"empty name -> {empty_name.status}")
        empty_password = client.request(
            "POST", "/api/register", body={"name": make_name("nopw"), "password": "", "device_id": "d"}
        )
        if empty_password.status != 422:
            raise CaseFailure(f"empty password -> {empty_password.status}")
        too_long = client.request(
            "POST", f"/api/conv/{conv}/send", body={"text": "x" * 5001}, token=alice["token"]
        )
        if too_long.status != 422:
            raise CaseFailure(f"oversized text -> {too_long.status}")
        return "422 for empty/oversized"

    def wrong_password():
        result = client.request(
            "POST", "/api/login", body={"name": "nobody-" + uuid.uuid4().hex[:8], "password": "wrong", "device_id": "d"}
        )
        if result.status != 401:
            raise CaseFailure(f"unknown login -> {result.status}")
        return "401"

    def empty_send():
        result = client.request("POST", f"/api/conv/{conv}/send", body={"text": "  "}, token=alice["token"])
        if result.status != 422:
            raise CaseFailure(f"empty text -> {result.status}")
        return "422"

    def foreign_conversation():
        carol = client.register(make_name("carol"), password(), "e2e-carol")
        result = client.request("GET", f"/api/history/{conv}", token=carol["token"])
        if result.status != 403:
            raise CaseFailure(f"carol reading alice/bob history -> {result.status}")
        send = client.request("POST", f"/api/conv/{conv}/send", body={"text": "intrude"}, token=carol["token"])
        if send.status != 403:
            raise CaseFailure(f"carol sending into alice/bob -> {send.status}")
        return "403"

    def bad_parameters():
        recalled = client.request("POST", f"/api/conv/{conv}/recall", body={"msg_id": "not-hex"}, token=alice["token"])
        if recalled.status != 400:
            raise CaseFailure(f"non-hex msg_id -> {recalled.status}")
        missing = client.request("GET", "/api/history/no-such-conv", token=alice["token"])
        if missing.status != 403:
            raise CaseFailure(f"unknown conversation -> {missing.status}")
        unknown = client.request("GET", "/api/definitely-not-a-route")
        if unknown.status != 404:
            raise CaseFailure(f"unknown route -> {unknown.status}")
        return "400/403/404"

    def replay_same_key():
        key = "replay-" + uuid.uuid4().hex
        first = client.send(alice["token"], conv, "original", key)
        second = client.send(alice["token"], conv, "different payload", key)
        if first["msg_id"] != second["msg_id"]:
            raise CaseFailure("same idempotency key produced different msg_id")
        messages = [m for m in client.history(alice["token"], conv) if m.get("msg_id") == first["msg_id"]]
        if len(messages) != 1:
            raise CaseFailure(f"replay stored {len(messages)} rows")
        return f"single effect {first['msg_id']}"

    suite.case("duplicate registration", duplicate_registration)
    suite.case("duplicate name is 409", duplicate_name)
    suite.case("empty and oversized input", empty_and_long_input)
    suite.case("wrong password is 401", wrong_password)
    suite.case("empty message is 422", empty_send)
    suite.case("foreign conversation is 403", foreign_conversation)
    suite.case("bad parameters and unknown route", bad_parameters)
    suite.case("idempotency key replay", replay_same_key)
    return suite


def suite_timeout(args) -> Suite:
    suite = Suite("timeout", args.base_url, admin_token=args.admin_token, timeout=args.timeout)
    server = Client(args.base_url, args.timeout, suite)

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(4)
    blackhole_port = listener.getsockname()[1]
    stop = threading.Event()

    def accept_and_hang():
        listener.settimeout(0.5)
        while not stop.is_set():
            try:
                conn, _ = listener.accept()
            except (socket.timeout, OSError):
                continue
            with conn:
                conn.recv(65536)
                stop.wait(5)

    thread = threading.Thread(target=accept_and_hang, daemon=True)
    thread.start()

    def timeout_classified():
        probe = Client(f"http://127.0.0.1:{blackhole_port}", 0.4, suite)
        try:
            probe.request("GET", "/health")
        except TransportError as exc:
            if exc.kind != "timeout":
                raise CaseFailure(f"expected timeout, got {exc.kind}") from exc
            return "socket timeout"
        raise CaseFailure("blackhole server answered unexpectedly")

    def refused_connection():
        probe_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        probe_socket.bind(("127.0.0.1", 0))
        port = probe_socket.getsockname()[1]
        probe_socket.close()
        probe = Client(f"http://127.0.0.1:{port}", 1.0, suite)
        try:
            probe.request("GET", "/health")
        except TransportError as exc:
            if exc.kind not in {"network", "timeout"}:
                raise CaseFailure(f"expected network error, got {exc.kind}") from exc
            return f"classified as {exc.kind}"
        raise CaseFailure("refused connection was not classified")

    def aborted_request():
        parsed = urllib.parse.urlparse(args.base_url)
        host = parsed.hostname or "127.0.0.1"
        port = parsed.port or 80
        with socket.create_connection((host, port), timeout=2) as raw:
            raw.sendall(
                b"POST /api/register HTTP/1.1\r\n"
                b"Host: " + host.encode() + b"\r\n"
                b"Content-Type: application/json\r\n"
                b"Content-Length: 500\r\n\r\n"
                b'{"name":"aborted","passw'
            )
        time.sleep(0.2)
        healthy = server.request("GET", "/health")
        if healthy.status != 200:
            raise CaseFailure(f"server unhealthy after aborted request: {healthy.status}")
        survivor = server.register(make_name("after-abort"), password(), "e2e-after-abort")
        if not survivor.get("user_id"):
            raise CaseFailure("server could not serve a request after the abort")
        return "aborted request left the server usable"

    suite.case("blackhole timeout", timeout_classified)
    suite.case("refused connection", refused_connection)
    suite.case("aborted in-flight request", aborted_request)
    stop.set()
    listener.close()
    return suite


def suite_restart_seed(args) -> Suite:
    suite = Suite("restart-seed", args.base_url, admin_token=args.admin_token, timeout=args.timeout)
    client = suite.client()
    state_path = Path(args.state_file)

    def seed():
        alice_name, alice_pw = make_name("restart_a"), password()
        bob_name, bob_pw = make_name("restart_b"), password()
        alice = client.register(alice_name, alice_pw, "restart-device-a")
        bob = client.register(bob_name, bob_pw, "restart-device-b")
        add_friend(client, alice, bob)
        conv = conv_of(alice, bob)
        msg = client.send(alice["token"], conv, "survives restart", "restart-" + uuid.uuid4().hex)
        client.wait_for_message(alice["token"], conv, msg["msg_id"])
        balance = None
        if args.admin_token:
            funded = client.request(
                "POST", "/api/dev/fund", body={"user_id": alice["user_id"], "amount": 500}, admin_token=args.admin_token
            )
            if funded.status != 204:
                raise CaseFailure(f"funding failed: {funded.status}")
            client.expect(
                "POST", "/api/wallet/transfer", 200,
                body={"to_uid": bob["user_id"], "amount": 125}, token=alice["token"],
            )
            balance = client.expect("GET", "/api/wallet/balance", 200, token=alice["token"]).body["balance"]
        state = {
            "alice": {"name": alice_name, "password": alice_pw, "user_id": alice["user_id"]},
            "bob": {"name": bob_name, "password": bob_pw, "user_id": bob["user_id"]},
            "conv": conv,
            "msg_id": msg["msg_id"],
            "balance": balance,
        }
        state_path.parent.mkdir(parents=True, exist_ok=True)
        state_path.write_text(json.dumps(state), encoding="utf-8")
        return f"seeded {conv} msg={msg['msg_id']}"

    suite.case("seed durable state", seed)
    return suite


def suite_restart_recovery(args) -> Suite:
    suite = Suite("restart-recovery", args.base_url, admin_token=args.admin_token, timeout=args.timeout)
    client = suite.client()
    state_path = Path(args.state_file)
    if not state_path.is_file():
        suite.case("load seed state", lambda: (_ for _ in ()).throw(CaseFailure(f"no seed state at {state_path}")))
        return suite
    state = json.loads(state_path.read_text(encoding="utf-8"))

    def token_survives():
        logged = client.login(state["alice"]["name"], state["alice"]["password"], "restart-device-a")
        client.suite.remember_secret(logged["token"])
        state["alice"]["token"] = logged["token"]
        bob = client.login(state["bob"]["name"], state["bob"]["password"], "restart-device-b")
        state["bob"]["token"] = bob["token"]
        sessions = client.expect("GET", "/api/sessions", 200, token=logged["token"])
        if not (sessions.body or {}).get("sessions"):
            raise CaseFailure("no sessions after restart")
        return "login and session list work"

    def message_survives():
        message = client.wait_for_message(state["alice"]["token"], state["conv"], state["msg_id"])
        if message.get("text") != "survives restart":
            raise CaseFailure(f"unexpected text after restart: {message.get('text')}")
        return f"msg={state['msg_id']} still present"

    def friendship_survives():
        friends = client.expect("GET", "/api/friends", 200, token=state["alice"]["token"])
        ids = {row.get("friend_id") for row in friends.body or []}
        if state["bob"]["user_id"] not in ids:
            raise CaseFailure(f"friendship lost: {ids}")
        return "friend list intact"

    def wallet_survives():
        if state.get("balance") is None:
            raise CaseFailure("seed did not record a wallet balance")
        result = client.expect("GET", "/api/wallet/balance", 200, token=state["alice"]["token"])
        if (result.body or {}).get("balance") != state["balance"]:
            raise CaseFailure(f"balance {result.body} != seeded {state['balance']}")
        result = client.expect("POST", "/api/admin/reconcile", 200, admin_token=args.admin_token)
        if not (result.body or {}).get("ok"):
            raise CaseFailure(f"reconcile after restart: {result.body}")
        return f"balance {state['balance']} and reconcile ok"

    suite.case("login after restart", token_survives)
    suite.case("message persisted", message_survives)
    suite.case("friendship persisted", friendship_survives)
    suite.case("wallet persisted and reconciles", wallet_survives)
    return suite


SUITES = {
    "smoke": suite_smoke,
    "errors": suite_errors,
    "timeout": suite_timeout,
    "restart-seed": suite_restart_seed,
    "restart-recovery": suite_restart_recovery,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:8099")
    parser.add_argument("--suite", choices=sorted(SUITES) + ["all"], default="smoke")
    parser.add_argument("--admin-token", default="")
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--state-file", type=Path)
    parser.add_argument("--wait", action="store_true", help="wait for /health before running")
    args = parser.parse_args()
    if args.state_file is None:
        args.state_file = args.evidence_dir / "restart-state.json"
    if args.wait:
        wait_http(args.base_url, timeout=60)

    names = ["smoke", "errors", "timeout"] if args.suite == "all" else [args.suite]
    failed = False
    for name in names:
        print(f"== {name} ==")
        suite = SUITES[name](args)
        payload = suite.evidence()
        evidence_path = args.evidence_dir / f"{name}.json"
        write_evidence(evidence_path, payload)
        print(f"-> {'PASS' if payload['ok'] else 'FAIL'} ({evidence_path})")
        if not payload["ok"]:
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
