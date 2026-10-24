//! `wxlike-client-core`：三端共用的客户端核心。
//!
//! 分工是"核心管逻辑，平台管能力"：
//!
//! ```text
//!   UI (React / Flutter)
//!        │ dispatch(ActionRequest) / subscribe(cursor)
//!        ▼
//!   client-core  ── runtime（会话、outbox 泵、事件扇出）
//!        │ ports
//!        ├── Transport   网络（native: ureq / web: fetch+WS / mobile: 同 native）
//!        ├── LocalStore  本地真相（native: SQLite / web: IndexedDB）
//!        ├── Clock       时间与睡眠（可注入，测试不用真等）
//!        ├── Secrets     密钥与敏感缓存加密材料
//!        ├── Platform    单实例锁、生命周期、能力声明
//!        └── Notification 前台/后台通知
//! ```
//!
//! 为什么是 ports 而不是 `if cfg!(target_arch = "wasm32")`：平台差异一旦写进
//! 核心，就会长成"三份逻辑、三套 bug"。端口把差异压到实现侧，核心只有一份。
//!
//! **wasm 不要求 `Send`**：浏览器的 JS 对象本来就不能跨线程，硬要求 `Send`
//! 只会逼人写一堆 `unsafe impl`。所以这里用 [`ports::MaybeSend`] 做条件约束：
//! 原生构建要 `Send + Sync`，wasm 构建不要求。异步 trait 也按同样方式切换
//! `?Send`（见 `ports::transport`）。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cache_cipher;
pub mod cancel;
pub mod ports;
pub mod runtime;
