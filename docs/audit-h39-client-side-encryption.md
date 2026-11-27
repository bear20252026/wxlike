# H-39：ClientSide 加密可配但 payload 仍明文

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `02088ae`。

审计原文：`AUDIT-60895e18.md:423-428`。`ExportBody.encryption` 接受
`ClientSide`，但写包时 payload 仍是原字节；manifest 却声称已做客户端加密。
恢复端如果信任这个标记，会以"已加密"为前提处理明文。

## 修复口径

在真正实现客户端加密 envelope 之前，直接拒绝 `ClientSide`，不生成包：

- `backup.rs::ensure_supported_encryption`：`None` 通过；`ClientSide` 返回
  `MediaError::Unsupported`，错误文案明确说明"拒绝把明文 payload 标成加密"。
- `plan_backup_export`、`export_backup` 在建计划/写包前拒绝。
- `verify_backup`、`import_backup`、`restore_backup_keys` 在读取 manifest 后
  同样拒绝，避免历史伪造包或手工改 manifest 的包被当作加密包处理。
- HTTP 层 `http.rs` 无需另写分支：`ExportBody.encryption` 原样传给 service，
  `Unsupported` 由既有错误映射返回 422。

`EncryptionMeta::client_side()` 构造器保留，供未来真实 envelope 使用；现在它
只能用于触发明确拒绝，不再是可用导出模式。

## 证据

`cargo test -p wxlike-media --test g04`：28 passed。

- 新增 `client_side_encryption_is_rejected_when_payload_is_plaintext`：
  `plan_backup_export` 与 `export_backup` 都返回 `Unsupported`。
- 原 backup roundtrip 改用 `EncryptionMeta::plaintext()`；明文包调用
  `restore_backup_keys` 报错且 escrow port 未收到 key ref。

## 边界

- 这是"拒绝错误声明"，不是"实现客户端加密"。真正的 ClientSide envelope
  （每对象加密、nonce、密钥封装、manifest 绑定密文摘要）仍是未交付能力。
- 仅拒绝导出/校验/导入入口；数据库中已有的历史 `ClientSide` 备份记录不会
  被删除，但再次 verify/import 会被拒绝并要求重新导出。
- 客户端若继续传 `ClientSide`，会得到 422 Unsupported，而不是静默明文包。
