//! ADR-28 的 adminx 样板：会碰 SQLite 的同步段整段进 blocking 池。
//!
//! 形状与 `crates/platform/src/db.rs::offload` 逐字对齐：**调用**跨线程边界，
//! 调用里面的事务不跨——闭包在池线程里取锁、跑完、放锁，await 上下文从头到尾
//! 不持有连接锁。adminx 的六套存储（Store / AccountStore / PlatformRead）都是
//! `Arc<Mutex<Connection>>` + 同步 rusqlite，handler 是 `async fn`：不卸载的话，
//! 一条 SQL 就把一个 tokio worker 占住（第四轮审计 N1）。
//!
//! 两条路，按"这段代码有没有 await"选：
//!
//! * [`offload`]：整个同步调用（准入 + 域方法 + 审计）搬进池。域方法保持同步，
//!   测试与调用点一个字不用改。
//! * [`Store::blocking`]（见 `crate::store`）：async 域方法里夹着端口 await 的，
//!   把 await 前后的 DB 临界区各自包进池；审计哈希链的"读尾 + 插入"必须整段
//!   在一个闭包里，不许拆成两次取锁。

use crate::error::{AdminError, Result};

/// 把一段会碰库的同步代码搬进 blocking 池（ADR-28）。
///
/// `JoinError`（池里的任务 panic 或被取消）一律折成 [`AdminError::Admission`]
/// 并留日志：这段代码的结论不可信时，没有任何一条路径应该继续往下走。
/// 闭包里的业务错误原样透传——卸载只改变"在哪条线程上等锁"，不改变判定。
pub(crate) async fn offload<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let ran = tokio::task::spawn_blocking(f).await;
    match ran {
        Ok(inner) => inner,
        Err(err) => {
            tracing::error!(%err, "管理面阻塞任务异常结束（blocking 池 panic 或被取消）");
            Err(AdminError::Admission)
        }
    }
}
