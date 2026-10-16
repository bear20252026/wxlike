# Third-party notices

> **License change (2026-10-16): wxlike is now GPL-3.0** (was MIT at first commit).
> Project positioning: research/experiment, not commercial. Consequences:
> - GPL-3.0 ecosystem code (e.g. Telegram official clients) may now be borrowed with attribution.
> - Apache/MIT/MPL code incorporated stays under its original license within this project.
> - AGPL-3.0 code (PaperPhone-plus): AGPL-to-GPL combination is designed to work (GPLv3 sec 13),
>   but treat as case-by-case; keep module-level license headers.
> - Original wxlike code is relicensable at will (sole copyright holder at change time).

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
  AI-generated chat-record concept. License-compatible with wxlike's GPL-3.0; keep per-file attribution when copying.

Everything else in wxlike is original code.
