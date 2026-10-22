# 全部更改项清单（第四轮审计）

分支：`fix/audit-round4`（基线 main `62124c5`）｜8 commit｜17 文件｜+994/−98
逐项审查：`git show <commit>`；整批：`git diff 62124c5..fix/audit-round4`

---

## CH-01 预密钥包端到端签名

- **变更编号**：CH-01（commit `e40a9ec`）
- **文件**：`crates/core/src/e2ee.rs`、`crates/core/tests/e2ee.rs`、`crates/platform/src/keys.rs`、
  `crates/platform/src/db.rs`、`crates/platform/tests/keys.rs`
- **类型**：安全修复（协议结构扩展，向后兼容见下）
- **关联风险**：AR-1（Critical）
- **摘要**：`PreKeyBundle` 增加两层 Ed25519 签名（`signature` 绑定 user_id+identity；
  `one_time_key_signatures` 逐把 OTK）；`encrypt_dm` fail-closed 验签；`publish_keys`
  校验曲线点形态+验签；`prekey_bundles` 补 `signature` 列迁移（旧行默认 ''，客户端拒绝，
  属主重发即恢复）；`otks` 列改存 `[key, sig]` 对（认领原子弹出）；`Device::safety_number()`
  带外安全码；6 条对抗测试（换身份键/无签名/拼接 OTK/长度不匹配/子集验签/安全码）。
- **验证**：core e2ee 25/25；platform keys（当批）11/11
- **回滚**：`git revert e40a9ec`（DB 列有 DEFAULT，回滚后旧库可直接用）
- **剩余风险**：桌面端未展示 safety_number、无身份变化告警（#25 遗留）；迁移窗口内旧 bundle
  失效需重新 publish（已文档化）

## CH-02 入口资源/凭据边界三连

- **变更编号**：CH-02（commit `f53891d`）
- **文件**：`server/src/lib.rs`
- **类型**：安全修复（行为收紧：faucet 从"环境变量即可用"变为"环境变量+管理令牌+额度"）
- **关联风险**：AR-2（High；H-1/H-9/H-10 三面）
- **摘要**：`/api/dev/fund` 要求 `x-admin-token`（`admin_ok`），未配 `WXLIKE_ADMIN_TOKEN`
  时路由不挂载并打印醒目警告；单笔上限 100 万分；fund 流水+tracing 审计。`/ws` upgrade 设
  `max_message_size/max_frame_size = MAX_FRAME(8MiB)` 对齐 QUIC 路径。`url_consumer` 改为
  URL 的 SHA-8 + pid，Redis 口令不再进入任何对外可见标识。
- **验证**：`cargo check -p wxlike-server` 0 错 0 警；组合态见 CH-09
- **回滚**：`git revert f53891d`
- **剩余风险**：WS 理想形态是 upgrade 前鉴权（#26 遗留）；faucet 额度是单笔上限而非单日额度

## CH-03 不可信输入执行边界

- **变更编号**：CH-03（commit `cf8bd82`）
- **文件**：`crates/dpage/src/lib.rs`、`crates/container/src/engine.rs`、
  `crates/container/tests/engine_limits.rs`（新增）
- **类型**：安全修复 + 新增测试文件
- **关联风险**：AR-4（High；H-5/H-7/M-3）
- **摘要**：dpage `unpack` 用 64MiB 封顶流式解压替换 `decode_all`（`take(MAX+1)` 探测溢出，
  拒绝而非截断）；Boa 引擎 `set_loop_iteration_limit(1_000_000)`；`call_global` 从
  `format!` 拼 JS 源码 eval 改为属性查找 + `JsValue::from_json` 数据传参。4 条边界测试：
  死循环抛错、50 万次循环通过、恶意函数名不执行、恶意字符串参数原样往返。
- **验证**：dpage 5/5（含 200MiB 炸弹拒绝）；container 5/5+4/4
- **回滚**：`git revert cf8bd82`
- **剩余风险**：Boa 0.20 无内存配额接口（#28 遗留）；循环限额为每循环语句预算，极端页包
  仍可消耗较长 CPU（毫秒级→秒级，不再无限）

## CH-04 注册即 argon2id + PBKDF2 兜底 600k

- **变更编号**：CH-04（commit `a14bdb6`；注：该 commit 原落在 `feat/ureq3-migration` 为
  `d4806d7`，因并行进程分支切换，已 cherry-pick 回本分支并还原对方分支指针——过程见
  AUDIT_REPORT §2）
- **文件**：`crates/platform/src/auth.rs`、`crates/platform/src/users.rs`、
  `crates/platform/src/security.rs`、`crates/platform/tests/api.rs`
- **类型**：安全修复 + 契约测试更新
- **关联风险**：AR-3（High；H-3）
- **摘要**：`register` 从 PBKDF2@options 改为直接 `hash_argon2id`（消除文档-实现漂移，
  与登录升级/恢复重置统一）；`DEFAULT_ROUNDS` 100k→600k（OWASP 基线，遗留行校验兜底）；
  透明升级测试改写为"注册即 argon2id + 人造遗留 pbkdf2 行首登升级"双路径。
- **验证**：platform 全套件绿（约 6 分钟；该次运行树缺 AR-1，组合态由 CH-09 重跑覆盖）
- **回滚**：`git revert a14bdb6`（老库 pbkdf2 行不受影响；回滚后新注册回到 PBKDF2）
- **剩余风险**：`PlatformOptions.pbkdf2_rounds` 成为遗留字段未删除（admin 测试夹具在他人
  在途文件中，本轮不动；删除时机随 #22 合入）

## CH-05 OTK 认领关系门 + 限流

- **变更编号**：CH-05（commit `7d21c72`；含后补 `ff4bef4`+`5826038` 两笔测试 commit）
- **文件**：`crates/risk/src/lib.rs`、`crates/platform/src/keys.rs`、
  `crates/platform/tests/keys.rs`、`crates/risk/tests/engine.rs`
- **类型**：安全修复 + 公共 API 扩展（`risk::Action` 新变体 `ClaimKeys`、
  `Platform::may_claim_keys_of`；**行为契约变更：陌生人认领从 200 → 404**）
- **关联风险**：AR-2（High；H-4）
- **摘要**：认领前校验关系（本人/好友/同群；同群非好友放行——群聊 sender key 走 1:1 Olm）；
  陌生人 404（不做存在性探测）；`Action::ClaimKeys` 1h 窗口 Throttle=30/Deny=120，成功才
  计数；测试经真实 HTTP 好友流程建立关系，新增陌生人 404 与 35 轮补货认领循环第 30 轮后
  必然 429 两条对抗用例。`ff4bef4` 曾提交一条失败测试（误解 check/record 语义），
  `5826038` 修正——如实保留两笔，不隐藏过程。
- **验证**：risk 8/8；platform keys 13/13
- **回滚**：`git revert 5826038 ff4bef4 7d21c72`（逆序）
- **剩余风险**：库内嵌调用方（非 HTTP）不经此门（与 media 鉴权同款边界，进程内视为可信）；
  阈值是否适合真实负载需观察后调整（配置在 `RiskConfig`，可调）

## CH-06 会话令牌哈希入库

- **变更编号**：CH-06（commit `bd2fcc1`）
- **文件**：`crates/platform/src/auth.rs`、`crates/platform/src/lib.rs`、
  `crates/platform/src/sessions.rs`、`crates/platform/tests/api.rs`
- **类型**：安全修复（存储格式变更，**旧会话全部失效需重登录**——研究阶段既定口径）
- **关联风险**：AR-3（High；H-2）
- **摘要**：`sessions.token` 改存 `sha256(token)`（复用 `core::msgmeta::sha256_hex`）；
  六个读写点摘要比对：INSERT / lookup / revoke / sessions_of 的 current 标记 /
  **revoke_other_sessions 的保留判断**（此处漏改会把当前会话一并删掉——本轮实现中真实
  发生过，被 `session_tokens_are_stored_as_digests` 测试拦截后修正）/ touch_session。
- **验证**：platform api 17/17
- **回滚**：`git revert bd2fcc1`（回滚后旧行明文比对恢复，令牌重新明文落库）
- **剩余风险**：摘要不可逆，服务端无法主动"列出有效令牌"（本来也不该有该能力）；
  `Authed.token` 仍在内存/请求头中流转（属运行态必需）

## CH-07 / CH-08 测试补充（随 CH-05 记录）

- `ff4bef4`：risk 引擎测试的 `RiskConfig` 字面量补 `claim_keys_*` 两字段 + ClaimKeys
  契约测试（**当笔含一条失败测试，下一笔修正**——两笔都保留以便审查真实过程）
- `5826038`：修正该测试的 check/record 语义（check 只裁决不计数；record 落窗口），
  risk 8/8 全绿

## CH-09 组合态终验（不改代码）

- **命令**：
  ```bash
  git checkout fix/audit-round4   # 确认分支（并行进程曾切换）
  cargo test -p wxlike-core -p wxlike-platform -p wxlike-server -p wxlike-risk \
             -p wxlike-dpage -p wxlike-container -p wxlike-net -p wxlike-message
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --check
  ```
- **结果**：见下方"终验记录"（交付时以实际输出回填，失败如实报告）
- **说明**：`wxlike-admin`、`wxlike-client`、`wxlike-ffi`、`wxlike-video` 的测试文件在
  他人在途的 ureq3 迁移中（工作区脏文件），本轮不对其运行结果背书；`server` 测试中涉及
  上述脏文件的部分同理（server/Cargo.toml 的 dev-dependency ureq 已被在途迁移改为 v3）。

---

## 未做但已开 issue 的变更（决策记录）

| issue | 内容 | 不做的理由（本轮） |
|---|---|---|
| #29 | 风控中间件化 | 全部写接口统一改造，应独立 PR + 对抗测试集；本轮以设计草案交付 |
| #30 | 秘密聊天实时链路 TTL | 协议面下沉 net 投递判定，需与 #15 验收清单对齐 |
| #31 | TURN 短期凭据 | 涉及 server API + 客户端 + coturn 三方协议变更 |
| #32 | 技术债组 M/L | 见报告 §8 排期；M-2 需产品决策（口令派生 vs OS 钥匙串） |

## 终验记录（2026-10-22 实际运行回填）

| 命令 | 结果 |
|---|---|
| `cargo test -p wxlike-core -p wxlike-risk -p wxlike-dpage -p wxlike-container -p wxlike-message` | **123 passed / 0 failed** |
| `cargo test -p wxlike-net` | **99 passed / 0 failed** |
| `cargo test -p wxlike-platform`（全套件，含组合态 AR-1+H-2+H-3+H-4 交互） | **全绿**（电池日志可见 27 个套件 ok：unit 21、api 17、keys 13、friends 17、groups 22、moments 17、wallet/p4_ops 17 等；schema_snapshot 初跑失败见下） |
| `cargo test -p wxlike-server` | 全绿，**除 `secret_enforcement.rs` 2/3 失败——归因：他人在途 ureq3 迁移的测试助手缺口（`post()` 未像 `get()` 那样改走 `http_status_as_error(false)` 的 agent），非本轮变更所致**（本轮不触及转发/秘密聊天路径；取证与修法已评论到 issue #22） |
| `cargo clippy --workspace --all-targets` | **0 error / 0 warning**（含在途脏文件） |
| `cargo fmt --check` | **通过**（本轮文件已 rustfmt；全仓亦无 diff） |

**终验期间发现并处理的第 9 项变更**：schema 快照机制按设计拦住了 AR-1 加列后漏同步的
`prekey_bundles.signature`（`2b56d1f`，快照 diff 恰一行）——这正是该机制作者预期的
"改表必须让评审看到"行为，属机制生效而非缺陷。

**过程事故（如实记录）**：`ff4bef4` 提交过一条失败测试（误解 risk 引擎 check/record 语义），
下一笔 `5826038` 修正；两笔均保留供审查。
