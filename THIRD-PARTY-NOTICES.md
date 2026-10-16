# Third-party notices

## box-im (bluexsx/box-im) — MIT

The cross-node routing design of `wxlike-mq` and `wxlike-net::mq_hub` is a Rust
reimplementation of box-im's MIT-licensed architecture:

- per-hub message queues (`wxlike:msg:hub:{hub_id}`, mirroring
  box-im's `im:message:private:{serverId}` Redis Lists)
- a user→hub connection routing table (box-im's Redis hash, here
  `wxlike:routes`)
- business/push separation and hub registry semantics

No Java source is included; this is an independent Rust implementation of the
published design. box-im is Copyright its authors, MIT licensed:
https://github.com/bluexsx/box-im

## Design-study references (ideas only, no code)

- LiangNiang/fake-world (GPL-3.0): local-first architecture principle and
  AI-generated chat-record concept. Ideas referenced only — no GPL code may
  enter this repository.

Everything else in wxlike is original code.
