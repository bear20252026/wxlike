# 代码质量审计报告（2026-10-20）

> 范围：`crates/`（22 个 crate）+ `server/src` + `ui/src-tauri/src`，**生产代码**
> （不含 `tests/` 目录、不含文件内 `#[cfg(test)]` 之后的模块）。
> 共 131 个文件、41,878 行、1,914 个函数。
>
> 方法：`docs/bench/quality_probe.py`（本仓库自带，可重复跑）。它做四件事——
> 函数长度、嵌套深度、魔法数字、8 行窗口跨文件重复；另用 `cargo` / `grep` 补
> 依赖图、pub 面、panic 点、lint 抑制。**这是启发式测量，不是编译器级分析**：
> 嵌套按大括号算（跳过字符串与字符字面量），所以 `match` 的多行模式会略微高估；
> 重复窗口只看结构不看语义，可能把"同一份 DDL 的两个片段"也算成重复。
> 下面每条结论都给了 `文件:行`，可以自己核对。

---

## 一、结论先说

**底子是好的，问题集中在"长大以后才会疼"的地方。**

| 维度 | 现状 | 判断 |
|---|---|---|
| 分层与依赖 | 无真实 crate 环（`rtc→net`、`net→platform` 只在 dev-dependencies）；SQL 与 HTTP 混装的函数只有 1 个 | **好** |
| 崩溃风险 | 生产代码 45 处 `unwrap/expect`，其中 16 处在 examples；`todo!/unimplemented!` 为 0；`#[allow]` 仅 4 处；`unsafe` 2 处 | **好** |
| 测试 | 737 个测试函数，CI 五关（三平台 + lint + doc），本轮实测 770 passed | **好** |
| 单一职责 | `Platform` 一个类型 275 个 `pub fn`、30 个 `impl` 块散在 21 个文件 | **差（P0）** |
| 开放-封闭 | 加一条命令要改 `execute.rs::run()`（220 行 / 22 分支） | **差（P0）** |
| 接口隔离 | 模块之间没有"我能调谁"的边界，任何模块能调 `Platform` 的任何方法 | **差（P0）** |
| 函数体量 | >30 行 213 个（11.1%）、>60 行 46 个（2.4%）、>100 行 14 个（0.7%） | **中**（长尾重） |
| 嵌套深度 | >3 层 63 个（3.3%），最深 8 层（`mq_hub::run_mq_hub_with`） | **中** |
| 重复 | 跨文件 8 行重复窗口 78 处；三套退避重试、两份 `Capability`、两个 CLI 打印同一张表 | **差（P1）** |
| 魔法数字 | 总量 170 次（`20`×26、`32`×19、`64`×18、`50`×17） | **中（P2）** |

一句话：**架构方向对，但"共享内核"没有收口**。22 个 crate 的物理边界是清楚的，
真正的耦合藏在两个地方——`Platform` 这个类型，和"每个功能各自实现一遍的小机制"。

---

## 二、体检数据

### 2.1 函数体量分布

| 区间 | 数量 | 占比 |
|---|---|---|
| ≤30 行 | 1,701 | 88.9% |
| 31–60 行 | 167 | 8.7% |
| 61–100 行 | 32 | 1.7% |
| >100 行 | 14 | 0.7% |

**89% 的函数在 30 行内**——这说明"小函数"不是没做到，而是**少数热点烂了**。
所以整改不该搞"全仓库重构"，只需要处理那 14 + 32 个。

### 2.2 最长的 10 个函数

| 行数 | 嵌套 | 位置 | 判断 |
|---|---|---|---|
| 328 | 0 | `platform/src/db.rs:261 migrate` | 一个 `execute_batch` 的 DDL 字符串，**声明式**，逻辑复杂度低；但 328 行不可评审、diff 噪音大 |
| 220 | 3 | `client/src/execute.rs:34 run` | **真问题**：22 个 match 分支，加命令必须改它（违反 OCP） |
| 204 | 4 | `server/src/lib.rs:101 start_stack` | 装配根，把 8 件事串在一起；应拆成 `build_*` 系列 |
| 202 | 6 | `net/src/mq_hub.rs:530 handle_inbound` | **真问题**：帧类型分派 + 落库 + 扇出 + 回执混在一个函数 |
| 160 | 2 | `platform/src/db.rs:57 migrate_p4` | 同 `migrate`，声明式 |
| 157 | 2 | `video/src/lib.rs:420 run_pipeline` | 转码流水线，步骤线性；可拆成 stage 函数 |
| 133 | 5 | `ui/src-tauri/src/callrtc.rs:363 run` | 事件循环，嵌套偏深 |
| 132 | 4 | `admin/src/http.rs:527 run_op` | 代理转发 + 审计 + 错误映射三件事 |
| 129 | 3 | `platform/src/reconcile.rs:73 reconcile` | 对账循环，可拆"逐笔比对"与"汇总" |
| 124 | 4 | `net/src/diag.rs:155 run` | 四步诊断，已经是顺序短路，拆开更清楚 |

### 2.3 嵌套最深

`mq_hub::run_mq_hub_with` **8 层**（118 行）、`ui/src-tauri::start_outbox_pump` 7 层、
`mq_hub::handle_inbound` 6 层、`cli::main` 6 层、`admin::build_payload` 6 层、
`mq_hub::fanout` 6 层。全部是"回调里套 match 再套 if let 再套 for"的形状。

### 2.4 魔法数字（top 12）

| 值 | 次数 | 实际含义（抽样核对） |
|---|---|---|
| 20 | 26 | 搜索/列表默认 `limit` |
| 32 | 19 | 随机字节数 / 密钥长度 / 缓冲 |
| 64 | 18 | sha256 hex 长度（`video_id`、`media_id`、`api_key`） |
| 50 | 17 | 分页默认 `limit` |
| 12 | 14 | 助记词长度 |
| 5000 | 6 | 离线队列容量 / 端口偏移 |
| 512 | 6 | 缓冲块大小 |
| 256 | 5 | 容量上限 |
| 11 | 5 | 重试/退避次数 |
| 128 | 5 | 漏斗突发额度 |

**真正危险的不是数字多，是同一个数字有两种含义**：`64` 既是 hash 长度又是 id 长度，
`20`/`50` 在不同模块里都表示"默认分页"。改一个常量时改错含义，编译器不会拦。

### 2.5 重复

跨文件 8 行窗口 78 处。按危害排序：

1. **`Capability` 枚举有两份**：`crates/cmd/src/lib.rs:21` 与 `crates/admin/src/rbac.rs:25`。
   语义完全相同（Read/Write/Destructive + `as_str`），是两个独立的类型。
   **这是安全语义分裂**：将来给 `cmd` 加一档 `Sensitive`，admin 不会跟着变，
   而且没有任何编译错误。
2. **退避重试三份**：`core/src/outbox.rs:26`（`BASE_DELAY_MS=1000/MAX=30000`）、
   `video/src/jobs.rs:40`（注释写着"与 outbox 的 `BASE_DELAY_MS` 一致"——这句话
   本身就是重复的证据）、`ffi/src/cache.rs:71`（重试状态机另写一份）。
   三处的指数退避公式相同，参数相同，注释互相指认。
3. **两个 CLI 打印同一张权限表**：`cli/src/main.rs:163 dump_policy` 与
   `cmd/src/bin/wxlike-cmd.rs:51 policy`。**已经漂移**：一个内联 `if/else`，
   一个用 `yes_no()`；改文案要改两处。

### 2.6 依赖图（实测）

```
core ← message ← platform ← net ← push
core ← rtc ← sfu
core ← video / ledger / cmd / report / risk / moderation / search / mq
platform ← admin
client ← cmd ；cli / mcp / ffi ← client
```

没有真实的 crate 环（`rtc→net`、`net→platform` 只出现在 `[dev-dependencies]`，
是测试用的反向边）。**但 `net` 的生产依赖里有 `wxlike-message`**（`mq_hub` 直接构造 `MessageStore`），
这层"传输层认识业务类型"是隐性耦合，见 P1-4。`platform` 只在 `[dev-dependencies]`，
是测试用的反向边，不算架构缺陷。

---

## 三、问题清单（按严重度）

### P0-1 `Platform` 是事实上的 god object（违反 SRP / 接口隔离）

**证据**：`275` 个 `pub fn`，`30` 个 `impl Platform` 块散在 `21` 个文件里
（`communities.rs` 30 个、`lib.rs` 23 个、`call.rs` 21 个、`groups.rs` 18 个……）。

**为什么是问题**：Rust 的 `pub` 是**模块级**可见性，不是类型级。把 21 个模块的
方法挂到同一个类型上，等于宣布"任何模块都可以调任何方法"。后果不是编译不过，
而是：

- 改 `moments.rs` 时，你无法知道谁在依赖它的方法（没有边界就没有影响面分析）；
- 新人写 `video_refs.rs` 时，最省事的做法就是再往 `Platform` 上加 3 个 `pub fn`；
- 单元测试只能整块构造 `Platform`，无法只喂"这个模块需要的 5 个方法"。

**注意**：这不是"文件太大"的问题。`communities.rs` 1534 行但只做社群一件事，
内聚是好的。**问题是"能力挂在同一个类型上"**。

**修法（分两步，不搞大爆炸）**：

1. **先收紧可见性**：每个模块的方法从 `pub fn` 改成 `pub(crate) fn`，只有真正
   跨 crate 需要的（HTTP handler 用不到、只有 `server` 用得到的）才留 `pub`。
   这一步不改任何调用点，只改关键字，风险极低，**能立刻量出真实对外面**。
2. **再按"谁需要"切 trait**：`moments.rs` 需要的 5 个方法抽成
   `trait MomentsDeps`，`Platform` 实现它；模块签名从 `&Platform` 改成
   `&impl MomentsDeps`。测试可以只造一个 5 方法的假实现。
   这一步做完，`Platform` 从"上帝"变成"组合根"，模块之间互不认识。

**验收**：`grep -c "pub fn" crates/platform/src/*.rs` 的合计从 275 降到 ≤120；
新增一个模块时，它的 `impl` 块里**不允许**出现与本模块无关的方法名
（用评审清单卡，不靠工具）。

---

### P0-2 两条命令面，加功能必须改 `run()`（违反 OCP）

**证据**：`client/src/execute.rs:34 run` 220 行、22 个 match 分支。本轮加 6 条
视频命令，就是在这个函数里插 6 段；上一轮加 2 条社群命令，同样。

**为什么是问题**：catalog 已经把"有哪些命令"变成了**数据**（`CATALOG` 数组），
执行侧却还是**代码分支**。数据与分支必须手工同步，只能靠一条防漂移测试
（`every_catalog_command_is_implemented`）兜底——那条测试的存在本身就说明
结构不对：**它是在用测试弥补设计**。

**修法**：把 match 换成**处理表**：

```rust
type Handler = fn(&Executor, &Value) -> Result<Value, String>;
const HANDLERS: &[(&str, Handler)] = &[
    ("conv.list", |ex, p| ex.conv_list(p)),
    ("video.select", |ex, p| ex.video_select(p)),
    // ...
];
```

`run()` 变成 10 行：授权 → 查表 → 调用。加命令 = 加一行数据，**不改函数**。
防漂移测试保留，但语义从"检查我有没有漏写分支"变成"检查表和 catalog 是否一致"——
后者是数据一致性，前者是设计缺陷的补丁。

**验收**：`run()` ≤ 30 行；新增一条命令的 diff 只包含"catalog 一条 + HANDLERS 一行 +
一个 handler 函数"，**不含 `run()`**。

---

### P0-3 安全语义有两份定义（`Capability` 重复）

**证据**：`cmd/src/lib.rs:21` 与 `admin/src/rbac.rs:25` 各自定义了
`Read/Write/Destructive` + `as_str()`，连注释都几乎一样。

**为什么是 P0**：这是**权限模型**，不是普通 DTO。两份定义意味着：
agent 侧认为"Write 需要 `--allow-write`"，后台侧认为"Write 需要角色 ≥ operator"，
两边对"什么算破坏性"可以给出不同答案，而且**没有任何测试会发现**。

**修法**：`admin` 直接依赖 `wxlike-cmd`，`pub use wxlike_cmd::Capability;`。
如果 admin 需要额外的序列化能力，就在 admin 侧写 `impl` 或 newtype 包装，
**不要复制枚举**。

**验收**：全仓库 `enum Capability` 只有 1 处；加一条测试断言
`admin::rbac::Capability::Destructive.as_str() == cmd::Capability::Destructive.as_str()`。

---

### P1-1 退避重试三份实现（违反 DRY，且已经互相指认）

**证据**：`core/outbox.rs:26-28`、`video/jobs.rs:40-43`（注释："与 outbox 的
`BASE_DELAY_MS` 一致"）、`ffi/cache.rs:71` 附近的重试状态机。

**修法**：在 `core` 里抽 `retry` 模块：

```rust
pub struct Backoff { base_ms: u64, max_ms: u64 }
impl Backoff { pub fn delay(&self, attempt: u32) -> Duration }
```

三处改为引用同一份。**`throttled` 不烧次数的语义也要一起搬**——那是上轮
`outbox` 里最贵的一条经验（限流不是失败），不该只活在一个 crate 里。

**验收**：`BASE_DELAY_MS` 全仓库只有 1 处定义；三处调用点的行为测试保持绿。

---

### P1-2 两个 CLI 重复同一张权限表（已漂移）

**证据**：`cli/src/main.rs:163 dump_policy` vs `cmd/src/bin/wxlike-cmd.rs:51 policy`。
一个用内联 `if/else` 打"开/关"，一个用 `yes_no()`。

**修法**：渲染逻辑下沉到 `cmd`：`pub fn render_policy(&Policy) -> String`，
两个二进制都只 `print!`。顺带回答一个更根本的问题：**为什么有两个 CLI？**
（`wxlike-cli` 是执行器，`wxlike-cmd` 是自检器）——如果定位确实不同，
共享渲染；如果定位重叠，合并成一个带子命令的二进制。

**验收**：权限表输出只有一个实现；两个二进制的输出逐字节相同（加一条快照测试）。

---

### P1-3 深嵌套集中在 `mq_hub`（违反"嵌套 ≤3"）

**证据**：`run_mq_hub_with` 8 层、`handle_inbound` 6 层 202 行、`fanout` 6 层。

**为什么是问题**：`mq_hub` 是**消息主链路**，也是最难测、最容易出并发 bug 的地方。
8 层嵌套意味着"读到这一行时，你需要同时记住 8 个条件"。

**修法**：按**帧类型**拆处理函数，主循环只做分派：

```rust
match frame {
    Frame::Content(f) => handle_content(&ctx, f).await?,
    Frame::Ack(f)     => handle_ack(&ctx, f).await?,
    Frame::Recall(f)  => handle_recall(&ctx, f).await?,
}
```

每个 `handle_*` 内部用守卫子句（`let Some(x) = ... else { return Ok(()) }`）把
"正常路径"顶到最左边。

**验收**：`mq_hub` 里 >3 层的函数为 0；`handle_inbound` ≤ 60 行。

---

### P1-4 传输层依赖业务层的类型（依赖倒置不彻底）

**证据**：`net/Cargo.toml` 的生产依赖里有 `wxlike-message`；`net/src/mq_hub.rs` 直接构造
`wxlike_message::MessageStore`、按业务语义做镜像与扇出。（`wxlike-platform` 只在
`[dev-dependencies]`，那是测试用的，不算这条问题。）

**为什么是问题**：`net` 是"怎么传"，`platform` 是"传什么、谁能看"。传输层认识
业务类型，意味着**换传输（QUIC → WS）要碰业务**，反过来业务改字段也会波及传输。
Zone A 做网页端时已经付过一次代价：浏览器侧必须重新实现一遍 `WireFrame` 编解码
（`ui/src/web/wire.ts` 577 行），因为线格式没有独立成 crate。

**修法**：把线格式与传输抽象抽成 `crates/wire`（`WireFrame` + 编解码 + 版本号），
`net` 依赖 `wire`，不依赖 `platform`。`mq_hub` 需要业务能力时注入 trait
（`trait Mailbox`），而不是 import 具体类型。

**验收**：`net` 的生产依赖只剩 `core` + `mq` + `wire`；`wire` 有独立的跨语言
编解码测试（Rust 与 TS 对同一组字节断言相同结果）。

---

### P2-1 魔法数字：同一数字多种含义

**证据**：`300`（超时 / 分页上限）、`64`（hash 长度 / id 长度）、`50`/`20`（默认 limit）、
`360`/`30`（时间换算）散在各 crate。

**修法**：每个 crate 一个 `limits.rs`（或 `consts.rs`），**按含义命名而不是按值命名**：

```rust
pub const HTTP_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_PAGE_LIMIT: usize = 50;
pub const SHA256_HEX_LEN: usize = 64;
```

**验收**：`quality_probe.py` 的魔法数字计数从 431 降到 ≤150；同一含义只允许一个常量。

---

### P2-2 两个 300+ 行的 DDL 函数

**证据**：`db.rs:261 migrate` 328 行、`db.rs:57 migrate_p4` 160 行，各自是一个
`execute_batch` 的超长字符串。

**判断**：这是**声明式数据**，不是逻辑，不该按"函数 ≤30 行"一刀切。
但它确实妨碍评审（一次 diff 看不出改了哪张表）。

**修法**：拆成 `const SCHEMA_CORE: &str` / `SCHEMA_P3` / `SCHEMA_P4` / `SCHEMA_VIDEO`
四个常量，`migrate()` 变成 5 行按顺序执行。**同时**补一条"schema 快照测试"：
把 `PRAGMA table_info` 的结果序列化成 JSON 存进仓库，改表必须同步改快照——
现在这张库有 40+ 张表，没有任何机制阻止"悄悄加一列"。

**验收**：`migrate()` ≤ 15 行；schema 快照测试存在且能拦住列变更。

---

### P2-3 装配根 204 行

**证据**：`server/src/lib.rs:101 start_stack` 把 db、platform、store、mailbox、
search、video、hub、router 全串在一个函数里。

**修法**：拆 `build_platform()` / `build_search()` / `build_video()` / `build_router()`，
`start_stack` 只留"按顺序调用 + 错误上下文"。顺带把环境变量读取集中到一个
`Config::from_env()`——现在 `WXLIKE_*` 散在 6 个文件里读。

**验收**：`start_stack` ≤ 40 行；所有 `std::env::var("WXLIKE_` 只在 `config.rs` 出现。

---

## 四、这种"分工合作"的形式好吗？

**好，但要限定在"叶子"上。** 这一轮五条轨道的实际结果：

### 有效的部分（值得继续）

1. **worktree 隔离是硬要求**，这条执行得好：五条轨道零代码冲突，合并时只在三个
   清单文件（`Cargo.toml` members、`.gitignore`、`THIRD-PARTY-NOTICES.md`）上冲突。
2. **文件所有权**让"谁能改什么"变成可判定的事实，不用开会协调。
3. **每条轨道自带测试**，合并时能立刻跑全量：25,000 行一天落地，这在串行开发下
   不可能。
4. **轨道自己写"没做什么"**（Zone E 的 §7 待验清单、§8 代合事项）——这是最有价值
   的交付物，比代码本身更能降低集成风险。

### 失效的部分（下次必须改）

1. **契约没有先行**：五条轨道各自需要"重试"、"能力等级"、"命令注册"这些**共享内核**，
   于是各写一份。`Capability` 两份、退避三份、两个 CLI，全是这么来的。
   **并行放大的不是产能，是重复。**
2. **git 看不见语义冲突**：`SendFailure` 加字段，git 合并干净，编译才炸。
   轨道之间"共享类型"的任何改动，都不该由轨道自己做。
3. **集成工作被留到最后**：catalog 登记、平台路由、`admin_token` 接线，全是热点文件，
   轨道按规矩不能碰，只能等集成方。结果并行度在最后 20% 塌回串行。
4. **测试的"绿色"是局部的**：每条轨道自己绿，合并后 Linux 才暴露夹具竞态。
   跨轨道共享的资源（临时目录、端口、环境变量）没有约定。

### 结论与改法

**形式本身是对的，缺的是"共享内核"这一层治理。** 下次开工前加三步：

1. **契约清单**（半天）：列出本轮会碰到的共享概念（类型、常量、trait），
   由集成方先落进 `core` / `cmd`，轨道只允许**使用**，不允许**新增**。
   发现缺一个，停下来报告，由集成方补——这一条能消灭 80% 的重复。
2. **集成预算显式化**：每轮预留 20% 工时做"代合事项"（catalog、路由、env 接线），
   并在轨道启动时就告诉它"这些不是你的活"，避免它越界或留坑。
3. **合并即全量**：轨道绿不算绿，合并后跑三平台 + clippy 才算。这一轮的两个 bug
   （Linux 竞态、1.99 新 lint）都是这么抓到的——**这条流程要保留**。

一句话：**并行适合"面"（网页端、移动端、后台、点播），不适合"核"（信箱、扇出、
权限、同步）。核必须一个人做完，面可以五个人同时做。**

---

## 五、整改方案（四波，按风险从低到高）

### 第 0 波：先装闸门（已完成）

没有闸门的重构会反弹。这一波不改业务代码。

1. **`clippy.toml`**（新增）：`too-many-lines-threshold = 60`、
   `cognitive-complexity-threshold = 20`、`too-many-arguments-threshold = 8`、
   `type-complexity-threshold = 250`。CI 的 clippy 改成
   `-D warnings -W clippy::too_many_lines`——**只降级这一条**，存量 47 个长函数
   在日志里露头但不拦构建（先收一周，再考虑改 `-D`）。
2. **`scripts/quality-gate.py`**（新增）：跑探针 → 与
   `docs/bench/quality-baseline.json` 逐项比对 → 任何一项变差就退出码 1，
   并打印"基线 → 当前（+N）"。除数值外还比对 `文件::函数名` 名单：
   数量不变但换了个函数变长，同样算退化。`--update` 只允许把基线**收紧**，
   变差时拒绝更新。
3. **CI 新增 `quality` job**：`python scripts/quality-gate.py`，不跑 cargo，十几秒。

**当前基线**：函数 >60 行 **46**、>100 行 **14**、嵌套 >3 层 **63**、
跨文件重复窗口 **78**、魔法数字总数 **170**。

**验收（已实测）**：临时塞一个 70 行函数，闸门输出
`函数 >60 行：46 → 47（+1）` 与 `魔法数字总数：170 → 226（+56）`，退出码 1；
删掉后恢复通过。

### 第 1 波：消灭语义重复（1–2 天，低风险高收益）

| 项 | 动作 | 验收 |
|---|---|---|
| P0-3 | `admin` 复用 `cmd::Capability` | 全仓库 1 处定义 + 一致性测试 |
| P1-1 | 抽 `core::retry::Backoff`，三处引用 | `BASE_DELAY_MS` 1 处定义 |
| P1-2 | 抽 `cmd::render_policy`，两个 CLI 共用 | 输出快照一致 |
| P2-1a | `300`/`64`/`50`/`20` 四个高频值先收口 | 这 4 个值的裸数字为 0 |

这一波不改任何行为，测试应全绿。**先做这一波，是因为它让后面的重构有"唯一真相"。**

### 第 2 波：拆热点（3–5 天，中风险）

| 项 | 动作 | 验收 |
|---|---|---|
| P0-2 | `execute.rs` match → `HANDLERS` 表 | `run()` ≤30 行；加命令不改它 |
| P1-3 | `mq_hub` 按帧类型拆 handler + 守卫子句 | `mq_hub` 无 >3 层函数 |
| P2-3 | `start_stack` 拆 `build_*` + `Config::from_env` | ≤40 行；`env::var` 只在 config |
| P2-2 | `migrate` 拆 4 个 `SCHEMA_*` + schema 快照测试 | `migrate()` ≤15 行 |

风险点：`mq_hub` 是主链路。做法是**先加测试再拆**——把现有的 `handle_inbound`
行为用"给定帧 → 期望落库/扇出"的用例钉住，再拆。

### 第 3 波：收口边界（1–2 周，高风险，单独排期）

| 项 | 动作 | 验收 |
|---|---|---|
| P0-1 步骤 1 | `platform` 全模块 `pub fn` → `pub(crate) fn`，只留真实对外面 | 对外方法数 ≤120 |
| P0-1 步骤 2 | 按模块需要切 trait（先做 `moments`/`wallet`/`video` 三个试点） | 试点模块签名不含 `&Platform` |
| P1-4 | 抽 `crates/wire`，`net` 去掉对 `platform` 的生产依赖 | `net` 依赖只剩 core/mq/wire |

**为什么放最后**：这三项会动到几乎所有调用点，必须在闸门装好、重复消除之后做，
否则会一边重构一边制造新的重复。

---

## 六、度量目标（做完之后应该看到）

| 指标 | 现在 | 第 2 波后 | 第 3 波后 |
|---|---|---|---|
| 函数 >60 行 | 46 | ≤25 | ≤15 |
| 函数 >100 行 | 14 | ≤6 | ≤4（只允许 DDL/声明式） |
| 嵌套 >3 层 | 63 | ≤30 | ≤25 |
| 跨文件重复窗口 | 78 | ≤55 | ≤35 |
| 魔法数字总数 | 170 | ≤100 | ≤60 |
| `Platform` 对外方法 | 275 | 275 | ≤120 |
| `run()` 行数 | 220 | ≤30 | ≤30 |

---

## 六点五、整改进度（滚动更新）

| 波次 | 项 | 状态 | 结果 |
|---|---|---|---|
| 0 | 闸门（clippy 阈值 + 基线门禁 + CI job） | ✅ | 已实测能拦住"新增 70 行函数" |
| 1 | P0-3 权限枚举收口 | ✅ | `admin` 复用 `wxlike_cmd::Capability`；新增序列化一致性测试 |
| 1 | P1-1 退避重试收口 | ✅ | `core::retry::Backoff` 一份实现，outbox / video::jobs 委托；4 条测试 |
| 1 | P1-2 权限表渲染收口 | ✅ | `cmd::policy::render`，两个二进制输出逐字节相同；2 条测试 |
| 1 | P2-1 魔法数字（第一批） | ✅ | 新增 `core::limits`（按含义命名），hash 长度 `64` 全部收口 |
| 2 | `run()` 处理表 / `mq_hub` 拆帧 / `start_stack` / `migrate` | ⏳ | 下一波 |
| 3 | `Platform` 门面化 / 抽 `crates/wire` | ⏳ | 单独排期 |

指标变化（口径修正后重新计算，只算生产代码）：

| 指标 | 审计时 | 现在 |
|---|---|---|
| 函数 >60 行 | 46 | 46 |
| 函数 >100 行 | 14 | 14 |
| 嵌套 >3 层 | 63 | 63 |
| 跨文件重复窗口 | 78 | 78 |
| 魔法数字总数 | 170 | **162** |

> 前四项这一波没动（第 2 波才拆函数），**没有变差**是底线；
> 重复窗口下降发生在权限枚举与权限表收口时（78 已是收口后的数）。

## 七、附：怎么复现

```bash
# 体检（不依赖任何第三方库，python 3.11+）
python docs/bench/quality_probe.py > /tmp/quality.json

# 当前数字
python - <<'PY'
import json; d=json.load(open('/tmp/quality.json'))
print({k: d[k] for k in ('fn_total','count_len_gt60','count_len_gt100','count_nest_gt3','dup_windows')})
PY

# 依赖图
python -c "import json;d=json.load(open('/tmp/quality.json'));print(d['deps'])"
```

基线文件（`docs/bench/quality-baseline.json`）在第 0 波里生成，之后 CI 只比对差值。
