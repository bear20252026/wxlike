# 任务单：ureq 2.x → 3.x 迁移

> 状态：**已完成**（2026-10-22）　｜　实际耗时：约 1.5 人日（大部分花在语义对齐上，不是机械替换）
>
> 接单人先读 §0 与 §6，再动手。**这份单子自包含**：仓库路径、命令、验收标准、
> 禁止事项都写在里面，不需要额外上下文。

---

## 0. 一句话

把五个位置（`admin` / `client` / `video` / `server` 测试 / `ui-src-tauri`）里对
`ureq 2.x` 的依赖升到 `3.x`，并**保住三件事**：错误文案不变、超时行为不变、
multipart 上传不变。

> 官方迁移文档（`MIGRATE-2-to-3.md`）自己声明"并非详尽无遗"，实际改动比它写的
> 多得多。下面 §7 是这次真正踩到的东西。

---

## 1. 为什么做（必要性判断，不是"为了新而新"）

### ⚠️ 先更正一处错误判断（2026-10-21，issue #21）

这份任务单最初写的"必须做的理由"是：

> `webpki-roots 0.26.11` 信任了私钥已公开的 emSign 根证书，修复只在 1.0.10+，
> 而 `ureq 2.x` 把 0.26 写死，**不升 ureq 就永远修不掉**。

**这个判断是错的。** 事实（`cargo tree` / 锁文件复核）：

- `webpki-roots 0.26.11` 本身**不含**独立根证书库，它转导出单独解析的
  `webpki-root-certs 1.x`；RustSec 公告把 `=0.26.11` 列为 **unaffected**。
- 真正要修的是**转导出的 1.x** 必须 `>= 1.0.10`。这不需要动 ureq：
  根 `Cargo.lock` 里 `1.x` 已经是 1.0.10，`cargo deny check advisories` 在
  删掉 `RUSTSEC-2026-0381` 豁免后依然 `ok`。
- 真正漏掉的是**桌面独立工程**（`ui/src-tauri/Cargo.lock` 不在根 workspace）：
  它还停在 `webpki-roots 1.0.9` / `webpki-root-certs 1.0.9` /
  `rustls-webpki 0.103.15`。已单独 `cargo update` 到 1.0.10 / 1.0.10 / 0.103.16。

所以：**根证书修复与 ureq 主版本迁移是两件事**，前者已完成，不该再拿它
当迁移的理由。

### 真正剩下的理由（都不是"安全救火"）

1. **依赖收口**：现在同时存在 `webpki-roots 0.26` 与 `1.x` 两条线
   （`0.26.11` 只是转发壳）。升 ureq 3 后只剩一条。
2. **可用系统根证书**：`ureq 3` 提供 `RootCerts::PlatformVerifier`，
   让客户端用操作系统的信任库，而不是打包一份 CA 列表——运维换 CA 不用等我们发版。
3. **长期维护**：`ureq 2.x` 已停止功能演进。

> 风险面（没变）：`ureq` 只用于**我们自己的客户端对自家服务**发 HTTP 请求
> （`admin` 转发管理面、`client` 是 CLI/MCP 的底座、`video` 发转码回调），
> 不是浏览器式的任意站点访问；生产环境前面还有 TLS 终止。
> 所以这是 **P2 的工程化迁移**，不是 P1 的安全修复。

### 顺带解决的两件事

1. **依赖收口**：`ureq 3.1.0` 起把 `webpki-roots` 升到 1.0，我们不用再单独维护
   `webpki-roots 0.26` 这条线。
2. **可以改用系统根证书**：`ureq 3` 提供 `RootCerts::PlatformVerifier`，
   让客户端用操作系统的信任库，而不是打包一份 CA 列表——运维换 CA 不用等我们发版。

### 不做的理由（也写出来，便于拍板）

- 迁移是**破坏性 API 变更**，三个 crate 共 91 处引用，其中 `crates/ffi/src/error.rs`
  还在**解析 ureq 的错误文案字符串**，这是最容易迁出静默 bug 的地方（见 §5 风险 R1）。
- 如果本迭代有更急的事，可以延后**一个**迭代；但不要"无限期挂着豁免"。

---

## 2. 现状盘点（接单人先自己跑一遍确认）

```bash
cd /d/mimo/test/wxlike            # Windows；Linux/macOS 换成对应路径
grep -rn "ureq" Cargo.toml crates/*/Cargo.toml
grep -rn "ureq::" --include="*.rs" crates/ server/ | wc -l   # 实测 91
```

| crate | 声明 | 用途 | 关键文件 |
|---|---|---|---|
| `wxlike-client` | `ureq = { version = "2", features = ["json"] }` | CLI / MCP 的 HTTP 底座；`Http` 结构封装了 `req/expect_json/post_idempotent/upload/download/post_video_job` | `crates/client/src/lib.rs`（12 处） |
| `wxlike-admin` | 同上 | 把后台写操作转发到平台管理面 | `crates/admin/src/upstream.rs`（6 处） |
| `wxlike-video` | 同上 | 转码完成后的出站回调；接入节点推片 | `crates/video/src/jobs.rs`（8 处）、`src/bin/wxlike-live-ingest.rs` |
| （测试） | — | 服务端静态页测试、平台 TCP 测试、admin 测试夹具 | `server/src/web_static.rs`、`crates/platform/tests/call_tcp.rs`、`crates/admin/tests/common/mod.rs` |

### 实际用到的 2.x API（这就是全部工作量）

| 2.x 写法 | 出现次数 | 说明 |
|---|---|---|
| `.set(name, value)` | 12 | 设请求头 |
| `.call()` | 4 | 发无 body 请求 |
| `ureq::Error::Status(status, resp)` | 8 | **非 2xx 时读响应体**（错误文案靠它） |
| `.timeout(Duration)` | 4 | 单请求超时 |
| `.send_json(&value)` | 4 | JSON body |
| `.into_string()` | 4 | 读文本响应 |
| `.send_bytes(&body)` | 2 | 手搭 multipart（`upload` / `post_video_job`） |
| `ureq::post/put/get/delete` | 4 | 便捷函数 |
| （无共享 Agent） | — | **现状：每个请求各自 `ureq::get/post/...`，超时写在 `Http::req` 里** |
| `resp.into_json::<Value>()` | 6 | 读 JSON 响应 |

---

## 3. API 映射表（2.x → 3.x）

> **⚠️ 标记「待确认」的三项，接单人必须在实现前用 `cargo doc -p ureq --open`
> 或 docs.rs 核对当前 3.x 版本的真实签名**，不要照抄本表。本表基于
> ureq 3.4.2 文档整理，3.x 仍在演进（`RequestBuilder` 在 3.0 从 Deref 改成了
> 显式包装器）。

| 2.x | 3.x | 备注 |
|---|---|---|
| `agent.get(url)` / `ureq::get(url)` | 同名 | 便捷函数仍在 |
| `.set(k, v)` | **`.header(k, v)`** | 重命名 |
| `.call()` | 同名 | |
| `.send_json(&v)` | 同名 | 需 `json` feature（默认不带，要在 Cargo.toml 显式开） |
| `.send_bytes(&b)` | **`.send(&b[..])`** | `&[u8]`/`Vec<u8>` 都实现了 `AsSendBody` |
| `resp.into_string()` | **`resp.body_mut().read_to_string()`** | |
| `resp.into_json::<T>()` | **`resp.body_mut().read_json::<T>()`** | |
| `ureq::Error::Status(code, resp)` | **`ureq::Error::StatusCode(code)`** | **拿不到响应体**，见 §4 决策 D1 |
| 无 Agent（每个请求单独建） | **`Agent::config_builder().timeout_global(Some(d)).build().new_agent()`**，`Http` 持有它 | `Http::req` 的返回类型要从 `ureq::Request` 变成 `RequestBuilder` |
| `.timeout(d)`（单请求） | **待确认**：`RequestBuilder` 的 config/超时入口，或统一提到 Agent 级 | 若 3.x 不支持单请求超时，改成"每个用途一个 Agent" |
| `ureq::Error::Transport(_)` 等 | 同名枚举仍在，但**文案变了** | 见 §5 R1 |

---

## 4. 三个必须由接单人拍板的设计决定

### D1. 非 2xx 的错误体怎么读

2.x 里 `Error::Status(code, resp)` 直接给 `Response`，我们的错误文案是
`"{服务端返回的 JSON} ({status})"`。3.x 的 `Error::StatusCode(u16)` **不带响应体**。

**要求**：保持现有文案形状不变（FFI 与 CLI 的测试都断言了它）。做法二选一：

- **推荐**：Agent 配置 `http_status_as_error(false)`，然后**自己判 status**：
  非 2xx 时照常读 body 并拼成同样的字符串。
- 备选：捕获 `StatusCode` 后重新发一次请求——**不要这么做**，多一次网络往返，
  而且对非幂等请求（POST 发消息）是错的。

### D2. 超时怎么放

现在有 4 处超时（`client` 的 `Http::req` 里 10s、`admin` 15s、`live-ingest` 15s、
视频回调 `CALLBACK_TIMEOUT_MS`）。**关键事实：`crates/client` 现在没有共享 Agent**，
每个请求都用 `ureq::get/post/...` 现建，超时挂在请求上。

3.x 的超时在 Agent 配置里，所以 `Http` 必须**新增一个 `Agent` 字段**，
`Http::req` 的返回类型从 `ureq::Request` 变成 `RequestBuilder`。这一步会波及
`Http::req` 的所有调用点（`client` 内部为主，`cli`/`mcp`/`ffi` 通过 `Executor` 间接用）。

按用途各建一个 Agent，**不要**把全局超时统一成 10 秒——视频回调要 15 秒，
混成一个值会让慢回调被误判失败。

### D3. 根证书用哪套

- 想**彻底去掉 webpki-roots 依赖**：开 `platform-verifier` feature，配
  `RootCerts::PlatformVerifier`（用系统信任库）。
- 想**保持行为完全不变**：用 3.x 默认（webpki-roots 1.x）。

推荐前者，但要注意：Linux CI 容器里可能没有系统 CA 包，**必须确认三平台 CI 都绿**；
如果 CI 环境缺 CA，就退回后者，并在任务单回执里写明原因。

---

## 5. 风险（按"迁出静默 bug 的概率"排序）

| 编号 | 风险 | 为什么危险 | 怎么防 |
|---|---|---|---|
| **R1** | `crates/ffi/src/error.rs` 解析 ureq 的**错误文案**（`"Network Error: …"`、`"Connection Failed: Connect error"`） | ureq 3 重写了错误类型与文案。**编译器不会报错**，但手机端的错误分类会全部落到"未知错误"，用户看到的提示从"网络不可用"变成"未知错误" | 先跑现有 FFI 测试；再补一条测试：用 `Connection refused` 与 DNS 失败两种真实错误，断言分类结果不变。**文案映射表要单独写在一个函数里并加注释说明来源** |
| R2 | multipart 手搭（`upload` / `post_video_job`） | 我们自己拼 boundary 与 `Content-Disposition`，3.x 的 `send` 会加自己的头，可能冲突 | 用现有测试（媒体上传、视频作业提交）验证；必要时改用 3.x 的 `multipart` feature |
| R3 | 非 2xx 响应体读取（D1） | 改错会让所有错误提示变成 `HTTP 400` 而没有服务端原因，排障成本陡增 | 保留并扩展 `expect_json` 的错误文案断言 |
| R4 | 依赖版本漂移 | ureq 3 依赖 rustls 0.23.46+，可能牵动 quinn / str0m 的 rustls 版本 | 迁移后必须跑**全量** `cargo test --workspace` 与 `cargo deny check` |
| R5 | CI 的 CA 环境（D3） | Linux 容器缺系统 CA 时 TLS 全失败 | 三平台 CI 必须绿；失败就退回默认根证书 |

---

## 6. 任务卡（可拆给一个人，按顺序做）

| 卡 | 内容 | 验收 |
|---|---|---|
| **T0** | 读 §2 自查，确认 API 清单与本文一致；把差异写进回执 | 差异清单（没有就写"一致"） |
| **T1** | `crates/client/Cargo.toml` 升 `ureq = "3"` + `features = ["json"]`；改 `src/lib.rs` 全部调用点；`Http::req` 的返回类型跟着改 | `cargo test -p wxlike-client` 全绿；错误文案与迁移前**逐字节相同**（用现有断言验证） |
| **T2** | `crates/admin`：`upstream.rs` 的转发与错误映射 | `cargo test -p wxlike-admin` 全绿（含 D1/D2 的行为） |
| **T3** | `crates/video`：`jobs.rs` 回调 + `bin/wxlike-live-ingest.rs` 推片 | `cargo test -p wxlike-video` 全绿；`CALLBACK_TIMEOUT_MS` 行为不变 |
| **T4** | 测试夹具：`server/src/web_static.rs`、`crates/platform/tests/call_tcp.rs`、`crates/admin/tests/common/mod.rs` | 三个 crate 的测试全绿 |
| **T5** | **R1**：`crates/ffi/src/error.rs` 的文案映射重写 + 新测试 | 见 §5 R1 的"怎么防" |
| **T6** | 配置收口：`deny.toml` 删掉 `RUSTSEC-2026-0381` 的豁免；确认 `cargo deny check` 四项全 ok | `cargo deny check` 输出 `advisories ok, bans ok, licenses ok, sources ok` |
| **T7** | 全量验收（见 §7） | 见 §7 |

---

## 7. 验收标准（全部满足才算完）

```bash
cd /d/mimo/test/wxlike

# 1) 全量测试（不许比迁移前少）
cargo test --workspace

# 2) 结构闸门（不许变差）
python scripts/quality-gate.py

# 3) 供应链：豁免必须被删掉，而不是换个 ID 继续挂
cargo deny check          # 期望：advisories ok, bans ok, licenses ok, sources ok
grep -n "RUSTSEC-2026-0381" deny.toml   # 期望：无输出

# 4) 拼写
typos

# 5) 依赖里不再有 webpki-roots 0.26（若选 D3 前者，应完全没有 webpki-roots）
cargo tree -i webpki-roots 2>&1 | head
```

外加：

- `cargo clippy --workspace --all-targets -- -D warnings` 干净；
- `cargo fmt --check` 干净；
- 三平台 CI 全绿（macOS / Linux / Windows）；
- **测试数量不少于迁移前**（当前基线：`cargo test --workspace` 786 passed）；
- 提交信息里写清"哪一处 API 变化导致了行为差异"，没有就写"无行为差异"。

---

## 8. 协作规则（沿用仓库既有约定）

1. **一个 worktree 一条分支**：`git worktree add /d/mimo/test/worktrees/ureq3 -b feat/ureq3`。
   绝不与别人共用工作目录。
2. **只提交，不推送**：推送与 CI 由集成方统一做。
3. **文件所有权**：只改 `crates/{client,admin,video}/**`、`crates/ffi/src/error.rs`、
   三个测试夹具、`deny.toml`。**不要碰** `crates/platform/src/lib.rs`、
   `server/src/lib.rs`、`crates/net/src/mq_hub.rs`、`Cargo.toml`（workspace 根）。
4. **不新增依赖**：`ureq 3` 之外不允许引入新的 HTTP 客户端（`reqwest` 一律拒绝）。
5. **不改变对外行为**：命令面（catalog）、HTTP 路径、错误文案形状、超时时长都不许变。
   迁移不是重构的机会。

---

## 9. 明确不在范围内

- 不把 `ureq` 换成 `reqwest` / `hyper`（那是另一件事，收益不抵风险）。
- 不改 `crates/net/src/diag.rs` 的手写 TCP/HTTP 探测（那里**故意**不用 HTTP 库，
  注释写明了原因：诊断要看到原始响应）。
- 不动 `server/src/web_static.rs` 的**生产**逻辑，只改它的测试。
- 不顺手升级其它依赖（`cargo update` 只允许动 `ureq` 及其直接传递依赖）。

---

## 10. 回执模板（交回给集成方）

```
T0 自查差异：
T1 client：改了 N 处；错误文案：一致 / 差异说明
T2 admin：...
T3 video：...
T4 夹具：...
T5 FFI 文案映射：旧→新 对照表（贴关键几条）
T6 deny：advisories ok
T7 验收：cargo test 786 passed / clippy 干净 / 三平台 CI 绿
遗留问题：
```


---

## 7. 实际迁移记录（2026-10-22）

### 7.1 API 形状变化

| 2.x | 3.x |
|---|---|
| `ureq::Request` / `ureq::Response` | `http::Request` / `http::Response<Body>`（`ureq::http`） |
| `.set(k, v)` | `.header(k, v)` |
| `.send_bytes(&v)` / `.send_string(&v)` | `.send(&v)` |
| `.into_json::<T>()` / `.into_string()` | `resp.body_mut().read_json::<T>()` / `read_to_string()` |
| `.into_reader()` | `resp.into_body().into_reader()` |
| `resp.content_type()` | `resp.headers().get("content-type")` |
| `.timeout(d)` | agent 级 `.timeout_global(Some(d))`（或请求级 `.config()`） |
| `Err(Error::Status(code, resp))` | `Err(Error::StatusCode(code))`——**不带响应体** |
| `Err(Error::Transport(e))` + `ErrorKind` | `Error::Io / Timeout / HostNotFound / ConnectionFailed / BadUri` |
| POST 不跟随重定向 | **默认跟随**（还把 POST 改成 GET） |

`RequestBuilder` 按"带不带 body"分成 `WithoutBody` / `WithBody` 两个 typestate，
所以 `Http::req()`（GET/DELETE）与 `Http::req_write()`（POST/PUT）必须分开——
一个闭包或一个 match 没法同时返回两种类型。

### 7.2 语义对齐（这才是主要工作量）

1. **错误分类不能再靠字符串**。3.x 的 `Display` 直接透出操作系统文案
   （Windows 上是"由于目标计算机积极拒绝，无法连接"），而 `crates/ffi` 的
   `classify()` 靠稳定英文特征串。所以在 `crates/client` 加了
   `transport_text()`：按**变体**翻译回 2.x 的形状
   （`Connection Failed: Connect error: connection refused`），
   `NETWORK_MARKERS` 一条都不用改。
2. **`http_status_as_error(false)`**。调用方要按 status 分流（404/409 表达业务
   结果），还要读服务端 `error` 字段给用户看原因。admin 那边额外守住旧契约：
   非 2xx 仍然映射成 `AdminError::upstream`。
3. **回调关掉重定向**（`max_redirects(0)`）：issue #8 要求"不跟随重定向"，
   3 默认会跟，显式关掉；3xx 于是落回 `classify_status` 的永久档。
4. **`send_json` 序列化空格变了**（`{"a": 1}` 而非 `{"a":1}`）：
   FFI 测试里按字符串匹配 body 的断言改成解析 JSON。

### 7.3 顺带

`Http` 现在持有一个共享 `ureq::Agent`：连接池 + 全局超时都在这，
2.x 的 `ureq::get()` 每次新建 agent（无池化）。

### 7.4 没做的

- **系统根证书**（`RootCerts::PlatformVerifier`）：任务书里列为"另一可选方案"，
  需要三平台/Android 实测，本次不动——继续用打包的 webpki-roots。
- **桌面独立工程**（`ui/src-tauri`）：本次只改了根 workspace 的 5 个位置；
  Tauri 工程有独立锁文件，升级需要单独一轮（它的 `ureq` 仍在 2.x）。
