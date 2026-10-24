# G02 交付说明（身份、关系、设备与加密安全）

分支：`feat/FP-G02`（7 张卡同一 PR，按卡分段提交，便于逐卡 review）。
任务单：[`docs/full-platform-delivery/teams/G02.md`](../teams/G02.md)。
集成请求：[`integration-requests/G02/`](../../../integration-requests/G02/README.md)。
迁移草稿：[`schema/proposals/G02/`](../../../schema/proposals/G02/README.md)。

## 一、做了什么

新增三个 workspace crate，把 G02 的规则从平台 HTTP 层里剥出来，**规则只有一份实现**：

| crate | 任务 | 内容 |
|---|---|---|
| `crates/features/identity` | G02-01 / 05 / 06 / 07 | 设备生命周期、会话与刷新轮换、step-up、限速、找回、验证码挑战、协议回执、扫码登录、服务端偏好、导出/注销作业 |
| `crates/features/relations` | G02-02 | 好友申请状态机、拉黑优先、私有偏好（备注/星标/标签）、群角色/邀请/踢人/解散/共同群、`RelationsPort` |
| `crates/features/security` | G02-03 / 04 / 05 | 每设备密钥包与原子认领、轮换与验证失效、本地密钥库（口令/恢复码/OS 密钥库三路包裹）、隐私与通知策略 |
| `crates/core/src/e2ee.rs` | G02-03 / 04 | 新增 `EncryptedEnvelope`、`Crypto::seal_envelope/open_envelope`、`seal_secret_blob/open_secret_blob`（纯新增，未改既有签名） |

设计上的三个取舍：

1. **不重写平台已有能力**。平台的 `sessions` / `friends` / `groups` / `prekey_bundles`
   表继续用，G02 只在它们之上补缺失的规则（刷新链、设备注册表、标签、群可见性、
   每设备密钥），表结构变更全部走 `schema/proposals/G02/`，由 G00 串行执行。
2. **跨域只走端口**。聊天/搜索要问"能不能发""共同群有哪些"，只能调
   `RelationsPort`；这样关系表的形状变化不会漏到别的域。
3. **测试用真实 SQL**。每个模块的测试都开一个真实 SQLite 内存库、跑真实 DDL 与
   真实 SQL，不用假存储；加密相关测试用真实 vodozemac 账户与真实 Argon2id。

## 二、验收证据（可复现）

```bash
cargo test -p wxlike-identity -p wxlike-relations -p wxlike-security --lib
cargo clippy -p wxlike-identity -p wxlike-relations -p wxlike-security -p wxlike-core --all-targets -- -D warnings
cargo fmt -p wxlike-identity -p wxlike-relations -p wxlike-security -p wxlike-core --check
python scripts/quality-gate.py     # 本组新增代码：0 长函数、0 魔法数字、0 跨文件重复窗口
```

覆盖矩阵（任务单要求的六类用例 + 重复提交）：

| 类别 | 覆盖点 |
|---|---|
| 成功 | 每张卡都有 happy path（登录→刷新→登出、扫码全流程、好友双向接受、认领密钥、密钥库开关、偏好同步、导出下载） |
| 空结果 | 空设备列表、空好友列表、空共同群、无一次性预密钥、无作业、空偏好 |
| 参数错误 | 设备 id/platform/标签、E.164、验证码格式、PKCE verifier、群名、标签数量、偏好键白名单 |
| 未授权 | 错受众令牌、已撤销设备、非收件人接受、非群主改角色、错误 verifier、锁定状态下的密钥库 |
| 超时/断网 | 过期会话、过期刷新令牌、过期挑战、过期二维码、过期导出链接（均失败封闭） |
| 取消 | 撤回好友申请、拒绝扫码、拒绝申请、作业失败标记 |
| 重复提交 | 重复注册设备、重复发申请、重复接受、重复发布密钥包、重复打偏好补丁、重复申请导出、重复消费验证码 |

## 三、逐卡状态

| 卡 | 代码 | 单元测试 | 集成验收 | 阻塞项 |
|---|---|---|---|---|
| G02-01 | 完成 | 通过 | 待接线 | 平台路由未挂载（归 G00） |
| G02-02 | 完成 | 通过 | 待接线 | 平台路由未挂载；G03 需改用 `RelationsPort` |
| G02-03 | 完成 | 通过 | 待接线 | 平台路由未挂载；G03 需按设备加密 |
| G02-04 | 完成 | 通过 | 待接线 | 客户端（G10/G12）未接 `SecretStore`；ffi 绑定待 G01 |
| G02-05 | 完成 | 通过 | 待接线 | 平台路由未挂载；作业 worker 归 G00 |
| G02-06 | 完成 | 通过 | **不达标** | 短信供应商 sandbox 证据缺失（需真实凭据） |
| G02-07 | 完成 | 通过 | **不达标** | 真实手机扫真实桌面码需 G10/G12 页面合入 |

**没有一张卡标 ACCEPTED。** 任务单明确"不得以 mock/toast 标完成"，而集成验收依赖
（G01-01、G10-06、G12-06、G07-01）尚未合入；G02-06/07 还缺真实供应商与真机证据。
代码与测试就绪，验收动作留给集成阶段。

## 四、给其他组的交接

* **G03（消息）**：发送前调 `RelationsPort::may_message`；E2EE 按
  `device_keys::claim_user` 的每设备结果逐个加密，信封带 `rotation_epoch`。
* **G05（内容/搜索）**：可见性用 `security::privacy::{profile_visibility, searchable_by}`，
  共同群用 `RelationsPort::shared_groups`。
* **G07（扫码入口）**：只解析 qr 登录前缀的 payload；challenge 不是 session token。
* **G10/G12（客户端）**：密钥库必须走 `SecretStore`（`migrate_legacy` 迁移旧裸密钥）；
  扫码登录的 `verifier` 只存内存，锁屏/退后台调 `lock()`。
* **G17（安全评审）**：`identity_consent`（合规留痕）、`relations_audit`（保留期）、
  SMS pepper 管理三处需要评审结论。

## 五、已知取舍与后续

1. **`prekey_bundles` 未退役**：多设备新表与旧的每用户表并存，等存量客户端全部
   升级后单独提退役迁移（不在 G02 范围）。
2. **群邀请走平台既有 `group_invites`**：G02 只实现"管理员拉人入群"，成员分享
   邀请链接仍是平台路径，未合并——避免与 G03 抢群相关文件。
3. **注销的跨域清理只是队列语义**：真正删数据要各域配合（`PURGE_DOMAINS`），
   法定保留记录由各域自行分域处置，G02 不做一刀切。
4. **恢复码丢失 = 数据不可解**：这是设计选择，不是缺陷；产品文案必须写清楚，
   禁止实现"客服帮你找回密钥"。
