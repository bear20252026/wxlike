# P4 交付报告：运营合规 + 资金账本 + 通话房间

> 对应合同第五条 P4（4 周 / 验收门槛：账本守恒 + 对账演示、管线演示、测试 ≥18）。
> 本文是 P4 的**三件套索引**：代码位置、验收命令、设计边界。

## 1. 交付清单

| 合同条目 | 交付物 | 代码位置 | 测试 |
|---|---|---|---|
| 风控/审核/举报三层 | 三个独立 crate + 平台接入面 | `crates/risk` · `crates/moderation` · `crates/report` · `crates/platform/src/{trust,report}.rs` | 26 |
| 发送实时拦截 | `DeliveryPolicy::send_gate` 帧级闸门 | `crates/net/src/policy.rs` · `mq_hub.rs` · `server/src/lib.rs` | 2 |
| 通话房间制 + 会议管控 | 名册/静音/踢人/锁定/录制/移交 | `crates/rtc/src/room.rs` | 13 |
| 对账任务 | 三条核对线 + 定时任务 + 运行记录 | `crates/platform/src/reconcile.rs` | 4 |
| 找回 + captcha | 一次性恢复码 + 算术挑战 + 来源限流 | `crates/platform/src/recovery.rs` | 5 |
| 秘密聊天完整版 | 会话开关/消失消息 TTL/安全码核对 | `crates/platform/src/secret.rs`（加密在 `crates/core/src/e2ee.rs`） | 2 |
| 设备管理页 | 会话列表/远程下线/一键退出其他设备 + UI 页 | `crates/platform/src/sessions.rs` · `ui/src/ui.ts` · `ui/src-tauri/src/lib.rs` | 2 |
| 红包卡片回写（ADR-5） | ephemeral 通道原地更新 | `crates/platform/src/wallet.rs`（上期） | 3 |
| 2FA / argon2id（ADR-3） | TOTP + 透明升级 | `crates/platform/src/security.rs`（上期） | 6 |

**P4 新增测试合计 58 项**（合同下限 18）。

## 2. 验收命令

全部命令在仓库根目录执行；`cargo` 命令均可用 `--release` 复现性能结论。

### 2.1 三层治理（风控 / 审核 / 举报）

```bash
# 单元层：滑动窗口阈值、变形绕过、工单状态机
cargo test -p wxlike-risk -p wxlike-moderation -p wxlike-report

# 平台层：注册挑战、登录锁定、审核挂起与放行、举报全流程、管理面鉴权
cargo test -p wxlike-platform --test p4_ops

# hub 层：被拒/被限速的帧不落库、不扇出（默认实现仍放行，P3 行为不变）
cargo test -p wxlike-net --test send_gate
```

HTTP 演示（服务端 `cargo run -p wxlike-server`，默认 `127.0.0.1:8099`）：

```bash
# 1) 取人机挑战（前 3 次同来源注册会返回 428）
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/captcha
# {"challenge_id":"...","question":"7 + 3 = ?","expires_in_ms":300000}

# 2) 带挑战注册
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/register \
  -H 'content-type: application/json' -H 'x-forwarded-for: 10.0.0.9' \
  -d '{"name":"alice","password":"hunter2","device_name":"Pixel 8",
       "captcha_id":"<id>","captcha_answer":"10"}'

# 3) 内容自查（不发消息，只看判定）
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/moderation/check \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"text":"加我微信 带你刷单"}'
# {"outcome":"hold","labels":["ad"],"severity":"review"}

# 4) 举报 → 分诊 → 处置 → 结案（管理面需 x-admin-token，见 WXLIKE_ADMIN_TOKEN）
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/reports \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"target_kind":"user","target_id":"<uid>","reason":"spam","detail":"刷屏"}'
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/admin/reports/<tid>/triage \
  -H 'x-admin-token: s3cret' -H 'content-type: application/json' -d '{"note":"受理"}'
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/admin/reports/<tid>/act \
  -H 'x-admin-token: s3cret' -H 'content-type: application/json' \
  -d '{"action":"mute","duration_ms":3600000,"note":"禁言1小时"}'
# 注：`action` 是平铺的（内部标签枚举的原生形状）。旧写法
# `{"action":{"action":"mute",...}}` 仍然接受，但新代码请用平铺。
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/admin/reports/<tid>/close \
  -H 'x-admin-token: s3cret' -H 'content-type: application/json' -d '{"note":"已通知"}'
```

### 2.2 账本守恒与对账

```bash
# 100 并发转账分毫不差 + 对账三条线
cargo test -p wxlike-ledger --test conservation
cargo test -p wxlike-platform --test p4_ops reconcile

# 手动触发一轮对账（管理令牌）
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/admin/reconcile \
  -H 'x-admin-token: s3cret'
# {"at_ms":...,"users_checked":2,"wallet_sum":9700,"escrow":300,
#  "supply":10000,"funded_total":10000,"mismatches":[],"ok":true}
```

对账三条线：逐用户 `balance == SUM(带符号流水)`、`供给 = 钱包 + escrow == 发行量`、
红包算术（`claimed ≤ count`、`claimed_total ≤ total`、状态与笔数一致）。
不通过时写 `reconcile_runs` 并在审计日志留 `wallet.reconcile` 事件。

### 2.3 通话房间

```bash
cargo test -p wxlike-rtc --test room
```

覆盖：容量、锁门、静音（本人/主持人）、踢人（不可踢主持人、管理员不可互踢、
被踢者禁入）、录制仅主持人、主持人离开移交（管理员优先 → 最早加入）、
转让主持后原主持人降级、举手队列按加入序。

### 2.4 找回 / 设备管理 / 秘密聊天

```bash
cargo test -p wxlike-platform --test p4_ops recovery
cargo test -p wxlike-platform --test p4_ops sessions
cargo test -p wxlike-platform --test p4_ops secret
cargo test -p wxlike-platform --test p4_ops safety
```

```bash
# 设备列表（含推送令牌）
curl -s --noproxy '*' http://127.0.0.1:8099/api/sessions -H "authorization: Bearer $TOKEN"
# 下线指定设备 / 一键退出其他设备
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/sessions/<sid>/revoke \
  -H "authorization: Bearer $TOKEN"
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/sessions/revoke-others \
  -H "authorization: Bearer $TOKEN"
# {"revoked":2}

# 恢复码：登录态生成（只显示一次）→ 离线重置密码
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/recovery/code -H "authorization: Bearer $TOKEN"
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/recovery/reset \
  -H 'content-type: application/json' \
  -d '{"name":"alice","recovery_code":"xxxxx-xxxxx-xxxxx-xxxxx","new_password":"newpass"}'

# 秘密聊天：开关 + 消失消息 TTL（0/30s/5m/1h/1d/7d）
curl -s --noproxy '*' -X PUT http://127.0.0.1:8099/api/secret/d-<a>~<b> \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"enabled":true,"ttl_ms":3600000}'
# 安全码（60 位，两端一致；对方换设备即变化）
curl -s --noproxy '*' http://127.0.0.1:8099/api/secret/<peer>/safety-number \
  -H "authorization: Bearer $TOKEN"
```

## 3. 设计边界（评审重点）

1. **三层不互相调用**（ADR-20）：`risk` 是纯内存滑动窗口，零 I/O；`moderation`
   是纯文本/媒体判定，无存储；`report` 是状态机，不判内容。平台层只做编排。
2. **风控失败 fail-open**：`send_gate` 内部出错放行并记日志——拦截增强不能
   变成可用性单点（与 P3 投递策略同一约定）。审核硬违规仍然拒绝。
3. **挂起是真挂起**：`Hold` 的消息进 `moderation_queue`，hub 丢弃该帧；放行
   接口返回原文，由调用方重新投递。**当前缺口**：hub 侧没有 NACK 帧，发送方
   看不到"已被挂起"，客户端提示归 P5（`MsgBody::Custom{kind:"moderation.held"}`）。
4. **恢复码无邮箱**：内部测试模式不接 SMTP；接口形状与生产一致，接邮件只需
   在 `issue_recovery_code` 前加一步投递。
5. **人机校验是算术题**：挡脚本够用，不挡专业打码平台；替换点已隔离在
   `issue_captcha` / `solve_captcha`。
6. **管理面默认关闭**：`PlatformOptions::admin_token = None` 时所有 `/api/admin/*`
   返回 403；P6 的 adminx 接手前不暴露。
7. **房间不写消息序列**：房间状态纯内存结构（`Room`），不占群信箱、不产生
   序列号——ADR-7 的"通话独立于消息管道"在类型层面就成立。

## 4. 遗留与下一步

| 项 | 归属 | 说明 |
|---|---|---|
| 审核放行后的重投递 | P5 | 需要 hub 增加 `Nack`/`Held` 控制帧 |
| 多模态审核模型 | P6 | `MediaJudge` trait 已留，接 Lexfence 式微服务 |
| 举报处置动作落地 | P6 | 禁言/封禁/删内容目前只写工单，需回调各业务服务 |
| 房间与媒体栈对接 | P5 | `Room` 名册已就绪，str0m SFU 分派属直播/群通话 |
| 对账告警通道 | P6 | 目前写审计 + stderr，接看板/告警 |
