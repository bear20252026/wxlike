# Third-party notices

> **License change (2026-10-18): wxlike is now AGPL-3.0** (was GPL-3.0 from 2026-10-16,
> MIT at first commit). AGPL-3.0 absorbs GPL-3.0 code (GPLv3→AGPLv3 is the compatible
> direction), so tweb's GPL-3.0 files stay as-is; AGPL-3.0 upstreams (Element, Synapse,
> spreed, Signal) can now be copied directly, with the combined work remaining AGPL-3.0.
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
| rusqlite / rand / hmac / ureq | 0.32 / 0.8 / 0.12 / 2.12 | MIT/Apache-2.0 | **已在 Cargo.lock 里**：`crates/video` 新增对这四个既有 crate 的依赖边（播放统计与作业队列的库、缺省 session_id、推流凭据 HMAC、出站回调与接入节点推片）。Cargo.lock 只 +4 行，无新包 |
| **tower-http** | 0.7 (`fs`, `set-header`) | MIT | A2 静态托管：`ServeDir`/`ServeFile` 发 `ui/dist-web`，并给壳/资源分别打 `no-store` / `immutable`（Zone A 请求，用户批准新增） |
| **uniffi** | 0.32.2 + `scaffolding-ffi-buffer-fns` | MPL-2.0 | Zone B 的绑定生成器（`crates/ffi`，ADR-11）。Rust 侧只贡献 `#[derive(uniffi::*)]` 元数据；生成的 Kotlin 与 Dart 都不进 git（`crates/ffi/generated/`、`apps/mobile/lib/src/rust/`），由 `crates/ffi/regenerate.sh` 重建。 |
| **uniffi-bindgen-dart** | 0.2.0 | MIT | [nchapman/uniffi-bindgen-dart](https://github.com/nchapman/uniffi-bindgen-dart) · 第三方 Dart 后端。UniFFI 官方只有 Kotlin/Swift/Python/Ruby，Flutter 侧必须有这一个；它依赖 `uniffi_bindgen 0.32.2`（与主 crate 同版本，不是 fork）。**只在生成期用**（`crates/ffi` 的 `dart` feature，`cargo build -p wxlike-ffi` 默认不带），不进手机上的 .so。产物 `apps/mobile/lib/src/rust/wxlike_ffi.dart` 是生成代码，重新生成走 `crates/ffi/regenerate.sh`。 |
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

## Reference priority (Tier 0, user directive 2026-10-18)

任何功能开工前先查这 8 个仓库；优先级 **Tier 0 > Telegram 系 > 其余 41 仓库**。
明细与"按缺口查表"见 [docs/reference-mirror.md](docs/reference-mirror.md)。

| 项目 | 许可（实测） | 主要用途 |
|---|---|---|
| Zulip | Apache-2.0 | 主题化线程模型、消息编辑历史 |
| Rocket.Chat | MIT（`ee/` 除外） | 权限矩阵、REST/Realtime API 形状 |
| Mattermost | 源码 AGPL-3.0（webapp Apache-2.0） | adminx、插件面、团队/频道 |
| Signal-Desktop / Signal-Android | AGPL-3.0 | 发送队列、附件分片、媒体压缩、通知聚合 |
| Element Web | AGPL-3.0 | 设备验证 UI、房间列表虚拟化 |
| Synapse | AGPL-3.0 | 事件 DAG、同步模型对照 |
| Nextcloud Talk (spreed) | AGPL-3.0 | 群通话房间 UI 与管控（**已浅克隆 `refs/spreed`**；`force-mute`/举手/锁门/录制的控制集与我们的 `rtc::room` + `/api/call/*` 对齐，未复制代码） |

> **2026-10-18 起本项目自身为 AGPL-3.0**：AGPL-3.0 上游代码可直接复制，合并后作品
> 仍为 AGPL-3.0（与我们的许可一致，不再有"传染"冲突）；MIT/Apache 依旧可直接复制。
> 唯一新增义务：本平台对外提供网络服务时须向用户提供源码（AGPL §13）——内部测试
> 阶段不触发。

## Vendored code (actual copies; origin recorded per contract §7.2)

| file | upstream | license | what was taken / adapted |
|---|---|---|---|
| `ui/src/vendor/tweb/slicedArray.ts` | [morethanwords/tweb](https://github.com/morethanwords/tweb) · `src/helpers/slicedArray.ts` | **GPL-3.0** | 降序分块数组（`SlicedArray` + `Slice` 的 ends 追踪、`insertSlice` 合并）。适配三处，均写在文件头：去掉 `MOUNT_CLASS_TO` 调试挂载、内联 `indexOfAndSplice` 与 number-only `compareValue`、按本项目 `strict` 补变量初始化。用于 `ui/src/msgIndex.ts`（历史分页合并 + 实时消息插入）。 |
| `ui/src/vendor/tweb/listenerSetter.ts` | 同上 · `src/helpers/listenerSetter.ts` | **GPL-3.0** | 事件监听器集中登记与清理（`add`/`remove`/`removeManual`/`addCleanup`/`removeAll`）。适配：去掉 tweb 内部类型 `EventListenerBase` 的 import 与 union 成员；`strict` 下补 `let listener: Listener \| undefined`。用于消息菜单/贴纸面板的"点外面关闭"——原来两处各写一遍自清监听器，切视图关菜单时会漏。 |
| `crates/rtc/src/sfu.rs` | [8xFF/atm0s-media-server](https://github.com/8xFF/atm0s-media-server) · `packages/media_core/src/endpoint/internal/bitrate_allocator/egress.rs` | **MIT** | 出口码率分配算法（按权重比例分、BWE 目标 6/5、无轨道兜底）。**跨库重写**：类型换成我们的 `TrackId=u32`，去掉 `log`/`indexmap`/`sans-io-runtime` 依赖，`Vec` 保持插入序。前 3 个单测从上游等价移植。 |
| `ui/scripts/check-msgindex.sh` | 本项目原创 | GPL-3.0 | 该 vendored 文件的回归脚本（esbuild 打包 + node 断言 7 条）。 |

> 这是本仓库第一次**实际复制**第三方源码。选择 tweb 而非自研的原因：它的
> 切片合并算法（跨页去重、乱序插入、ends 语义）是消息列表虚拟化的核心，
> 自写一遍等于重犯它已经踩过的边界。GPL-3.0 与本项目许可一致。

## Ported designs (no code copied yet; origin recorded per contract §7.2)

- **Rocket.Chat** (RocketChat/Rocket.Chat, MIT — `ee/` excluded and not consulted):
  the **channel-role** layer of `crates/platform/src/communities.rs` (Zone C · C2)
  follows its `app/authorization` shape — roles own permission *strings*, an
  endpoint asks `can(user, 'post-media')` and never inspects which role a user
  holds. Names are kept comparable to upstream (`post-message`, `post-announce`,
  `delete-any-message`, `manage-roles`) and the four channel tiers
  (`owner`/`moderator`/`member`/`guest`) are theirs. No source file was copied:
  the repo is not mirrored locally (size), and the judgement logic lives in our
  own pure functions (`channel_role_perms`, `perm_for_type`) on our `Perm` enum.
- **Zulip** (zulip/zulip, Apache-2.0): "topic is a first-class attribute of a
  message" — the reason thread replies keep their own sequence and never enter
  the channel timeline (invariant 3). Concept only, no code.
- **本仓库 `crates/core/src/outbox.rs`** (Zone E): 转码作业队列的状态机/退避/幂等入队/
  `throttled 不烧次数` → `crates/video/src/jobs.rs`（同仓库参照重写，两处偏离写在模块头）。
- **本仓库 `crates/platform/src/media.rs`** (Zone E): `Range` 解析与 206/416/Accept-Ranges
  语义 → `crates/video/src/range.rs`。日后应统一到 `crates/core`，删掉其中一份。
- **本仓库 `crates/platform/src/live.rs`** (Zone E): 自包含推流凭据
  `<exp>.<hmac(stream_key|exp)>` 与 `constant_time_eq` → `crates/video/src/live.rs`
  （差别：打包器侧**不查库**）。
- **本仓库 `crates/ledger`** (Zone E): append-only 事件 + 重放聚合 → `crates/video/src/stats.rs`。

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

Everything else in wxlike is original code as of 2026-10-18 (P4 + P5 first
increment merged); the inventory above is updated with every borrowing PR.
