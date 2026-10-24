# G02 迁移提案（身份 / 关系 / 安全）

**执行权在 G00。** 本目录只有草稿与回滚说明，没有迁移编号——编号由 G00 统一分配
后串行合入，避免并行组各自编号撞车。

## 文件

| 文件 | 覆盖任务 | 新建表 | 改动既有表 |
|---|---|---|---|
| `identity.sql` | G02-01 / 05 / 06 / 07 | 11 张 `identity_*` | `sessions` 补列 `audience` |
| `relations.sql` | G02-02 | `friend_tag` / `group_meta` / `relations_audit` | `friends` 补列 `starred` |
| `security.sql` | G02-03 / 04 | `security_device_key` / `security_device_verify` / `security_rotation` | 无 |

全部语句幂等（`IF NOT EXISTS`），两处补列必须先查 `PRAGMA table_info(...)` 再
`ALTER`——SQLite 没有 `ADD COLUMN IF NOT EXISTS`。crate 内的 `migrate()` 已经这么做
（`crates/features/identity/src/lib.rs::ensure_sessions_audience`、
`crates/features/relations/src/lib.rs::ensure_friends_starred`），G00 可直接复用同
逻辑，也可以把补列写进正式迁移脚本。

## 回滚兼容说明

**原则：新表可以整体丢弃，既有表的补列不删。** 理由：SQLite 删列需要重建表
（`ALTER TABLE ... DROP COLUMN` 仅在 3.35+ 且受约束限制），重建在生产库上是
高风险操作；而多出来的列对旧代码完全无害（旧代码不 `SELECT *`，写入走显式列）。

| 场景 | 回滚动作 | 数据影响 |
|---|---|---|
| 回滚 G02-01 | `DROP TABLE identity_refresh / identity_step_up / identity_throttle / identity_device / identity_reset_token` | 用户需重新登录一次（会话表未动）；设备列表丢失，重登后重建 |
| 回滚 G02-05 | `DROP TABLE identity_pref / identity_job / identity_export_link` | 服务端偏好回落到产品默认值；本地偏好（主题等）不受影响，因为它从来不在服务端 |
| 回滚 G02-06 | `DROP TABLE identity_challenge / identity_consent` | 进行中的验证码全部失效（用户重发即可）；协议同意历史丢失，**需业务确认**后再回滚（合规要求留痕） |
| 回滚 G02-07 | `DROP TABLE identity_qr` | 进行中的扫码登录失效，重开二维码即可 |
| 回滚 G02-02 | `DROP TABLE friend_tag / group_meta / relations_audit` | 标签丢失；群可见性回落为"全部公开"（`group_meta` 缺失时 `LEFT JOIN` 走 `public` 默认）；解散标记丢失 → 已解散的群会重新可见，**回滚前必须先导出 `group_meta.dissolved_at` 清单** |
| 回滚 G02-03 | `DROP TABLE security_device_key / security_device_verify / security_rotation` | 多设备密钥目录丢失；平台旧 `prekey_bundles`（每用户一份）仍在，单设备路径可继续用 |
| `sessions.audience` | 保留列不删 | 旧代码不读该列，`DEFAULT 'user'` 语义正确 |
| `friends.starred` | 保留列不删 | 同上；旧代码不读，默认 0 |

## 与平台旧表的关系

`prekey_bundles`（每用户一份）**不删**。多设备上线期间两条路径并存：新客户端发布
到 `security_device_key`，老客户端仍读 `prekey_bundles`。等 G03（消息）切到
`keys.claim` 新接口、且存量客户端全部升级后，再单独提退役迁移——退役不在 G02 范围。

## 索引与容量

* `identity_refresh` 按 `sid` / `(user_id, device_id)` 建索引：登出与设备下线都是
  索引命中，不做全表扫；
* `identity_challenge` 按 `(phone_hash, purpose, created_at)` 建索引：限速与"最近一条
  挑战"都走它；
* `relations_audit` 只增不改，建议纳入 G00 的对账/归档作业（保留期由合规定）。
