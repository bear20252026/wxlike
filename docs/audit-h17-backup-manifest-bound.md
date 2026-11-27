# H-17：backup manifest 未校验长度即分配

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `b056598`。

审计原文：`AUDIT-60895e18.md:245-250`。`read_header` 读到包内声明的 `manifest_len`
后直接 `vec![0u8; manifest_len as usize]`，没有检查剩余包长，也没有业务上限。
14 字节的恶意包声明约 4 GiB manifest 就能让导入进程 OOM/abort。

## 修复

`crates/features/media/src/backup.rs`：

- 新增 `BACKUP_MANIFEST_MAX_BYTES = 64 * MIB`，解析前检查声明长度不超过业务上限。
- `read_container` 在打开包后计算 `manifest_room = file_len - BACKUP_HEADER_LEN -
  BACKUP_TRAILER_LEN`，并传给 `read_header`。manifest 必须给 32 字节 trailer 留空间。
- `read_header` 在 `vec![]` 之前做两个检查：
  - `manifest_len > BACKUP_MANIFEST_MAX_BYTES` → `Tampered`；
  - `manifest_len > manifest_room` → `Tampered`。
- 两个失败都在分配前发生；内存峰值与包内声明长度解耦。

## 证据

`cargo test -p wxlike-media --test g04 backup_roundtrip_preserves_ids_and_rejects_tampering`：

- 在原有 roundtrip/tamper 流程后追加 14 字节 `huge-manifest.wxbk`：magic `WXBK`、
  version 1、`manifest_len = u32::MAX`；
- `verify_backup` 返回 `MediaError::Tampered`，错误文本包含 `manifest`；
- 没有尝试分配 4 GiB。

## 边界

上限 64 MiB 是按 `BACKUP_MAX_ENTRIES=100_000` 的元数据量留的余量；如果未来允许
更多条目，需要同步调高上限并补 near-limit 包测试。该检查只覆盖 manifest 解析；
payload 的物理大小仍由 `BACKUP_MAX_BYTES` 和逐 entry size 限制。
