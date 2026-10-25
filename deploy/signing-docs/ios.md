# iOS 签名

## 现状

仓库里**还没有 `apps/mobile/ios`**，`flutter build ipa` 无法运行。`ios-release`
job 的第一步会显式失败并说明原因，而不是跳过或产出假证据。补 iOS 工程属于
移动端职责，已写入 `integration-requests/G16/G16-03.yaml`。

## 补齐工程后

1. 在 macOS 上 `flutter create --platforms=ios apps/mobile`，提交 `ios/`；
2. 配置 Bundle Identifier（与 Android applicationId 同一产品）；
3. 在 Apple Developer 生成 Apple Distribution 证书与 App Store 描述文件；
4. 把 p12、口令、描述文件、Team ID 写入 `mobile-release` 环境 secret：
   `APPLE_CERTIFICATE_BASE64`、`APPLE_CERTIFICATE_PASSWORD`、
   `APPLE_PROVISIONING_PROFILE_BASE64`、`APPLE_TEAM_ID`。

## CI 做了什么

`ios-release` 在 macOS runner 上：

1. 缺 `apps/mobile/ios` → 失败；
2. 缺任一 secret → 失败（不会静默改跑无签名构建）；
3. 建临时 keychain，导入证书和描述文件，`security set-key-partition-list`；
4. 用 `deploy/signing-docs/ExportOptions.plist` 替换 Team ID；
5. `flutter build ipa --release --obfuscate --split-debug-info=...`；
6. `check_ipa.py`：版本匹配 `pubspec.yaml`，签名主体必须是
   `Apple Distribution`/`iPhone Distribution`，否则失败；
7. 上传 IPA、符号、证据，保留 30 天。

## 复核

```bash
unzip -l wxlike.ipa | grep Info.plist
codesign -dv --verbose=4 Payload/Runner.app
```

证书过期、描述文件不含目标设备、Bundle ID 不匹配都会在上传 App Store 时失败；
CI 只证明签名主体与版本，不替代 App Store Connect 的校验。
