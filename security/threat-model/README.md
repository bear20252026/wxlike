# 授权与滥用威胁模型（G17-03）

`docs/threat-model.md` 是一页纸，回答"E2EE 保护什么、不保护什么"。这份文档回答
它没覆盖的第二层：**登录之后，谁能对哪个对象做什么**，以及**做太多次会怎样**。

一句话概括本层的前提：**登录 ≠ 授权**。会话 token 只证明"你是某个人"，
不证明"这个群/这条会话/这个媒体对象属于你"。这一层出问题的方式几乎全是同一种——
某个 handler 忘了检查对象归属，或者检查了但没测。

## 1. 资产与参与者

| 资产 | 谁能读 | 谁能写 | 泄露后果 |
|---|---|---|---|
| 单聊/群聊消息体 | 会话参与者（端到端） | 发送者 | 最严重：即产品本身 |
| 会话元数据（`conv_id`/`head_seq`/成员表） | 参与者 | 服务端 | 通信关系侧信道（见 #230） |
| 好友关系与备注 | 属主 | 属主 | 社交图 |
| 媒体对象 | 公开对象=任何人；私有=属主/被引用者 | 属主 | 私密内容 |
| 会话与设备列表 | 属主 | 属主（下线设备） | 可冒用凭证（若回吐 token） |
| 钱包余额与流水 | 属主 | 仅通过受控命令 | 资金 |
| 管理面（举报/审核/对账） | 持 `x-admin-token` 者 | 同上 | 全站治理 |
| 预密钥目录 | 任何登录用户（认领语义） | 属主 | MITM（已有签名修复，CH-01） |

## 2. 五条信任边界

1. **客户端 ↔ 平台 HTTP**：客户端完全不可信；每个请求都要独立鉴权，不看"上一跳做过什么"。
2. **平台 ↔ 消息存储**：`conv` 由客户端给出，**不能**用形状（`d-` 前缀）推导权限。
3. **用户态 ↔ 管理态**：两种令牌不通用；管理面必须验 `x-admin-token`，且**先于任何 body 解析**。
4. **小程序页包 ↔ 宿主**：页包不可信（解码、执行预算、能力清单见 G17-04）。
5. **供应商回调 ↔ 平台**：回调按共享密钥对原始 body 验签，不信任表单字段。

## 3. 身份 → 对象 → 操作矩阵

矩阵是**从源码扫出来的**（155 条端点），不是手写清单：

```bash
cd tools/source-audit
python -m sourceaudit authz sync      # 源码新增端点 → 补进矩阵（保留人工字段）
python -m sourceaudit authz check     # 三方核对：源码 ↔ 矩阵 ↔ 用例
```

| 数据 | 位置 | 内容 |
|---|---|---|
| `authz-matrix.json` | 本目录 | 每行：method / path / handler / module / auth / object / scope / tests / public_reason |
| `../release-reviews/blockers.json` | 发布评审 | 已知偏差与阻塞（每条带 owner + issue） |

现状（2026-10-24）：**用户态 134 · 管理态 12 · 公开 9**，全部有证据（用例名或公开理由）。

判定口径（`auth` 列的来历）：

| 值 | 判定依据 |
|---|---|
| `user` | handler 签名里有 `Authed` 提取器 |
| `admin` | handler 体里调用 `require_admin(` / `admin_ok(`（或读 `x-admin-token`） |
| `public` | 两者都不是 —— **必须在矩阵里写 `public_reason`**，否则 `authz check` 直接红 |

反向约束（这是矩阵不会烂掉的原因）：

- 源码新增端点未登记 → `AUTHZ-UNREGISTERED`
- 矩阵残留已删端点 → `AUTHZ-STALE`
- 矩阵的 `auth` 与源码不一致 → `AUTHZ-AUTH-DRIFT`（想改鉴权先改代码）
- 私有端点没有用例 → `AUTHZ-NO-TEST`
- 引用了不存在的用例函数 → `AUTHZ-GHOST-TEST`

## 4. STRIDE 对照（本层）

| 威胁 | 具体形态 | 控制 | 证据 |
|---|---|---|---|
| **S**poofing | 用 B 的 token 打 A 的对象；管理面用普通 token | 每请求独立鉴权；对象归属校验；管理令牌隔离 | `authorization.rs` AUTHZ-001…008 |
| **T**ampering | 客户端注入 `user_id` 改余额归属；重放写命令 | 身份只从 token 取；幂等键绑 actor+参数摘要 | AUTHZ-010、AUTHZ-011 |
| **R**epudiation | 谁下的命令查不到 | 命令审计中间件（`X-Wxlike-Command` → `cmd.<id>`） | `crates/platform/src/audit_mw.rs` |
| **I**nfo disclosure | 非成员读群信息/媒体/收藏；非参与者读会话未读数；错误信息枚举字段名 | 归属校验；统一 401/403/404；**错误语义不透露内部结构** | AUTHZ-002/008/009；#230（已修复并回归）、#245（缺口） |
| **D**oS | 刷屏、好友枚举、验证码爆破、上传轰炸 | GCRA 限速（12 个动作、独立桶） | `abuse.rs` ABUSE-001…008、`abuse-limits.json` |
| **E**levation | 普通成员拉人/改公告；用户进管理面 | manager scope；管理令牌 | AUTHZ-003、AUTHZ-007 |

## 5. 滥用限额

完整表在 `abuse-limits.json`（阈值来源是 `crates/limits` 的常量，本文件只做说明）。
摘要：

| 动作 | 窗口 | throttle | deny | 备注 |
|---|---|---|---|---|
| Register | 1h | 50 | 200 | 按来源 IP，避免共享出口连坐 |
| Login | 15min | 5 | 12 | 按账号，拦撞库 |
| Send | 10s | 20 | 60 | 真人打字上限之内 |
| FriendRequest | 1h | 30 | 100 | 拦好友枚举 |
| ClaimKeys | 1h | 30 | 120 | 防一次性预密钥库存耗尽 |
| MediaUpload | 10min | 60 | 200 | 九宫格 = 9 个文件，窗口必须给足 |
| CallSignal | 10s | 100 | 300 | **ICE trickle 是合法突发**，阈值高一个量级 |

"不误伤"是被测的，不是被声明的：

- `normal_burst_stays_allowed`：低于 throttle 阈值必须全放行；
- `call_signal_trickle_is_not_throttled`：正常 ICE 突发不打回；
- `actions_do_not_share_buckets`：发消息刷爆不能把认领密钥一起废掉；
- `chat_spam_is_denied_then_recovers`：窗口滑过必须恢复（限速不是封号）。

## 6. 金融面单独评审（G06 域）

G17 只做**越权与重放**，不做账务正确性（那是 G06 的守恒/幂等测试）：

| 风险 | 本层要求 | 证据 |
|---|---|---|
| 读别人余额 | 余额只按会话身份返回，忽略 `user_id` 参数 | AUTHZ-010 |
| 重放转账/建群 | 同 `Idempotency-Key` 不产生第二笔 | AUTHZ-011（以建群为例，同一中间件路径） |
| 金额/收款人篡改 | 确认令牌必须绑参数摘要（G15-04） | 见 G15 交付 |
| 余额由缓存/浮点决定 | 不在本层断言；G06 用整数分 + 事务 | 待 G06 用例 |

## 7. 小程序面单独评审（G08 域）

本组只负责"宿主不因恶意页包崩溃/越权"的部分（详见 G17-04）：
解码上限、路径穿越、执行预算、能力清单缺失。页包签名与资源包完整性属 G08。

## 8. 隐私：最小权限与日志脱敏

| 要求 | 实现 | 证据 |
|---|---|---|
| 响应不返回凭证类字段 | `token` / `password` / `secret` / `private_key` / `ciphertext` 均不出现在 `/api/me`、`/api/sessions`、`/api/devices`、`/api/audit/self` | `audit_surfaces_do_not_leak_secrets` |
| 会话列表只给 `sid`，不给 token | `SessionRow` 无 token 字段（DB 里存的是 SHA-256 摘要） | 同上 + `crates/platform/src/sessions.rs` |
| 审计只记"谁/什么动作/什么目标/状态" | `detail = "status=200 method=POST"`，无 body、无 token | `crates/platform/src/audit_mw.rs` |
| 查询面最小权限 | 审计自查只看自己（`/api/audit/self`）；全量检索归 P6 adminx + RBAC | 同上 |

## 9. 已知缺口（不藏着）

| 编号 | 内容 | 归属 | 处置 |
|---|---|---|---|
| SEC-AUTHZ-012（已修复） | 信箱参与者归属 | G03 / #230 | PR #269 修复；独立用例已启用，覆盖 unread/read/sync |
| SEC-ADMIN-ORDER | 2 个管理端点的 body 校验先于令牌校验 → 未认证 422 且泄露字段名 | G14 / #245 | sweep 里以 `exempt` 登记（必须带 issue），修复后要求 401/403 |

历史问题均保存在 `security/release-reviews/blockers.json`；#230 记录关闭证据，#245 仍是 open 发布阻塞。

## 10. 怎么跑

```bash
# 授权矩阵
cd tools/source-audit && python -m sourceaudit authz check

# 越权 + 滥用 + 全端点 sweep（真实平台路由，不用 mock）
cargo test --manifest-path tests/security/Cargo.toml --test authorization --test abuse --test sweep

# 全套独立安全测试（没有忽略用例）
cargo test --locked --manifest-path tests/security/Cargo.toml
```
