#!/usr/bin/env python3
"""Minimal `/ws` fixture for the C-04 browser test.

It speaks the same bincode frames as `crates/net/src/wire.rs` but only the
subset the browser test needs:

* first connection: ClientHello + Hello -> Auth; read Subscribe; send Msg(42);
  close, forcing the browser to reconnect;
* second connection: ClientHello + Hello -> Auth; read Subscribe + Resume(42);
  send Msg(43). If the browser did not resume, it gets nothing and the test
  times out.

Not a product server: no auth, no hub, no mailbox. It exists only to prove the
browser's binary handshake/reconnect path in a real Chrome.
"""

from __future__ import annotations

import asyncio
import struct

import websockets

PORT = 18765
CONV = "d-browser-test"
USER = "u-browser-test"


def u32(value: int) -> bytes:
    return struct.pack("<I", value)


def u64(value: int) -> bytes:
    return struct.pack("<Q", value)


def text(value: str) -> bytes:
    raw = value.encode("utf-8")
    return u64(len(raw)) + raw


def frame(variant: int) -> bytes:
    return u32(variant)


def auth_frame() -> bytes:
    # WireFrame::Auth { user_id, device_id, session_id, version }
    return (
        frame(6)
        + text(USER)
        + text("web-browser-test")
        + text("s-browser-test")
        + bytes([3])
    )


def msg_frame(seq: int, body: str) -> bytes:
    # WireFrame::Msg(Envelope { version, conv_id, sender, to, seq, kind,
    # body, sent_at_ms, msg_id, quote_of, receipt })
    return (
        frame(3)
        + bytes([2])
        + text(CONV)
        + text(USER)
        + u64(0)  # to: empty
        + u64(seq)
        + u32(0)  # MsgKind::Text
        + u32(0)  # MsgBody::Text
        + text(body)
        + u64(1_700_000_000_000 + seq)
        + bytes(range(16))
        + bytes([0])  # quote_of: None
        + bytes([0])  # receipt: None
    )


def read_string(data: bytes, pos: int) -> tuple[str, int]:
    length = struct.unpack_from("<Q", data, pos)[0]
    pos += 8
    return data[pos : pos + length].decode("utf-8"), pos + length


def read_u32(data: bytes, pos: int) -> tuple[int, int]:
    return struct.unpack_from("<I", data, pos)[0], pos + 4


def read_u8(data: bytes, pos: int) -> tuple[int, int]:
    return data[pos], pos + 1


def decode_client_frame(data: bytes) -> tuple[int, dict[str, object]]:
    variant, pos = read_u32(data, 0)
    if variant == 5:  # ClientHello
        device, pos = read_string(data, pos)
        protocol, pos = read_u8(data, pos)
        caps, pos = read_u32(data, pos)
        return variant, {"device": device, "protocol": protocol, "caps": caps}
    if variant == 0:  # Hello
        token, pos = read_string(data, pos)
        return variant, {"token": token}
    if variant == 7:  # Subscribe
        count = struct.unpack_from("<Q", data, pos)[0]
        pos += 8
        scopes = []
        for _ in range(count):
            scope, pos = read_string(data, pos)
            scopes.append(scope)
        return variant, {"scopes": scopes}
    if variant == 10:  # Resume
        conv, pos = read_string(data, pos)
        cursor = struct.unpack_from("<Q", data, pos)[0]
        return variant, {"conv": conv, "cursor": cursor}
    return variant, {}


async def handler(ws, second: bool) -> None:
    print(f"fixture: connected {'second' if second else 'first'}", flush=True)
    hello = decode_client_frame(await ws.recv())
    print("fixture: hello", hello, flush=True)
    assert hello[0] == 5, f"first frame must be ClientHello, got {hello}"
    auth = decode_client_frame(await ws.recv())
    assert auth[0] == 0, f"second frame must be Hello, got {auth}"
    await ws.send(auth_frame())

    first = decode_client_frame(await ws.recv())
    assert first[0] == 7, f"after Auth must Subscribe, got {first}"
    if second:
        seen_resume = False
        for _ in range(3):
            item = decode_client_frame(await ws.recv())
            if item[0] == 10:
                assert item[1]["conv"] == CONV, item
                assert item[1]["cursor"] == 42, f"resume cursor must be 42: {item}"
                seen_resume = True
                break
        assert seen_resume, "browser did not send Resume(conv, 42)"
        await ws.send(msg_frame(43, "second"))
    else:
        await ws.send(msg_frame(42, "first"))
    await asyncio.sleep(0.15)
    await ws.close()


async def main() -> None:
    done = asyncio.Event()
    count = 0

    async def wrapped(ws) -> None:
        nonlocal count
        count += 1
        await handler(ws, second=count >= 2)
        if count >= 2:
            done.set()

    async with websockets.serve(wrapped, "127.0.0.1", PORT):
        await done.wait()
        await asyncio.sleep(0.2)


if __name__ == "__main__":
    asyncio.run(main())
