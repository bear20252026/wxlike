# IM 的 AI 命令面：借 WordCraft 的架构，不借它的形

> 日期：2026-10-19 · 基线：`main @ 14b2a40`
> 结论先说：**四条原则照搬，三处必须偏离，两个坑别踩。**

## 一、核实结果

`github.com/storytold/wordcraft` 真实存在（MIT OR Apache-2.0，纯 Rust，alpha 0.8.0，
1015 commits）。以下事实是我从仓库/文档里读到的，不是转述：

| 事实 | 出处 |
|---|---|
| 422 条命令驱动 Ribbon / 快捷键 / 命令搜索 / CLI / JSON 控制通道 / MCP | README「Built for agents」 |
| 命令 id 形如 `domain.action`（`text.insert`、`para.set`、`table.resize`） | README 示例、`cmd/mod.rs` 注释 |
| 注册表**不是宏**：每个域模块导出 `specs()`，`registry()` 里 `v.extend(text::specs())` 逐个汇总成 `Registry::new(v)` | `crates/engine/src/cmd/mod.rs` |
| MCP 是**独立 crate**（L5），只做协议适配 | README 分层表 |
| 只暴露少数元工具：`list_commands`（可按 query 过滤）/ `execute` / `batch` / `inspect_document` | `docs/mcp.md` |
| 能力边界由**启动参数**注入：`--automation-read-root` / `--automation-write-root`，读写分离 | `docs/mcp.md` |
| 路径检查收敛到一个地方（`io::open_path_with` 等），并有测试禁止命令模块自己碰文件系统 | `docs/mcp.md` |
| 它自己承认的边界：不带路径的命令（`text.insert`）不受 root 约束；桌面应用**无法**被约束 | `docs/mcp.md`「What the roots do not cover」 |

那份文档的诚实度值得学：把"这套约束管不到什么"写在同一页，而不是只写承诺。

## 二、我们现在的命令面（实测）

```
Tauri 命令      67 个   （ui/src-tauri/src/lib.rs）
前端 invoke 封装 28 个   （ui/src/api.ts）
HTTP 路由      150 条   （platform + server）
MCP            0        （全仓 grep 无 mcp）
独立 CLI       0        （只有 server 一个 bin）
```

问题不是"没有命令"，而是**命令长在三处、互不相识**：HTTP handler 一套参数、
Tauri 命令另一套签名、wire protocol 第三种形状。再加一个 MCP，就是第四套。

## 三、照搬的四条

### 1. 一套命令，多个入口

定义 `ImCommand` 目录（catalog），UI / CLI / MCP / HTTP 都只是驱动器。
**不做** `send_message_for_ai` 这种"AI 专用阉割接口"——能力对等是这条原则的全部意义。

### 2. 分层解耦：catalog 与 executor 分开

WordCraft 只有**一个**执行面（本地文档），所以 `engine` 既定义又执行。
IM 有**两个**执行面：本地（本地库、消息泵、E2EE）与远端（HTTP API）。
照抄会错，必须拆成：

```
crates/cmd      (wxlike-cmd)   ← 只定义：id / 参数 / 能力等级 / 帮助文本
                                  不认识 HTTP、不认识 Tauri、不认识 MCP
crates/mcp      (wxlike-mcp)   ← 只做协议：stdio JSON-RPC ↔ catalog + executor trait
apps/cli        (wxlike-cli)   ← 人用：inspect / run --cmd / mcp 子命令
ui/src-tauri                   ← 薄包装：把 Tauri command 转成同一个 executor 调用
```

`wxlike-cmd` 暴露一个 trait，由两个实现填上：

```rust
pub trait Executor {
    fn exec(&self, id: CommandId, params: serde_json::Value) -> Result<serde_json::Value, CmdError>;
}

// 实现 A：ui/src-tauri —— 本地命令走 LocalStore/泵，远端命令走 wxlike-client
// 实现 B：wxlike-cli  —— 只有远端（wxlike-client），本地命令返回 NotAvailable
```

这样 MCP 崩了、CLI 崩了，桌面端照常跑；换协议（MCP → 别的）只重写 `wxlike-mcp`。

### 3. 元工具优先

不要 150 条路由各包一个 `#[tool]`。MCP 面**只要四个**：

| 工具 | 作用 |
|---|---|
| `im_list_commands` | 返回命令清单（id / 一句说明 / 参数 / 能力等级），支持 `query` 过滤 |
| `im_execute` | `{command: "msg.send", params: {...}}` |
| `im_batch` | 顺序执行多条，遇错即停（与 WordCraft 的语义一致） |
| `im_inspect` | 不用截图就能验收：未读、head_seq、成员、最近 N 条、我在哪些会话 |

Agent 的工作流因此是：`list_commands` → `execute` → `inspect` 自查。
工具面稳定，新增功能只扩 catalog。

### 4. 能力边界外置

**权限不写在 handler 里**，由启动 MCP 的父进程注入：

```
wxlike-cli mcp \
  --agent-token ~/.wxlike/agent.token \
  --allow-conv 'd-*~alice,g-abc' \
  --allow-write \
  --allow-destructive        # 默认关闭
```

对应关系：

| WordCraft | wxlike | 为什么不同 |
|---|---|---|
| `--automation-read-root` | `--allow-conv`（读白名单） | 我们的"空间"是会话，不是文件夹 |
| `--automation-write-root` | `--allow-write` + 同一白名单 | 写 = 发消息/建群/改资料 |
| （无对应） | `--allow-destructive` | 踢人/封禁/转让群主/删除，**默认拒绝** |
| （无对应） | `--agent-token` | IM 是多人共享状态，必须有身份与审计 |

## 四、三处必须偏离

### 偏离 1：命令要有"能力等级"，不是所有命令都平等

WordCraft 的 422 条命令都是改一个文档，风险同质。IM 不是：

```rust
pub enum Capability { Read, Write, Destructive }
```

`msg.list` / `conv.unread` = Read；`msg.send` / `conv.ack` = Write；
`group.kick` / `group.transfer` / `msg.recall_other` = Destructive。
**默认只给 Read**，Write 要显式开，Destructive 再开一层。这与项目已有的
"子进程隔离 + 默认拒绝"一致。

### 偏离 2：写命令必须幂等

Agent 会重试。WordCraft 的命令是幂等的（设格式设两次结果一样），IM 的发消息不是。
所以每个 Write 命令接受 `idempotency_key`，服务端映射到 `msg_id`（已有去重机制）。
没有这个，Agent 一次网络抖动就会重复发三条消息。

### 偏离 3：命令要有审计

WordCraft 是单机工具，不需要。IM 是多人产品：谁（哪个 agent）在哪个会话做了什么，
必须可查。执行器在**入口处**统一记一条：

```
actor=agent:<token_id>  command=msg.send  conv=g-abc  result=ok  idem=...
```

写进现有 `audit` 表（P4 已建），不新造机制。

## 五、命令清单（首批 18 条，不是 150）

先做"能读会看"，再开写：

| 域 | 命令 | 能力 |
|---|---|---|
| 会话 | `conv.list` / `conv.unread` / `conv.history` / `conv.sync` | Read |
| 消息 | `msg.list` / `msg.search` / `msg.get` | Read |
| 成员 | `group.members` / `group.info` | Read |
| 自查 | `me.info` / `me.sessions` | Read |
| 写 | `msg.send` / `conv.ack` / `msg.forward` | Write |
| 群管理 | `group.create` / `group.add_member` | Write |
| 破坏性 | `group.kick` / `msg.recall` | Destructive |

`conv.history` 直接复用刚做完的信箱分页（`fetch_before`），`conv.unread` 复用
`MessageStore::unread`——**catalog 不是新实现，是给已有能力起名字**。

## 六、分期与验收

| 期 | 内容 | 验收 |
|---|---|---|
| **P0** | `wxlike-cmd` catalog（18 条，纯数据）+ 快照测试（id 不许悄悄改名） | `cargo test -p wxlike-cmd` 全绿；catalog 快照可 diff |
| **P1** | `wxlike-cli inspect`（只读，直连 HTTP） | 命令能打印未读/历史，无需 UI |
| **P2** | `wxlike-mcp`（stdio，3 元工具 + Read 白名单） | 一个 agent 通过 MCP 读出未读与历史；越界会话被拒且错误可读 |
| **P3** | Write + 幂等键 + 审计；`--allow-destructive` | 重试同 `idempotency_key` 只发一条；踢人被默认拒绝 |

P0/P1 是纯增量，不碰现有 67 个 Tauri 命令；P2 之后桌面端再逐域迁到同一 executor。

## 七、不照搬的部分

| 不搬 | 原因 |
|---|---|
| 422 条命令的规模 | 那是文字处理器的功能面；IM 首屏只需 18 条。命令多了 AI 反而选不准 |
| 基于路径的 root 模型 | 我们的边界是会话 + 用户，不是文件夹；硬套会让"能否读某个群"变成路径判断 |
| `screenshot` / `click` / `key` | 桌面端不需要 agent 点按钮——命令面已经覆盖；加截图等于开了绕过权限的口子 |
| `render_page` 类工具 | 没有对应物；IM 的"验收"是 `im_inspect` 的结构化状态 |
| 每个域一个 `specs()` 模块 | 方向对，但 18 条命令用一个文件更清楚；超过 ~60 条再拆域 |

## 八、一句话

**WordCraft 真正的可迁移资产不是"422 条命令"，是"命令是一等公民、入口是可替换驱动器"这个结构。**
我们照搬结构，把命令面控制在 18 条，把权限做成启动参数，把审计和幂等补上——
这三样是他们不需要、我们必须有的。

## 九、已落地：`wxlike-cmd` 与权限控制中心（P0）

`crates/cmd`（`wxlike-cmd`）已经能跑：

```
$ cargo run -p wxlike-cmd --bin wxlike-cmd -- list
$ WXLIKE_AGENT_ALLOW_CONV='d-*~alice,g-abc' WXLIKE_AGENT_ALLOW_WRITE=1     cargo run -p wxlike-cmd --bin wxlike-cmd -- policy
```

`policy` 会逐条打印"这条命令在当前策略下能不能执行，不能的话为什么"：

```
权限控制中心 · 当前生效策略
  会话白名单   d-*~alice, g-abc
  写权限       开
  破坏性权限   关

group.kick           destructive 拒绝 — group.kick is destructive and destructive
                                    access was not granted (add --allow-destructive)

可执行 16 / 18 条
```

### 中心做什么（`policy.rs`，14 项测试）

| 判断 | 规则 |
|---|---|
| 命令存在吗 | 先查 catalog，再谈权限——避免用错误消息探测命令表 |
| 能力够吗 | `Read` 永远可以；`Write` 要 `--allow-write`；`Destructive` 要 `--allow-destructive` |
| 会话在名单里吗 | 支持 `*` 通配；**空名单 = 一个都不许**（与 WordCraft 不给 root 同理） |
| 转发查几边 | `msg.forward` 的 `from_conv` 与 `to_conv` **都要**在名单里，否则能把白名单内容转出去 |
| 配置矛盾怎么办 | 只开破坏性不开写 → 按"写也没开"拒绝，不出现"能踢人不能发言"的怪状态 |
| 拒绝怎么说 | 带具体会话 id + 该开哪个开关，agent 读到能自己纠正 |

### 三条刻意选择

1. **不做正则**：通配只支持 `*`，避免灾难性回溯，也让人能预测。
2. **不写在 handler 里**：`authorize()` 是唯一入口，命令实现不碰权限。
3. **不做 AI 专用命令**：`msg.send` 与人类发消息是同一条命令、同一套参数。

## 十、P1 落地：`wxlike-cli`（第一次真的执行）

```
$ WXLIKE_AGENT_ALLOW_CONV='*' WXLIKE_TOKEN=$T wxlike-cli --server http://127.0.0.1:8099 inspect
{
  "conv.list": [{"conv":"540a2aaaf0153369","kind":"group","title":"team","members":1,"unread":0}],
  "me.info":   {"user_id":"d909f4ddae66e4a7","name":"alice", ...}
}

$ wxlike-cli ... run --cmd 'conv.sync={"conv":"540a2aaaf0153369","version":0}'
{"directive": "up_to_date"}
```

**权限先于网络**：白名单为空时 `conv.unread` 直接返回
`conversation d-x~y is not allowed ... (grant it with --allow-conv)`，
**不会发出任何请求**（有测试：把 server 指到 `127.0.0.1:1`，错误里不能出现 connect）。

已实现（只读）：`me.info` / `conv.list`（群 + 直聊，附未读）/ `conv.unread` /
`conv.history` / `conv.sync` / `msg.search` / `group.info` / `group.members`。
写与破坏性命令明确报"尚未在 CLI 实现"，不静默成功。

顺带补了一个缺口：平台原本**没有** `GET /api/groups/{id}/members`
（只有 POST 拉人），`group.members` 无从读起。现在加了，且只对群成员开放——
非成员拿 403 而不是空数组，空数组会泄露"群存在但你没在里面"。
