//! wxlike 的共享契约类型：三端（Rust / TS / Dart）与两端（客户端 / 服务端）
//! 都只认这一份定义。
//!
//! 为什么单独一个 crate：UI v2 的 37 页要共用同一批 ID、金额、序列号、错误码
//! 与信封，而它们现在散落在 `crates/core`（`Envelope`）、`crates/client`
//! （`direct_conv_id`）、`crates/ffi`（`WxlikeError`）和 `ui/src/api.ts` 里，
//! 每加一个字段就有四处要改、三处会漏。这个 crate 把它们收成一处，
//! 其余位置改为引用（迁移对照见 `docs/`，旧定义由 G00/G02 在各自卡里删）。
//!
//! 四条硬规则，改代码前先读：
//!
//! 1. **ID 不透明**。`UserId` 之类是私有字段的 newtype，只能 `parse` 出来、
//!    只能 `as_str` 借出去。调用方拿不到内部表示，就没法对它做字符串拼接、
//!    大小写折叠、数值解析这些会破坏不变量的事。
//! 2. **金额是最小单位的整数**。`Money` 里只有 `i64` 分，没有浮点。JSON 上
//!    金额与序列号一律走**字符串**——`2^53` 以上 JavaScript 的 `number` 会
//!    静默丢精度，而 Dart 的 `int` 在 VM 上是 64 位、在 web 上是 double，
//!    只有字符串能同时满足三端。
//! 3. **未知枚举必须有确定行为**。服务端加了新事件类型，旧客户端要能解出
//!    "我不认识它"而不是整体解析失败（见 [`event::EventKind`]）。
//! 4. **错误码是契约**。`DomainError` 的 serde 标签就是三端共用的 code，
//!    改名等于破坏兼容；`error::CODES` 把它们列全，有测试钉住。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod action;
pub mod contract;
pub mod conv;
pub mod cursor;
pub mod deadline;
pub mod error;
pub mod event;
pub mod ids;
pub mod money;
pub mod seq;

pub use action::{ActionOutcome, ActionRequest, ActionResult};
pub use contract::{ContractVersion, SchemaStamp, CONTRACT_VERSION};
pub use conv::{direct_conv, direct_conv_id, parse_conv, ConvKind, ParsedConv};
pub use cursor::{Cursor, CursorKind, CursorValue};
pub use deadline::Deadline;
pub use error::DomainError;
pub use event::{EventEnvelope, EventKind, SubscribeRequest};
pub use ids::{
    ActionId, ConvId, DeviceId, EntryId, EventId, IdError, IdempotencyKey, MessageId, TraceId,
    UserId, MAX_ID_LEN, NO_MESSAGE_ID,
};
pub use money::{Currency, Money, MoneyError};
pub use seq::Seq;
