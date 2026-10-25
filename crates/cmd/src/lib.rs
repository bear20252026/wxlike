//! IM 的命令目录（catalog）：**只定义，不执行**。
//!
//! 结构借自 WordCraft（`storytold/wordcraft`）：一条命令一个 id、一处定义、
//! 多个入口（UI / CLI / MCP / HTTP）共用。差异在于我们把"定义"和"执行"拆开——
//! IM 有两个执行面（本地客户端、远端 HTTP），catalog 不该知道任何一边。
//!
//! 三件事在这里定死，别处不许再判断：
//! 1. **命令有哪些**（[`catalog::CATALOG`]）——新增功能先加一条 spec；
//! 2. **每条命令多危险**（[`Capability`]）——读 / 写 / 破坏性；
//! 3. **谁能做什么**（[`policy::Policy`]）——由启动参数注入，不写在 handler 里。
//!
//! 命令 id 用 `域.动作`（`msg.send`、`group.kick`），与 WordCraft 的
//! `text.insert` / `table.resize` 同一套命名直觉。
//!
//! 模块划分（G15-01 起）：
//! * [`catalog`]：命令表 + 参数声明 + 生成的 JSON Schema / CLI 帮助 / MCP 工具定义；
//! * [`policy`]：授权判定（会话白名单 + 能力开关）；
//! * [`auth`]：会话/audience/scope 校验与代理衰减（G15-02）；
//! * [`domains`]：业务域覆盖表（命令 ↔ 入口 ↔ 例外，G15-03）；
//! * [`contract`]：contract-v1 适配（ActionRequest/ActionResult/DomainError）；
//! * [`confirmation`]：危险动作的 prepare → token → execute（G15-04）；
//! * [`jobs`]：长任务的进度 / 取消 / 输出脱敏（G15-04）。
//!
//! **不重复声明**：`Capability` / `CommandSpec` / `CATALOG` / `get` / `search`
//! 都从 [`catalog`] 再导出，外部（`wxlike-client`、`wxlike-cli`、`wxlike-mcp`、
//! `wxlike-admin`）继续按原路径使用，不需要改一行。

pub mod auth;
pub mod catalog;
pub mod confirmation;
pub mod contract;
pub mod domains;
pub mod jobs;
pub mod policy;

pub use catalog::{
    get, search, Capability, CommandSpec, Confirmation, HostCapability, Idempotency, Param,
    ParamType, Platforms, ScopeClass, CATALOG,
};
