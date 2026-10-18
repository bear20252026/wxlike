# Third-party notices

> **License change (2026-10-16): wxlike is now GPL-3.0** (was MIT at first commit).
> Project positioning: research/experiment, not commercial (contract §1.4 内部测试模式).
> Per contract §7.2 the standing obligation is **origin recording** in this file;
> the reactivation clause restores full license vetting before any public release.
> **Policy (2026-10-17): prefer existing crates/projects over hand-writing** —
> every new dependency or ported design MUST be added here in the same PR.

## Dependency inventory (cargo crates, transitive deps via Cargo.lock)

| crate | version | license | used for |
|---|---|---|---|
| tokio / axum / tower / hyper | 1.x / 0.8 | MIT | async runtime, HTTP data plane |
| quinn / rustls / rcgen / ring | 0.11 / 0.23 | MIT/Apache-2.0 | QUIC + TLS transport |
| **vodozemac** | latest | Apache-2.0 | E2EE Olm/Megolm ratchet (WP-12; hand-rolled ratchets are banned by design) |
| **str0m** | latest | ISC/Apache-2.0 | WebRTC media (WP-08) |
| rusqlite (bundled) | 0.32 | MIT | SQLite storage (server + client local library) |
| boa_engine | 0.20 | MIT/Apache-2.0 | JS engine in the research container |
| image, zstd, sha2, chacha20poly1305, hkdf, x25519-dalek, pbkdf2, rand, hex, base64 | — | MIT/Apache-2.0 | media/crypto primitives |
| ureq | 2.x | MIT | UI-side HTTP client |
| **tantivy** | 0.22.1 | MIT | P3 full-text search index (ADR-22) — replaces the transitional P1 substring search; ngram tokenizer for CJK |
| **async-nats** | 0.42.0 | Apache-2.0 | P3 realtime channel backend (JetStream durable queues + KV routes) — the official Rust NATS client; no broker hand-written |
| tracing / tracing-subscriber | 0.1/0.3 | MIT | observability |
| serde / serde_json / bincode | 1.x | MIT | serialization |

(Exhaustive transitive list: `Cargo.lock`; only load-bearing crates are
itemized here.)

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

## Ported designs (no code copied yet; origin recorded per contract §7.2)

- **OpenIM** (openimsdk/open-im-server, Apache-2.0): the five-layer sync
  contract (digest → delta → fallback full → batch threshold → lazy member
  paging) is the required protocol shape for the P3 sync-service. Origin must
  stay attached to that module's docs.
- **WuKongIM / goim**: channel subscription model and layered (active-realtime
  / inactive-aggregated) push — reference shapes for P3 gateway/push work.
- **Telegram clients** (GPL-3.0): message-list recycling and weak-network
  patterns for the virtualized list work.

## Borrow-first roadmap (where the wheel must NOT be reinvented)

- P3 search → **Tantivy** ✅ DONE (2026-10-17): crates/search wraps tantivy
  0.22.1; server indexes via the store's append listener into a background
  batch-commit thread (fsync off the gateway path), query side is a read-only
  replica; recall/authorization rulings stay in the authoritative store.
- P3 queue → **NATS** ✅ DONE (2026-10-17): `crates/mq/nats_backend` on
  async-nats 0.42 (JetStream durable pull consumers = at-least-once, ack_wait
  redelivery; KV bucket = route table). Feature `nats`, real-backend test
  ignored like Redis; docker-compose ships a nats:2 service.
- P4 ledger → **cala-ledger** (double-entry core) + **obix** (outbox) +
  **dtmrs** (saga) embedded, per ADR-12.
- P4 auth hardening → `argon2` crate, TOTP crate (RFC 6238) ✅ DONE 2026-10-18.
- P4 governance → **no third-party code**: `wxlike-risk` (sliding window),
  `wxlike-moderation` (normalize + trie), `wxlike-report` (state machine) are
  original. Design shapes only: Hyperswitch's FSM/idempotency discipline
  (Apache-2.0) and the "Lexfence 式独立审核微服务" boundary from ADR-15;
  neither contributed code.
- P4 rooms → `wxlike-rtc::room` is original; str0m remains the media stack.
- P5 mobile → Flutter + UniFFI/cargo-ndk; live ingest → **Xiu** (external
  process); sandbox → **Wasmtime**; mini-app runtime per miniprogram-blueprint.
- P6 admin → **adminx** embedded in Axum; observability → OpenTelemetry/Loki/
  Prometheus (configuration, not code).

## Design-study references (ideas only, no code)

- LiangNiang/fake-world (GPL-3.0): local-first architecture principle and
  AI-generated chat-record concept. License-compatible with wxlike's GPL-3.0; keep per-file attribution when copying.

Everything else in wxlike is original code as of 2026-10-17 (P2 merged);
the inventory above is updated with every borrowing PR.
