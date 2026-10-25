# 桌面端（Tauri v2）打包与更新

桌面端是 UI（TypeScript + esbuild）套在 Tauri v2 外壳里，源码在 `ui/src-tauri`。
仓库里的 `ui/src-tauri/tauri.conf.json` 保持 `bundle.active=false`，**发布用
override 打开**，避免日常 `cargo test` 误触发打包：

```bash
npx @tauri-apps/cli@2.9.0 build \
  --config ci/jobs/desktop/tauri.bundle.override.json \
  --bundles deb,appimage          # Linux；Windows 用 nsis,msi，macOS 用 app,dmg
```

CI 在三个 runner 上各跑一次，产物与 `manifest-<platform>.json` 一起上传为
`desktop-<platform>` artifact（保留 14 天）。CI 产物**未签名**，不能发布。

## 签名与更新清单

发布 job 在 `production-release` 保护环境中执行：

1. 注入 `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
   和解锁密码；私钥永不出现在仓库、日志或 artifact 中。
2. 用同一 override 打包，`createUpdaterArtifacts=true`，产出 `.sig`。
3. `ci/jobs/desktop/make_update_manifest.py --require-signature` 为每个平台
   生成清单；缺签名直接失败，不会生成“可发布但未签名”的清单。
4. `ci/jobs/desktop/check_updater.py --verify` 用公钥验证每个 artifact 的
   minisign 签名。

`check_updater.py --self-test` 在普通 CI 里生成临时密钥、签名、验证，并篡改
内容期望拒绝——证明“验签确实会拦住坏包”，而不是只检查文件存在。

## 平台与安装方式

| 平台 | 产物 | 安装 |
|---|---|---|
| Windows x86_64 | NSIS `.exe`、MSI | 双击安装；静默 `wxlike_<ver>_x64-setup.exe /S` |
| macOS arm64 | `.app`、`.dmg` | 拖入 Applications；首次启动需在“隐私与安全性”放行 |
| Linux x86_64 | `.deb`、`.AppImage` | `dpkg -i` 或 `chmod +x` 后直接运行 |

macOS 最低版本 11.0（见 override）。Linux 包 `depends: []`，运行时依赖
WebKitGTK 4.1、GTK3；在 Debian/Ubuntu 上由 CI 的 apt 安装步骤覆盖。

## 卸载

Windows 用“应用和功能”；macOS 删除 `.app` 并清理
`~/Library/Application Support/dev.wxlike.ui`；Linux `apt remove wxlike` 或删除
AppImage。卸载不删除服务端数据。

## 更新失败怎么回退

Tauri updater 只在签名验证通过后替换，失败保留旧版本。手工回退：安装上一版
安装包，数据目录不变；桌面端不持有服务端数据库，无需迁移回滚。发布清单和
回滚步骤见 `deploy/runbooks/release.md`。
