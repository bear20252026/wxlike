# 回滚

## 服务端（镜像）

1. 从 `release-manifest.json` 找到上一版 digest；
2. 修改 `deploy/production/wxlike.env` 的 `WXLIKE_IMAGE` 为
   `ghcr.io/<repo>/wxlike-server@sha256:<previous>`；
3. `docker compose -f containers/server/compose.prod.yml --env-file deploy/production/wxlike.env up -d`；
4. 等待 `/health` 200，跑 `POST /api/admin/reconcile` 要求 `ok=true`；
5. 观察 10 分钟错误率/延迟。

旧镜像必须仍在 registry 中；不要用 `latest` 回滚，也不要重新构建“旧代码”
（依赖解析可能已变化）。

## 数据库

当前迁移是**向前执行**的 SQLite 迁移，没有自动 down。回滚应用前确认：

- 新版本是否写入了旧版本不认识的行/列；
- 若有破坏性迁移，优先恢复**发布前备份**（`backup-restore.md`），
  而不是启动旧二进制碰运气。

恢复会丢失备份点之后的写入，必须记录 RPO 窗口并通知用户。

## 桌面端

Tauri updater 只在签名验证通过后替换，失败保留旧版本。手工回退：安装上一版
安装包；数据目录 `dev.wxlike.ui` 不随版本删除。不要删除签名私钥——否则无法再
发布更新。

## 移动端

- Android：Play 控制台暂停分阶段发布，或上传更高 versionCode 的修复版；
  已安装用户不会自动降级。
- iOS：TestFlight/商店停止分阶段发布；签名证书/描述文件回滚需同步更新
  `mobile-release` secret。

## 回滚也算发布

回滚后更新 `release-manifest.json` 的 `rollback_to` 记录（下一次发布清单应包含
上一 digest），并在事故记录中写明回滚原因与验证结果。
