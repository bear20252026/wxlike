# 并行开发轨道（2026-10-20 起）

> 目的：把"一个人串行做完全部"改成"多条互不冲突的轨道并行"。
> 本文是**边界契约**：谁可以改哪些文件、怎么合并、什么绝对不能并行。

## 一、为什么之前慢（三条，都是可修的）

| 原因 | 量级 | 对策 |
|---|---|---|
| 串行：一次只推进一块，做完才开下一块 | 每块 1–3 小时 | 拆轨道，互不阻塞 |
| 每块的验证成本固定 | `cargo test --workspace` ~8 分钟 + CI ~7 分钟 | 轨道内只跑自己 crate 的测试；合并后统一跑全量 |
| 我做的事偏"深"（协议/一致性）而不是"广"（功能面） | — | 广度类工作（后台/点播/沙箱）交给并行轨道 |

**不修的那部分**：核心链路的改动（信箱、扇出、同步）必须串行——它们共用
`mq_hub.rs` 与 `platform/lib.rs`，并行改必冲突，且冲突处的语义最难判断。

## 二、硬规则（违反会毁掉 main）

1. **一条轨道一个 git worktree**，绝不共用工作目录：
   ```bash
   git worktree add /d/mimo/test/worktrees/<track> -b feat/<track>
   ```
   共用目录时两个 agent 会抢同一份文件与 git index，冲突不可恢复。
2. **轨道只提交，不推送**。推送与 CI 由集成方（我）统一做——每条轨道各推一次
   会触发 4 次 CI，且分支落后时 rebase 成本更高。
3. **文件所有权**：轨道只能改自己名下的文件。跨所有权 = 停下来报告，不要顺手改。
4. **新增 crate 优先**：能开新 crate 就别往既有文件里塞。新 crate 是零冲突的。
5. 每个 worktree 有自己的 `target/`（约 3–5 GB）。磁盘不够时先删旧 worktree。

## 三、轨道划分

### 轨道 A：adminx 只读运营后台（`crates/admin`）— **进行中**

| 项 | 内容 |
|---|---|
| 工作树 | `/d/mimo/test/worktrees/admin`，分支 `feat/adminx` |
| 所有权 | `crates/admin/**`（新建）、`Cargo.toml`（workspace members 一行）、`docs/adminx.md` |
| 受限改动 | 若必须加 Platform 读接口：只允许**新建** `crates/platform/src/admin_read.rs`，并在 `crates/platform/src/lib.rs` 加 `mod admin_read;` + 一行 `pub use` |
| 交付 | 独立二进制 `wxlike-admin`：`WXLIKE_ADMIN_BIND`（默认 127.0.0.1:8090）+ `WXLIKE_ADMIN_TOKEN`（必填，缺失拒绝启动）；`GET /health`、`/api/stats`、`/api/users`、`/api/groups`、`/api/audit`，单页 HTML |
| 验收 | 6 项以上测试；只读（任何写路径都不存在）；token 缺失拒绝启动 |

### 轨道 B：点播 HLS（`crates/video`）— 待启动

| 项 | 内容 |
|---|---|
| 所有权 | `crates/video/**` |
| 交付 | 切片清单生成（`#EXTM3U`）、按码率选流、`Range` 请求解析、播放时长统计 |
| 为什么不冲突 | 纯新逻辑，不碰 platform/net/ui |
| 前置 | 无（不依赖 ffmpeg 二进制，先做**清单与选流**，转码另说） |

### 轨道 C：小程序沙箱 M3（`crates/container` 内新增模块）— 待启动

| 项 | 内容 |
|---|---|
| 所有权 | `crates/container/src/sandbox/**`（新建子模块） |
| 交付 | 权限白名单执行器（`im_bridge` 4 方法之外的调用一律拒绝）、资源配额、超时中断 |
| 风险 | `container` 已有代码，需先读 `package.rs` 的验签流程再动 |

### 轨道 D：只读研究（不写代码）— 待启动

| 项 | 内容 |
|---|---|
| 交付 | ① P5 Flutter + UniFFI 的绑定面清单（`crates/client` 哪些方法要暴露）；② P3 社群层 ADR 草案（community/thread/频道角色）；③ 蓝图 P1–P6 逐项"已完成/未完成"复核表 |
| 为什么值得 | 这三份东西是我下一步的输入，但不需要我写 |

### 串行轨道（我做，不并行）

| 项 | 为什么必须串行 |
|---|---|
| P3 社群层（community/thread/角色） | 改 `platform/lib.rs` 路由 + 新表，与轨道 A 的 `admin_read.rs` 同文件 |
| 多设备多连接（ADR-4） | 改 `mq_hub.rs` 与 `conn.rs`，热点中的热点 |
| 客户端 UI（未读红点、AI 面板） | 改 `ui/src-tauri/src/lib.rs`（2100 行），单文件冲突 |
| 任何 schema 迁移 | 多分支各自加迁移 = 版本号打架 |

## 四、冲突热点文件（任何轨道都不要碰）

```
crates/platform/src/lib.rs      # 路由注册、PlatformOptions、open_with
server/src/lib.rs               # 装配、HistoryDeps、所有 HTTP 端点
ui/src-tauri/src/lib.rs         # 2100 行，命令与本地库编排
crates/net/src/mq_hub.rs        # 扇出、镜像、投递策略
Cargo.lock                      # 各轨道都会改，合并时以 main 为准重新生成
```

例外：轨道 A 允许在 `platform/lib.rs` 加 `mod admin_read;` + `pub use` 各一行
（已写进所有权表）。合并时我手工处理这两行。

## 五、合并顺序（我执行）

1. 轨道先 `cargo fmt` + `cargo clippy -p <自己的 crate> --all-targets -- -D warnings` + 自己的测试；
2. 报告"改了什么、测了什么、留了什么没做"；
3. 我在 main 上 `git merge --no-ff`，跑**全量** `cargo test --workspace`；
4. 绿了才 push（一次 CI）；
5. 冲突时以 main 的语义为准，轨道的改动**重做**而不是硬合。

## 六、进度快照

### 2026-10-20（五条轨道合并后）

- 规模：**22 个 crate、71,206 行 Rust、7,113 行 TS/Dart、737 个测试**，
  CI 五关（macos / linux / windows / lint / doc）。
- 五条轨道全部合入 main（`a6a6d09`）：Zone A 网页端、Zone B 移动端（UniFFI + Flutter）、
  Zone C 社群层、Zone D adminx、Zone E 点播/直播。
- 合并实测：`cargo test --workspace` **770 passed / 0 failed / 9 ignored**；
  `clippy --workspace --all-targets -D warnings` 干净；`tsc --noEmit` 干净。
- **集成期抓到两处 git 看不见的语义冲突**（这是并行开发的真实代价，记下来）：
  1. main 给 `SendFailure` 加了 `throttled` 字段，Zone B 的 FFI 侧仍用字面量构造 →
     编译失败。改法不是补一个 `throttled: false`，而是改用 `retryable()/permanent()`
     构造器——下次再加字段，所有调用点自动跟着走。
  2. `PlatformOptions.admin_token` 从来没有被 `server/src/lib.rs` 接上环境变量，
     所以 `/api/admin/*` 在生产里恒 403（adminx 自己的冒烟测试照出了这条）。
     现在按 `WXLIKE_ADMIN_TOKEN` 接线，未设置 = 管理面关闭。
- 加权进度约 **58%**（P1 90% / P2 80% / P3 75% / P4 65% / P5 45% / P6 35%，
  权重取蓝图工期）。

### 2026-10-20（轨道开工前，留作对照）

- 规模：20 个 crate、50,081 行 Rust、602 个测试。
- 加权进度约 **43%**（P1 90% / P2 75% / P3 50% / P4 65% / P5 15% / P6 25%）。
- 当时已闭环：Mars 的保活、发送队列、自适应心跳、网络诊断；
  命令面 18/18 + MCP + 审计；P3 信箱接线（顺序/未读/同步/成员）。
