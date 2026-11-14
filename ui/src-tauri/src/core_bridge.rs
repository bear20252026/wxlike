//! G01-02：新 UI（apps/desktop-web，契约 v1）的宿主桥。
//!
//! 前端只发 `ActionRequest`（snake_case JSON）、监听 `core://event`；
//! 这里把 [`IMPLEMENTED_ACTIONS`] 列出的 action_id 映射到 [`AppState`] 的既有业务方法
//! （HTTP 控制面 + QUIC hub + 本地 SQLite 镜像库），**不引入第二套业务
//! 逻辑**——旧 UI 与新 UI 走同一个后端，行为不会分叉。
//!
//! 数据形状以 `apps/desktop-web/src/shell/models.ts` 与 fixture 适配器为
//! 参照（fixture 是新 UI 开发时的参照实现）；本文件里的映射函数负责把
//! 服务端/本地库的事件形状（render.rs 的 payload）转换成 UI 的
//! ChatMessage / Conversation / Contact。
//!
//! 事件：`emit_core` 以 EventEnvelope 形状发 `core://event`，同时把每条信封
//! 记进有界环形日志；收包泵（app_state::start_recv_pump）每条入站消息都会调
//! [`forward_inbound`] 同步转发一份。`core_subscribe` 带的游标即据此补发
//! （见 [`subscribe`] 的诚实边界：同进程能补，跨重启只报 gap）。
//!
//! 未实现的 action 返回 `NOT_IMPLEMENTED` 并注明原因——按"最新 UI 所有
//! 入口都能完成真实操作"的验收口径，宁可明确说没有，也不伪造成功。

include!("core_bridge/types.inc");
include!("core_bridge/support.inc");
include!("core_bridge/dispatch.inc");
include!("core_bridge/identity.inc");
include!("core_bridge/messaging.inc");
include!("core_bridge/media.inc");
include!("core_bridge/relations.inc");
include!("core_bridge/wallet.inc");
include!("core_bridge/search.inc");
include!("core_bridge/prefs.inc");
include!("core_bridge/storage.inc");
include!("core_bridge/tests.inc");
