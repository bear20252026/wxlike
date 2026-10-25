# Secret 清单（只有名称，没有值）

环境：GitHub Actions `mobile-release`（需要 reviewer 批准）；发布 job 只读。

| 名称 | 用途 | 轮换 | 失效后果 |
|---|---|---|---|
| `ANDROID_KEYSTORE_BASE64` | release keystore 的 base64 | 每年或泄露时 | 无法发布；换 key 会改变签名 |
| `ANDROID_KEYSTORE_PASSWORD` | keystore 口令 | 同上 | job 无法解锁 |
| `ANDROID_KEY_ALIAS` | 签名 alias | 随 keystore | job 找不到 key |
| `ANDROID_KEY_PASSWORD` | key 口令 | 同上 | job 无法签名 |
| `APPLE_CERTIFICATE_BASE64` | Apple Distribution 证书 p12 | 证书到期前 30 天 | 无法签 IPA |
| `APPLE_CERTIFICATE_PASSWORD` | p12 口令 | 同上 | 无法导入证书 |
| `APPLE_PROVISIONING_PROFILE_BASE64` | App Store 描述文件 | 随证书/设备变更 | 安装或上传失败 |
| `APPLE_TEAM_ID` | 团队 ID（非秘密，但与环境绑定） | 团队变更 | 导出 plist 填错 |
| `TAURI_SIGNING_PRIVATE_KEY` | 桌面更新 minisign 私钥（`production-release` 环境） | 每年 | 无法发布更新 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 上述私钥口令 | 同上 | 无法签名 |

## 禁止事项

- 把 keystore/证书/私钥提交进仓库或放进 artifact；
- 在 `run:` 里 `echo` 任何 secret，或把 secret 拼进错误信息；
- 用 `pull_request_target` 读取这些 secret；
- 在 fork PR 上尝试签名（拿不到 secret，必须失败而不是跳过）。

配置缺失时 job 显式 `::error::` 并退出 1，不会生成“未签名但绿色”的发布产物。
