# 移动端签名与设备证据

这份目录回答三件事：怎么签、密钥放哪、凭什么说“真机验过”。

## 现状（2026-10-24）

| 平台 | 云构建 | 签名 | 真机证据 |
|---|---|---|---|
| Android | `ci/jobs/mobile/job.yml`（debug key 的 release 构建） | `android-release` 在 `mobile-release` 环境签 | **缺**，见 `device-matrix.md` |
| iOS | **不可构建**：`apps/mobile/ios` 不存在 | `ios-release` 会显式失败 | 缺 |

因此 `ci/jobs/policy/known-gaps.json` 保持 `MOBILE-DEVICE-EVIDENCE` 打开，
发布门禁失败。补上真机证据和 iOS 工程前，不要把这个缺口改成 `closed`。

## 签名密钥

- Android keystore、iOS 证书与描述文件只存在于 GitHub Actions 的
  `mobile-release` 环境保护环境 secret 中，以及运维的密钥管理器里。
- 仓库只保存**变量名和用途**（`secret-inventory.md`），不保存值、不保存
  keystore 文件、不把 base64 写进日志。
- 发布 job 把临时文件写入 `$RUNNER_TEMP`，结束即销毁；`key.properties`
  只在该 job 的工作区存在，删除在 job 结束时由 runner 回收。

## 证据判定

- Android：`check_apk.py` 用 `aapt` 读 versionName、`apksigner` 读签名 DN，
  发布构建若仍是 `Android Debug` 直接失败；同时要求混淆构建的 `.symbols`。
- iOS：`check_ipa.py` 解包 IPA 读 Info.plist 版本，`codesign` 必须是
  `Apple Distribution`；ad-hoc/development 签名失败。
- 真机：`check_device_evidence.py` 校验 `device-evidence.schema.json`，要求
  每台矩阵设备的 push/call/WebView/权限都有带时间、构建 SHA、结果的文件，
  `TBD`/`example` 之类占位值会被拒绝。

## 轮换

1. 新密钥在密钥管理器生成，旧密钥保留 90 天以便回滚。
2. 更新环境 secret，跑一次 `release.yml` 的签名 job。
3. 验证新签名安装包能覆盖安装旧版（Android 同 applicationId 且签名一致）。
4. 在 `secret-inventory.md` 记录轮换日期与负责人（不记录值）。
