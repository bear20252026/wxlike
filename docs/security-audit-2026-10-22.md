# wxlike 深度安全审计报告（2026-10-22）

> 第三轮独立审计。前两轮见 `docs/audit-report.md`、`docs/audit-progress.md` 与 issue #3–#22。
> 本报告只记录**可复现的增量发现**，每条都给出文件与行号；无法在代码中复现的猜测一律不写入。

## 0. 审计基线

| 项 | 值 |
|---|---|
| 仓库 | `bear20252026/wxlike` |
| 基线 commit | `62124c5`（main） |
| 工作区状态 | **脏**：18 个文件改动 + 未跟踪的 `patch_client2.py`、`ureq3_migrate.py`。差异处单独标注 |
| 规模 | 229 个 `.rs` / 约 76.5k 行，25 crate + `server` + `ui`(Tauri2) + `apps/mobile`(Flutter) |
| 方法 | 人工审计（密码学、鉴权、协议、并发、资源边界）+ 模式检索 + 依赖/许可核对 + CI 审阅；**每条结论均在代码中回读核对** |
| 未覆盖 | 未做动态渗透/模糊测试；本机无 cargo 工具链，依赖结论基于 `Cargo.lock` 静态核对 |

威胁模型取自仓库自述 `docs/threat-model.md:11`：hub/server 与 platform **零信任**
（"能看、能改、能重放一切经过它的字节"）。下面 C-1 的本质是**实现与该自述不符**。

## 1. 结论摘要

| 级别 | 条数 | 主题 |
|---|---|---|
| Critical | 1 | E2EE 信任模型未落地：预密钥包无签名、无指纹校验 |
| High | 11 | 无鉴权铸币口、会话令牌明文、口令轮数不足、预密钥可耗尽、页包解压无上限、风控覆盖不全、JS 宿主无执行限制、TURN 静态凭据、WS 无帧上限、Redis 口令进消费者名、秘密聊天 TTL 兜底不覆盖实时链路 |
| Medium | 12 | 弱密码学脚手架仍导出、设备密钥同盘、JS 字符串拼装、密钥污染、棘轮窗口驱逐、内存增长、panic 面、迁移脚本、许可元数据、MQ 路由未接线、SFU 无鉴权 |
| Low | 3 | 3xx 语义、`limits` 名不副实、CI 缺 audit/覆盖率闸门 |

优先三件事：
1. **给预密钥包签名 + 做指纹校验**（否则"服务器零信任"只是文档里一句话）；
2. **风控做成中间件**（现在是 handler 里手工调用，必然漏）；
3. **给 `/api/dev/fund` 加鉴权**（一个环境变量决定资金安全边界）。

## 2. Critical

### C-1 E2EE 预密钥包无签名，服务端可替换身份密钥实施中间人

- 位置：`crates/core/src/e2ee.rs:137-145`（`PreKeyBundle` 定义）、`352-365`（`encrypt_dm` 直接用
  `bundle.identity` / `one_time_keys` 建会话）、`crates/platform/src/keys.rs:39-73`（`publish_keys`）
- 证据：
  - `PreKeyBundle` 有 `identity_signing`（Ed25519）字段，但**全仓库没有任何一处对它签名或验签**
    （`grep -rn identity_signing` 只命中结构体定义、DB 列与测试）。
  - `Crypto::encrypt_dm` 拿到服务端返回的 bundle 后直接
    `create_outbound_session(cfg, identity, otk)`，不校验来源。
  - `publish_keys` 只校验长度（`identity.len() <= 128`、OTK 数量 ≤ 256），不校验 base64/曲线点。
  - 全仓库没有用户可见的安全码/指纹比对实现：`grep -rn "fingerprint\|安全码\|sas"` 只在
    `core/tests/e2ee.rs` 与 QUIC 证书 pin（`net/src/transport.rs`）中出现。
- 影响：能写 `prekey_bundles` 表的一方（服务端运营者、拿到 DB/备份的攻击者）可把受害者的
  `identity` 换成自己的公钥，之后所有发给受害者的消息都能被解密，**用户侧无任何提示**。
  这与 `docs/threat-model.md` 明写的"server 零信任"直接冲突。
- 修复：
  1. `PreKeyBundle` 增加 `signatures`，用 Ed25519 身份密钥对 `identity` 与每个 OTK 签名；
  2. 客户端建会话前验签，失败即拒绝（fail-closed，与现有 `encrypt_dm_existing` 一致）；
  3. `publish_keys` 校验 base64 与 Curve25519 公钥合法性；
  4. 提供安全码/指纹比对 UI，并在对端身份密钥变化时强制告警。

## 3. High

### H-1 `/api/dev/fund` 无鉴权，可给任意账户凭空加钱

- 位置：`server/src/lib.rs:1139-1163`，挂载于 `server/src/lib.rs:444`
- 证据：handler 签名里**没有 `Authed`**；`user_id` 直接取自请求体；
  仅由 `WXLIKE_DEV_FAUCET=1` 开关（仓库内确认默认未开启，只有 README:517 提到）。
- 影响：开启后任何人可 `POST {"user_id":"<任意>","amount":N}` 无限铸币（金额受
  `check_amount` 约束为正数且 ≤ 约 68 亿，所以不能清零他人余额，但可无限增发）。
  平台的钱包/账本/红包都基于同一余额，账目会被污染。安全边界完全由一个环境变量承担，
  无鉴权、无额度、无审计日志。
- 修复：即使开启也要求管理员 `Authed`；加单笔/单日上限；写审计日志；启动时打印醒目警告。

### H-2 会话令牌明文入库

- 位置：`crates/platform/src/db.rs:281-286`（`sessions(token TEXT PRIMARY KEY, …)`）
- 影响：SQLite 文件（备份、快照、容器卷）泄漏 = 所有在线会话凭证可直接使用，且难以吊销。
  与"密码用盐+慢哈希"的做法不一致。列表接口不返回 token（`sessions.rs:6-9` 注释）值得肯定，
  但存储层没有同等保护。
- 修复：库中只存 `sha256(token)`，查询时哈希后比对（token 本身 32 字节随机，无需慢哈希）。

### H-3 口令哈希轮数低于现行基线，且未用内存硬函数

- 位置：`crates/platform/src/auth.rs:20`（`DEFAULT_ROUNDS = 100_000`，PBKDF2-HMAC-SHA256）
- 影响：OWASP 2023 对 PBKDF2-HMAC-SHA256 建议 **600,000** 轮；100k 在 GPU 下抗离线爆破能力不足，
  且 PBKDF2 无内存硬度。`rounds` 已按用户落库（可平滑提升，这点设计得对），但默认值决定新账号强度。
- 修复：默认提到 600k；更建议迁移 Argon2id（旧账号登录成功时透明重写）。

### H-4 预密钥可被任意已登录用户耗尽（OTK exhaustion）

- 位置：`crates/platform/src/keys.rs:170-179`（`claim`）、`keys.rs:26`（`MAX_PUBLISHED_OTKS = 256`）
- 证据：`claim` 只要求 `Authed`，**不校验调用者与 `user_id` 的任何关系**（对比 `media.rs` 已按
  "好友或同会话"校验）；`grep -n "risk\|limit" crates/platform/src/keys.rs` **零命中**，即无限流。
- 影响：一个账号循环 `GET /api/keys/{victim}` 即可把受害者 256 个一次性密钥全部消费，
  之后他人无法与其新建 Olm 会话；每次 claim 伴随一次 DB 写，同时是廉价写放大 DoS。
- 修复：claim 纳入风控闸门并按 (调用者, 目标) 限流；要求好友/同会话关系；耗尽时对受害者告警。

### H-5 动态页包（DPG1）解压无上限、无签名

- 位置：`crates/dpage/src/lib.rs:125-153`
- 证据：`let files: FileMap = bincode::deserialize(&zstd::stream::decode_all(&blob[STUB_LEN..])?)?;`
  —— 既无解压字节上限，bincode 1.x 反序列化 `Vec<FileEntry>` 也无条目/长度上限
  （`with_limit` 未使用）。`unpack` 只校验 magic、version 与 `blob.len() == STUB_LEN + payload_len`。
- 影响：恶意 `.dpage` 可在宿主进程触发超大内存分配 → OOM；包本身无签名，配合 JS 容器即
  构成"任意代码装载"。
- 修复：`decode_all` 换成带 `take(MAX)` 的流式解压（如 64 MiB）；bincode 加 `with_limit`
  或显式条目数上限；生产装载路径要求 Ed25519 签名并校验。

### H-6 风控闸门只覆盖 4 个入口，写接口与好友/媒体面全裸

- 位置：`crates/platform/src/recovery.rs:71-135`（`risk_gate*` 定义）
- 证据：全仓库调用点只有 4 处 ——
  `recovery.rs:352`（Login）、`users.rs:319`（Register）、`users.rs:406`（Reset）、
  `report.rs:429`（Report）。`friends.rs`、`media.rs`、`groups.rs`、`moments.rs`、
  `wallet.rs`、`communities.rs` **均无风控调用**。
- 影响：好友请求轰炸、媒体上传轰炸、群组/朋友圈刷写无任何速率约束。
  根因是"在每个 handler 里手工调用闸门"这一模式本身——漏掉是必然，不是意外。
- 修复：把风控做成 axum 中间件或统一 extractor，按 `Action` 声明式覆盖所有写接口。

### H-7 JS 宿主执行没有任何指令/时间/内存限制

- 位置：`crates/container/src/engine.rs:38-44`（`eval`）、`46-62`（`call_global`）
- 证据：`self.ctx.eval(Source::from_bytes(code.as_bytes()))` 直接执行；全文件无
  `instruction limit` / timeout / 内存配额（Boa 提供 `Context` 的相关配置，未使用）。
- 影响：任一页包里的 `while(true){}` 即可永久冻结宿主线程（桌面端/服务端进程）。
- 修复：设置指令上限或把 eval 放到可超时终止的工作线程；内存配额同理。
  附带：`call_global` 用 `format!("{}({})", name, args_json.join(", "))` 拼 JS 源码执行，
  若 `name` 可受页包/网络影响即构成 JS 注入（见 M-3）。

### H-8 TURN 使用全局静态长期凭据

- 位置：`crates/rtc/src/ice.rs:45-70`（读 `WXLIKE_TURN_URL/USER/PASS`）、`deploy/README.md:92`
- 证据：`IceConfig::from_env` 读取**进程级共享**的一组用户名/口令，全仓库（含 deploy/scripts）
  没有按用户签发的凭据机制；配置文档也只描述"配一次"。
  **桌面端同样从环境变量读同一组凭据**：`ui/src-tauri/src/callrtc.rs:266-280`
  （`wxlike_rtc::ice::TurnConfig::from_env()` → `TurnClient::allocate`），
  即这套凭据会被分发/配置到每一个客户端。
- 影响：凭据一旦泄漏（脚本入库、日志、客户端包、docker 环境变量），任何人可把你的 TURN
  当作任意流量中继：带宽盗用、被用作反射源、绕过网络策略。若客户端下发同一组凭据，等于公开。
- 修复：改用 coturn `static-auth-secret`：服务端按 `user = <expiry>:<user_id>` +
  `HMAC-SHA1(secret, user)` 签发**短期、按用户**的凭据（≤ 1 小时）；仓库内禁止明文默认口令。

### H-9 WebSocket 网关未设单帧上限，且鉴权前已开始收数据

- 位置：`server/src/lib.rs:492-505`（`ws_routes`）、`crates/net/src/conn.rs:139-152`（`WsRx`）
- 证据：`WebSocketUpgrade::on_upgrade` 未调用 `max_message_size` / `max_frame_size`；
  `WsRx::read_frame` 直接 `b.to_vec()` 返回，没有像 QUIC 路径那样的 `MAX_FRAME` 校验
  （对比 `crates/net/src/framing.rs:18-31`，QUIC 侧有 8 MiB 上限且先校验长度再分配）。
  身份校验发生在 `Hello` 帧之后（`server/src/lib.rs:371-390` 的 `lookup_token`）。
- 影响：未认证连接可推送 axum 默认上限（64 MiB）级别的 WS 消息；QUIC/WS 两条路径的
  资源边界不一致，WS 侧更宽松。
- 修复：`on_upgrade` 设 `max_message_size(MAX_FRAME)`；`WsRx` 加同样的长度校验；
  理想情况下在 upgrade 前完成 token 校验。

### H-10 Redis 消费者名包含完整连接 URL（含口令）

- 位置：`server/src/lib.rs:579-581`（`url_consumer`）、用于 `lib.rs:542`
- 证据：`fn url_consumer(url: &str) -> String { format!("{url}-pid-{}", std::process::id()) }`
  —— 直接把 Redis 连接串（通常形如 `redis://:password@host:6379`）当作消费者名。
- 影响：消费者名会出现在 `XINFO CONSUMERS` / `XPENDING` 输出、Redis 慢日志与任何
  把这些结果打出来的运维命令里 → **Redis 口令明文泄漏**到日志与屏幕。
- 修复：消费者名只用 hub_id + pid（或对 URL 做哈希），不要把连接串拼进任何对外可见的标识。

### H-11 秘密聊天的 TTL / 禁止转发兜底只覆盖 HTTP 面，实时链路未见过滤

- 证据：`grep -rn "ttl" crates/net/src crates/message/src` 的命中只有两类 ——
  `net/src/mq_hub.rs:467-471` 是**路由租约** `lease_ttl_ms()`（与消息 TTL 无关），
  `net/src/policy.rs` 命中的是 `Throttle` 字样；`crates/message/src` 只有 `lib.rs` 且不含 TTL 判断。
  服务端对"秘密聊天消息不得转发 / 超时后不得再检索"的执行集中在 platform 的 HTTP 路径
  （`server/tests/secret_enforcement.rs` 覆盖的就是这部分）。
- 影响：通过 QUIC/WS 长连接与 MQ 分发路径投递的消息，未见同等的 TTL/禁止转发过滤 ——
  "服务端兜底"在实时链路上可能是空的，这正是 issue #15（仍 OPEN）里"没有形成端到端闭环"的具体缺口。
- 修复：把 TTL/转发策略下沉到 `wxlike_net` 的投递判定（一处生效，两条链路都覆盖），
  并为 MQ 路径补一条与 HTTP 侧同款的回归测试。
- 说明：本条基于定向检索，未逐行通读 `net`/`mq` 的全部分发路径，请以 #15 的验收清单为准复核。

## 4. Medium

| # | 问题 | 位置 | 影响 / 修复 |
|---|---|---|---|
| M-11 | MQ 路由表从未被真正维护：`register_hub` / `unregister_hub`（`crates/mq/src/lib.rs:50-51`）在各后端都有实现，但 **server 中零调用**（`grep register_hub server/src` 无命中），路由只靠 `add_user`/`remove_user` | `crates/mq/src/redis_backend.rs:177-188`、`server/src/lib.rs:542` | 多节点下 hub 上下线不会更新路由表，跨节点投递退化/误投。接线 register/unregister 或明确"单节点"边界 |
| M-12 | SFU `join(peer, offer, video)` 只按 peer 字符串入会，无鉴权、无房间成员校验 | `crates/sfu/src/lib.rs:122-160` | 当前 server 未接线（风险暂未生效），接线前必须校验调用者身份与房间 membership |
| M-1 | 旧密码学脚手架仍 `pub` 导出：static-static X25519 + 常量 AAD + 随机 nonce，**无棘轮（无前向保密）、AAD 不含任何身份/会话上下文、无序号（可重放重排）**；全仓库零调用者 | `crates/core/src/crypto.rs`（`lib.rs:3` 导出） | 等着被误用的弱加密 API。删除或 `#[deprecated]` + 明确警示 |
| M-2 | 秘密聊天快照的设备密钥与密文**同一目录**裸存，无口令派生；`write_private` 只在 Unix 上 `chmod 0600` | `core/src/e2ee.rs:98-132`、`ui/src-tauri/src/lib.rs:1463-1532` | 拷走数据目录 = 拷走身份。改 Argon2id 派生或 OS 密钥链/DPAPI；Windows 收紧 ACL |
| M-3 | `call_global` 用字符串拼 JS 再 eval | `container/src/engine.rs:56-58` | `name` 可控即 JS 注入。改为直接构造 `JsValue` 调用，不走 eval |
| M-4 | `publish_keys` 不校验 OTK 的 base64 / 曲线点合法性 | `platform/src/keys.rs:46-53` | 可写入脏密钥，使目标用户不可被建立会话。加 `Curve25519PublicKey::from_base64` 校验 |
| M-5 | 重排序窗口满时按 `BTreeMap::pop_last()` 驱逐，驱逐的是**字典序最大**的键，不是索引最远的消息 | `core/src/e2ee.rs:531-543` | 高乱序下可能丢掉"马上就绪"的消息。按 idx 距离驱逐 |
| M-6 | 群会话重放水印表 `group_seen: HashMap<(group, sender, session_id), u32>` 无淘汰 | `core/src/e2ee.rs:310` | 长期运行内存单调增长。加 LRU 上限 |
| M-7 | 非测试代码 517 处 `unwrap()/expect()`，集中在 `admin/read.rs`、`platform/wallet.rs`、`core/outbox.rs`、`net/offline.rs` | 全局 | 畸形输入/DB 异常可 panic。请求路径上的改为 `?` |
| M-8 | `ureq3_migrate.py` 用正则把 `.set(` 全局替换为 `.header(`；工作区有 18 个未提交文件（含 HTTP 客户端行为变更） | `ureq3_migrate.py:8-11`；`git status` | 非 ureq 对象的同名方法会被误改；审计基线与 main 不一致。迁移后需全量编译+测试 |
| M-9 | 许可元数据不一致：仓库 `description` 仍写 **MIT**，`LICENSE` 与 `Cargo.toml` 为 **AGPL-3.0-only** | 仓库元数据 / `Cargo.toml:11` | 对外声明歧义，可能被误当作 MIT 使用。统一仓库描述 |
| M-10 | `next_id` 用 FNV-1a 由目录名派生包 id，无冲突检测 | `dpage/src/lib.rs:172-186` | 同名/哈希碰撞的包 id 冲突。加显式冲突处理或用内容摘要 |

## 5. Low

- STUN 属性步进用了错误的填充量：`crates/rtc/src/ice.rs:155`
  `offset += 4 + alen + (alen % 4);` —— 正确应为 `pad4(alen)`（`crates/rtc/src/turn.rs:176/275`
  用的是对的）。`alen % 4 == 1` 时少走 2 字节 → 后续属性错位、`XOR-MAPPED-ADDRESS` 解析失败。
- `crates/sfu/src/lib.rs:129`：`join` 开头无条件 `self.leave(peer)`，用他人 id 调用即可把对方踢下线
  （与 M-12 同源，接线前必须一起修）。
- `crates/admin/src/upstream.rs:127-160`：迁移 ureq 3 后手工判 `(400..600)`，3xx 会落入"成功"分支，
  语义边界需明确。
- `crates/limits` 名不副实：只导出常量（分页、文件名长度、sha256 hex 长度），没有请求级限流实现，
  容易造成"已有限流"的误判。
- CI（`.github/workflows/ci.yml`）有 `cargo deny check` 但无 `cargo audit`，也无覆盖率门槛与变异测试闸门；
  质量闸门（`scripts/quality-gate.py`）是结构统计，不是安全证明（issue #18 已指出同一问题）。

## 6. 抽样确认做得好的地方（非客套，均已在代码中回读）

- **用 vodozemac（Matrix Olm/Megolm）而非自研棘轮**，并自行补齐它明确留给宿主的三件事：
  128 条乱序窗口、重放拒绝、重复 pre-key 处理；`core/tests/e2ee.rs` 有交替方向 / 乱序 / 重放专项测试。
- **fail-closed**：`encrypt_dm_existing`（`e2ee.rs:400-421`）没有会话就报错，绝不退化明文
  （issue #4 的根因真正修掉了）。
- **钱包实现正确**：`transfer`/`fund`/`claim_redpacket` 都在**单事务**内完成，且用
  `UPDATE wallets SET balance = balance - ?1 WHERE user_id = ?2 AND balance >= ?1` 的原子写法 +
  `updated == 0` 判断 + CHECK 约束兜底（`wallet.rs:216-262`、`188-215`、`382-460`）。
  没有读-改-写竞态。
- 口令：`per-user salt` + PBKDF2 + **常量时间比较**（`auth.rs:53-68`）；会话列表只暴露 `sid`，
  永不返回 token（`sessions.rs:6-9`）。
- 动态 SQL 用 `Clause` + 绑定参数构造（`admin/src/read.rs:806-880`），未发现字符串拼接式注入。
- 静态资源 `/app` 有**路径越界回归测试**（`server/src/web_static.rs:253-274`），非 GET/HEAD 返回 405。
- 页包路径校验 `safe_rel_path` 放在 `unpack` 内部（"一处校验，所有消费者自动安全"），
  并有整包拒绝的回归测试（`dpage/src/lib.rs:100-124`、测试 `196-225`）。
- 全仓库日志/指标宏中**未发现** token / 口令 / 私钥泄漏。
- CI 覆盖面真实：Linux / Windows / macOS 三平台 + Android 出包 + Flutter + Tauri + 前端 wire 自测。

## 7. 建议修复顺序

1. **C-1**（签名 + 指纹）—— 信任模型的根，其他 E2EE 工作都建立在它之上；
2. **H-1 / H-4 / H-6**（鉴权与风控）—— 单个补丁即可闭合；
3. **H-5 / H-7 / H-8**（不可信输入的资源边界与外部凭据）—— 部署前必须处理；
4. **H-2 / H-3 / M 组**—— 随迭代清理。

## 8. 与既有 issue 的关系（增量）

已抽样复核确认修复有效、不再重复：
#3（QUIC/WS 自报身份 → 现由 `lookup_token` 统一鉴权，`server/src/lib.rs:371-390`）、
#4（第二条消息明文 → `encrypt_dm_existing` fail-closed）、#14（媒体下载鉴权，`media.rs` 按
好友/同会话校验）、#12（QUIC 证书 TOFU pin，`net/src/transport.rs:183-215`）。

本报告新增证据但未闭环的：#15（秘密聊天闭环，对应 M-2 / C-1）、#22（ureq3 迁移，对应 M-8）、
#23（审计总跟踪，本报告作为其输入）。
