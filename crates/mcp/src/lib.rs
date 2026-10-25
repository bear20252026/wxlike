//! `wxlike-mcp` 的库部分：协议核心与传输层。
//!
//! 拆出库是为了让**契约 fixture 能在进程内直接回放**（`tests/` 只能访问
//! 库，不能访问 bin）。远程 transport 落地后，同一批 fixture 会跑在
//! "同引擎、不同搬运方式"两条路径上——协议语义只有一份，才有资格谈兼容。

pub mod server;
pub mod transport;
