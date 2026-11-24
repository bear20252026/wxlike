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
   `--diff` 模式（CI 日常，`--slack 5`）只扫 PR diff 涉及的文件，并与**这些
   文件的 base 版本**比较（同文件集，用临时 worktree 检出 base）。比较对象
   不能是全量基线——「全仓库 vs diff 文件」的 delta 结构上恒为 0，门禁会
   形同虚设。探针的 `--files <listfile>` 让同一份列表能分别喂给 base 与
   HEAD。
3. **CI 新增 `quality` job**：`python scripts/quality-gate.py --diff --slack 5`，不跑 cargo，十几秒。

**当前基线（2026-11-24 第六轮收紧）**：函数 >60 行 **0**、>100 行 **0**、嵌套 >3 层
**0**、跨文件重复窗口 **127**、魔法数字总数 **172**。重置时是 `>60` 43 / 嵌套 69
（扫描面 472 个文件），本轮拆完长函数后又用 `quality-gate.py --update` 收紧到
当前值（先后把嵌套从 4 收到 0、`>60` 从 38 收到 14、再收到 0）。旧基线（扫描面 134 个文件、`>60` 41 / 嵌套 63 / 重复 66 / 魔法 162）已归档到
`docs/bench/quality-baseline-2026-10-25.json`：扫描面从 134 扩到 472，绝对计数
不能再和旧基线相减，否则 `--update` 永远拒绝收紧。重置只把当前值写成新起点，
之后任何指标上涨仍会被全量闸门拦下。

**验收（已实测）**：临时塞一个 70 行函数，闸门输出
`函数 >60 行：39 → 40（+1）` 与 `魔法数字总数：172 → 228（+56）`，退出码 1；
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
| 嵌套 >3 层 | 63 | ≤30 | ≤25 || 跨文件重复窗口 | 78 | ≤55 | ≤35 |
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
| 2 | `run()` 处理表 | ✅ | 220 行 → 15 行；顺手修掉两条绕过审计标记的命令 |
| 2 | `migrate` / `migrate_p4` | ✅ | DDL 抽成 `SCHEMA` / `SCHEMA_P4`；新增表结构快照测试 |
| 2 | `start_stack` | ✅ | 204 行 → 55 行，拆成 5 个 `build_*` |
| 2 | 错误契约收口（`crates/api`） | ✅ | 新增零业务依赖的 `wxlike-api`；`video` 的 30 处 `(StatusCode, String)` → `ApiResult`，顺带修掉 Windows 上"视频不存在返回 500" |
| 2 | `mq_hub` 拆帧 | ⏳ | 主链路，需先补帧级测试 |
| 3 | `Platform` 门面化 | 🚧 | 五批共降 168 个方法（21+43+48+39+17），公开方法 258→**90**；`AccountSessions` 扩到会话列表/撤销，新增 `AdminAccess` / `ConversationAccess` / `GroupMembership` 三个访问 trait，本批再把投递策略/设备目录/免打扰的七个判定切进 `SocialGraph` / `GroupModeration` / `GovernanceGate` / `AccountDevices` / `MessagingPrefs`；剩余门面按 `docs/platform-facades.md` 迁移 |
| 3 | 抽 `crates/wire` | ⏳ | 单独排期 |

指标变化（口径修正后重新计算，只算生产代码）：

| 指标 | 审计时 | 现在 |
|---|---|---|
| 函数 >60 行 | 46 | **0** |
| 函数 >100 行 | 14 | **0** |
| 嵌套 >3 层 | 63 | **0** |
| 跨文件重复窗口 | 78 | 127（扫描面扩到 472 文件） |
| 魔法数字总数 | 170 | 172（同上） |

> 前四项这一波没动（第 2 波才拆函数），**没有变差**是底线；
> 重复窗口下降发生在权限枚举与权限表收口时（78 已是收口后的数）。

2026-11-24 全仓生产代码复测（`docs/bench/quality_probe.py`）：`>30 行 596`、
`>60 行 0`、`>100 行 0`、`深嵌套 0`、`重复窗口 127`、`魔法数字 172`。统计面随仓库模块
增加而扩大，不能直接和上表的历史口径相减；本轮已拆掉 Platform 最后三个
`>60` 行函数（`claim_redpacket` 75、`send_danmaku` 73、`login` 72），Platform
侧 `long_fns` 归零；随后把 `message/src/lib.rs::open` 的 210 行建表 DDL
抽成 `SCHEMA_SQL` 常量，`open` 由 233 行降到约 20 行；`net/src/diag.rs::run`
的 DNS/TCP 两段收进 `dns_probe` / `tcp_probe`，124→70 行；
`rtc/src/turn.rs::allocate` 的地址解析与 401 挑战重发收进 `resolve_server` /
`allocate_relayed`，103→约 25 行；`video/src/lib.rs::run_pipeline` 的逐档编码、
master 写出、缩略图收进 `encode_renditions` / `encode_one` /
`publish_playlists` / `make_thumbnail`，157→约 45 行。`net/src/mq_hub.rs` 的
`run_mq_hub_with`（204 行 / 9 层）与 `serve_transport`（306 行 / 6 层）已拆成
状态装配、accept/consume/patrol/续租/清理四类后台任务、握手（`accept_hello` /
`authenticate_hello` / `register_connection`）、控制帧（订阅 / stored-ack /
resume）与入站泵；两者都降到约 30 行。`handle_inbound`（316 行 / 6 层）再按
发送者校验 → 成员/直聊/广播形状 → send gate/关系策略 → 回执/撤回 → 内容
持久化/重放/扇出拆开，主函数降到约 40 行。全仓 `>100 行` 的最后一个
`ui/src-tauri::callrtc::run` 也已拆开：133 行降到 97 行，UI 命令处理、
信令轮询、状态广播与收尾分别收进 `handle_command` / `poll_signals` /
`publish_status` / `finish_status`，全仓 `>100 行` 归零。

同一波继续压深嵌套：`ui::app_state::start_outbox_pump` 的重发轮次收进
`flush_outbox_task` / `persist_local_echo`，`start_recv_pump` 的收包循环整段搬到
`recv_pump_loop`（代次判断收成 `pump_generation_is_current`）；`admin::http::build_payload`
的 action+note 分支收进 `build_action_payload`；`net::mq_hub::fanout_content` 的收件人
解析、单收件人投递、成功记账收进 `pending_recipients` / `spawn_delivery` /
`mark_content_delivered`；`rtc::call::check_deadlines` 用 `let Some(..) else` 摊平两层
可选时间戳；`rtc::turn::maybe_refresh` 的收发与 438 nonce 处理收进 `exchange` /
`absorb_refresh`。`admin::http::build_action_payload` 的 action 归一又收进
`normalize_action` / `tag_action` / `object_action` / `attach_duration`，
主函数只剩"归一 → 补 note"。`video::jobs::submit_job` 的回调白名单、已可播补行、
源字节落盘、作业行回读分别收进 `resolve_callback` / `record_ready_job` /
`write_pending_source` / `submit_reply`，90→约 35 行。`ui::render::render_event_with`
把信封正文分支收进 `render_body`、秘密聊天解密收进 `render_e2ee`，75→约 30 行。
`adminx` 二进制的 `main` 把日志初始化、无配置子命令、`--check` 自检分别收进
`init_tracing` / `early_command` / `check_config`，68→约 20 行。`start_outbox_pump`
的线程主体整段搬进 `run_outbox_pump`，外层只留"取队列 + 单次启动 + spawn"。
`ffi::client::send_now` 的成功/失败落地收进 `complete_sent` / `complete_failure`，
主函数只剩一次 `try_send` 分派。深嵌套从 73 降到 68，`>60` 从 49 降到 43。

2026-11-24 继续压：`rtc::audio_io` 的四个采集/播放回调把重采样投递与队列填充收进
`send_mono_pcm` / `fill_playback_f32` / `fill_playback_i16`，i16 多声道平均抽到
`dsp::interleaved_i16_to_mono`；`rtc::call::check_deadlines` 的响铃与静默看门狗
各自收进 `ring_timeout` / `silence_timeout`；`rtc::host::pump_media` 的媒体事件
改成一次 `filter_map`；`rtc::ice::parse_xor_mapped_address` 的地址解码抽到
`xor_mapped_address`。`client::Http::json` 的发送与解析收进 `send_request` /
`decode_json_body`；`video::jobs::read_job_fields` 的 bytes/callback 字段各抽一个
异步读取函数；`net::mq_hub::persist_content` 的归档与信箱镜像收进
`archive_content` / `mirror_content`；`admin::auth::grants` 的三重循环收进
`push_resource_grants`，`admin::http::run_op_prepare` 的三类拒绝各抽一个留痕函数，
`admin::publishing::decide` 的线上生效段搬进 `apply_online`；
`message::delivery::claim_delivery` 的行映射/选任务/租约更新拆成三个自由函数；
`net` 的 chat 示例把收包线程、特殊命令、出站信封各抽一层；
`ui::callrtc::run` 的副作用落地整段搬进 `apply_effect`，
`ui::app_state::recv_pump_loop` 的取包与断流上报收进 `next_envelope_or_notify`。
深嵌套从 68 降到 54，`>60` 从 43 降到 39，`>30` 从 618 降到 614。

同一轮再把剩下 14 个深嵌套热点摊平：`server::dev_routes` 的水龙头开关与请求体校验
收进 `admin_token_configured` / `faucet_request` / `admin_token_of` / `faucet_fund`，
`turn_routes` 的签发闭包搬进 `turn_credentials`；`net::mq_hub::apply_delivery_policy`
按群帧/广播/直发三种形状拆成 `group_frame` / `direct_frame` /
`permitted_recipients` / `recipient_allowed`，`dispatch_control` 的限流收进
`admit_content`，`accept_hello` 的首帧尺寸检查、协议协商、凭证读取各抽一层，
`fanout` 的收件人去重、单收件人任务、首错收敛拆成 `dedup_recipients` /
`spawn_fanout_delivery` / `first_task_error`；`net::server::serve_conn` 把取帧、
分配连接号、广播收进 `next_relay_frame` / `next_peer_id` / `relay_frame`；
`net::offline` 的打开与建表拆成 `open_connection` / `init_schema` /
`migrate_claimed_until`；`platform::moments::save_moment_draft` 的覆盖/新建收进
`write_draft` + `DraftFields`；`platform::recovery::risk_gate_many` 的取最严裁决与
裁决翻译收进 `worst_verdict` / `stricter` / `verdict_result`；`server::commerce_mount`
的鉴权、错误体、解析、阻塞分发收进 `error_response` / `parse_payload` / `run_action`；
`dpage` 二进制按子命令拆成 `pack_command` / `unpack_command` / `info_command` /
`write_page_file`；`video` 的 live ingest `send` 拆出 `push_once` + `PushFailure`；
`core::e2ee::drain_parked` 的到点查找与解密收进 `next_due` / `deliver_parked`；
`core::outbox::flush_with` 的单条记账收进 `deliver_one`；`search::rank_trends_paged`
的窗口交集与分页收进 `effective_window` / `page_slice`；`push::aggregator::spawn`
的冲刷循环搬进 `flush_loop` / `flush_once`；`server::serve_http` 的服务循环搬进
`serve_http_loop`；`mcp::server::start` 的会话拉起搬进 `spawn_session`；
`ffi::client::conversations` 的远端/缓存两条路收进 `conversations_from_remote` /
`conversations_from_cache` + `conversation_row`，`spawn_pump` 的循环拆出
`pump_loop` / `pump_once`；`video::lib::transcode` 的目录落位收进 `finish_transcode`；
`relations::port` 的 ACS 解析按字母取位收进 `bit_of`，delta 扫描收进
`delta_segments` / `apply_segment`；`commerce::settlement` 的批量重试收进
`processing_withdrawals` / `refresh_advanced`。
深嵌套从 54 降到 28，`>60` 从 39 降到 38，`>30` 从 614 降到 596。

同一轮第三批把嵌套继续压到 4：`search::IndexerHandle::spawn` 的攒批/落盘搬到
`index_loop` / `collect_batch` / `stage_batch`；`net::mq_hub::spawn_accept_loop`
的 accept 循环搬进 `accept_loop`，每条连接的任务收进 `spawn_conn_task`；
`consume_memory` 的一轮消费收进 `consume_memory_once`；`spawn_delivery_prune`
的清理循环搬进 `prune_deliveries_loop`；`net::server::run_hub` 的接入循环搬进
`accept_loop`；`miniapps::packages::install::recover` 的残留清理收进
`clear_staging` / `clear_incomplete_versions`；`admin` 二进制的 `check_config`
把表状态与账号读回拆成 `report_table_state` / `report_accounts`；
`cmd::policy::authorize` 的会话白名单校验收进 `check_convs`；`admin::read::open`
的只读回退收进 `open_query_only`；`server::commerce_mount::spawn_sweep` 的单轮
收进 `run_sweep`；`ledger` / `message` / `moderation` 的建库流程各拆成
`open_raw` / `apply_pragmas`、`open_connection` + `init_schema`、`insert_term` /
`child_or_insert`；`admin::audit::sanitize` 的对象分支收进 `sanitize_map`；
`media::processing::spawn_job_worker` 的一轮收进 `worker_round`；
`risk::map_verdict` 的拒绝/限流翻译收进 `denied` / `throttled`；
`platform::storage::open_sqlite` 的父目录创建收进 `ensure_parent_dir`；
`rtc::host::drain` 的单条动作收进 `drain_one`；`ui::app_state` 的断流上报收进
`notify_disconnected`；`cli::args::parse` 的扫描循环收进 `scan`，Help 组装收进
`help_invocation`；`mcp::server::execute_tool` 的 properties 表先建好再拼；
`net` chat 示例的取包收进 `next_or_log`；`ui::callrtc::pump_mic` 的写帧收进
`write_frame`（先把编解码器 `take` 出来，避免与 `sent` 的借用冲突）；
`ui::render::render_event_with` 的外层字段与回执体收进 `base_payload` /
`receipt_payload`；`platform::spawn_redpacket_sweeper` 的循环收进
`redpacket_sweep_loop`。
深嵌套从 28 降到 4，其余指标不变（`>60` 38、`>30` 596、重复 128、魔法 172）。
剩下 4 个仍是 4 层的是 `message::open`、`video::jobs::spawn_worker`、
`mcp::list_commands_tool` 与 `container` 示例里的宏展开。

同一轮第四批专攻长函数：按探针 `long_fns` 从大到小拆掉 20 个 `>60` 行热点，
全部只抽命名步骤、行为不变。`identity::governance::user` 的 `suspend` /
`unsuspend` / `set_disabled` 共用 `Prep` + `prepare` / `state_of` / `close_tx`，
三个函数从 87/78/83 降到 40 上下，且 `unsuspend` 复用 `finish_govern` 后事件
形状与镜像和另两条路径完全一致；`admin` 二进制 `account_command` 拆出
`list_accounts` / `run_account_action` / `mfa_label` / `yes_no`；`admin::read::stats`
把 12 项计数收进 `Gauge` / `gauge_counts` / `audit_in_sample` / `last_reconcile_run`；
`admin::publishing::submit` 拆 `find_by_idem` / `new_item` / `insert_item`；
`relations::group::grant_member` 拆 `check_approver` / `next_acs` / `check_ceiling` /
`transfer_ownership`；`identity::login_challenge::verify` 拆 `fetch_challenge` /
`check_open` / `consume_challenge`；`identity::governance::device::revoke_device`
拆 `revoke_events` / `revoke_draft`；`search::search` 拆 `boolean_query` / `hit_of` /
`text_of`；`server::forward_message` 拆 `check_forward_access` / `build_forward`；
`server::start_stack` 的六段装配收进 `assemble_apps`；`video::stats::record_watch`
拆 `watch_fields` / `insert_watch`；`video::encode_one` 拆 `encode_mp4` / `encode_hls` /
`bandwidth_of`；`container::engine::install_bridge` 的五个内嵌函数提到模块级；
`admin::g14::router` 按域拆五段 `.merge()`；`net::diag::run` 拆
`run_dns` / `run_tcp` / `run_http` / `run_quic`；`video` live ingest `run` 拆
`follow` / `drain_tail` / `check_outcome`；`admin::http::run_op` 拆 `classify_outcome`；
`net::mq_hub::register_connection` 拆 `register_peer` / `ensure_user_bucket` /
`bind_route`；`rtc::call::start` 的离线分支收进 `start_offline`；
`rtc::turn::allocate_relayed` 的探包与地址解码收进 `AllocateProbe` /
`probe_allocate` / `relayed_addr` / `attr_text`；`admin::http::build_payload` 未再动。
`>60` 从 38 降到 15（含新抽出的 `ui::lib::handlers` 注册表 74 行）。

同一批再收尾四个 4 层嵌套：`message::open` 的父目录创建抽成 `ensure_parent_dir`；
`video::jobs::spawn_worker` 的一轮收进 `worker_round`；`mcp::list_commands_tool`
把 `inputSchema` 先建好再拼外层对象；`container` 示例的 DOM 垫片提成
`DOM_SHIM` 常量（宏/字符串不再计入函数嵌套）。深嵌套 4 → 0。
`ui::callrtc::run` 的麦克风喂帧与副作用落地收进 `pump_mics` / `apply_all`；
`ui::lib::run` 的 70 项命令注册搬到 `handlers()`；`message` 压测示例
`bench_mailbox::main` 拆 `env_usize` / `write_load` / `count_readable` /
`percentile` / `print_report`。最终 `>60` 14、`>30` 597、重复 127、魔法 172、
嵌套 0，基线已同步收紧。

第五批把最后 14 个 `>60` 全部拆完，`long_fns` 归零：
`platform::wallet` 的 `transfer` / `create_redpacket` 共用 `ref_or_500`（`Option`
→ 缺失即 500）与 `store_idem`，幂等命中提前返回；`platform::domain::wallet::claim_redpacket_tx`
把拒绝条件收进 `claimable`（状态/自领/领完/重复领取）、分钱记账收进 `pay_claim`；
`admin::accounts::login` 拆 `check_mfa`（未开 TOTP / 开而未带码 / 密钥缺失 /
验码失败四分）与 `insert_session`；`admin::jobs::run_batch_items` 拆
`batch_cancelled` 与 `audit_batch_item`；`admin::publishing::create_release` 拆
`draft_release` / `insert_release`；`admin::commerce::resolve_diff` 拆
`check_diff_input` / `open_diff_state` / `set_diff_state`；`platform::media::upload`
拆 `multipart_error`（超限 413、其余 400）与 `check_upload_conv`；
`commerce::redpackets::fund_escrow` 的平衡分录收进 `fund_wallet_into_escrow`；
`net::mq_hub::release_conn` 拆 `drop_conn_registrations` / `release_inflight` /
`unbind_route`；`identity::governance::user::settle_expired` 拆
`expired_suspensions` / `clear_expiry` / `audit_expiry`；`identity::session::open_with`
拆 `normalize_ttl` 与 `write_session_rows`（该文件在 provenance registry 内，
`local_sha256` 已按 LF 归一重算）；`ui::callrtc::run` 的命令排空、声卡喂帧与
播放再收进 `drain_commands` / `AudioSlot`（无 `audio` 特性时是空壳，调用点不再写
`cfg`），`apply_and_play` 由两份 cfg 变体收敛回一个；`platform` 的 `wp01_demo`
示例拆 `register_and_login` / `set_profile` / `read_self`。
复测 `>60 0`、`>30 596`、嵌套 0、重复 127、魔法 172，`quality-gate.py --update`
输出"函数 >60 行: 14 → 0"，五项全 `+0` 通过。

存储端口本波从 6 个 repo、20 个函数扩到 32 个 repo、353 个函数：
`notification` / `audit` / `devices` / `read` / `video_refs` / `qr_login` /
`notes` / `favorites` / `stickers` / `friends` / `status` / `media` / `keys` /
`report` / `inbox` / `perms` / `trust` / `secret` / `recovery` / `reconcile` / `content_index` / `groups_admin` / `relations` / `groups` / `communities` / `moments` / `video_feed` / `account` / `wallet` / `live` / `articles` / `governance`
的参数统一为 `&StorageConnection<'_>`。`notes` 首个打通 `with_tx` 事务内的
`SqliteTx(&Connection)`，`favorites` / `stickers` / `friends` / `status` /
`recovery` 随迁；跨域撤权与通知分别用 `revoke_source_tx` / `notify_tx` / `upsert_tx` 桥接。
`inbox::notify_conn` 已删除——`articles` / `moments` 的发布扇出直接把手里的端口传给
`inbox::notify`，不再绕回旧 `&Connection`。剩下的事务桥接（`notify_tx` / `enqueue_conn` /
`sqlite_borrow`）只服务 `with_tx` 或旧签名入口。
`Db::acquire()` 是新的统一入口，`Db::lock()` 只留给尚未迁移的事务闭包。
`docs/g00-04-storage-port.md` 同步记录；`G00-04-PG` 仍未关闭，因为
`Db::open` 还不接受 PG profile。

### 测试有效性（2026-10-20 晚补测）

**覆盖率**（`cargo llvm-cov --workspace --summary-only`）：行 **82.18%**、函数 82.33%。

补测前最差的几个模块，都是"库层有测试、接口层没测"：

| 模块 | 补测前 | 补测后 |
|---|---|---|
| `platform/src/sessions.rs` | 50.3% | **87.9%**（函数 100%） |
| `platform/src/report.rs` | 55.6%（包内口径） | **86.0%**（函数 97.2%） |

仍然偏低、且**已知原因**的：`video/src/lib.rs` 48.5%（ffmpeg 依赖路径）、
`client/src/lib.rs` 53%（HTTP 客户端，需要本地服务端配合）、
各 `bin/*.rs` 0%（入口二进制，`main()` 不被测试直接调用）。

**变异测试**（`cargo mutants -p wxlike-cmd -f policy.rs`）：

| | 变异体 | 存活 | 分数 |
|---|---|---|---|
| 补测前 | 47 | 15 | 67% |
| 补测后 | 47 | **4（全部为等价变异体）** | **91%** |

剩下 4 个杀不掉的，逐个核对过：`Policy::none()` 的实现就是 `Self::default()`；
`probe_conv` 的三种变异在 `glob_match` 语义下与原实现等价。
**可杀的变异体已全部杀掉。**

教训：测试数量（786）与测试有效性是两件事。覆盖率能告诉你"哪段没跑过"，
变异测试才能告诉你"跑过了但断言错了地方"——本轮 15 个存活变异体里，
有 7 个属于后者（渲染测试只数行数、只断言"关"不断言"开"）。

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
