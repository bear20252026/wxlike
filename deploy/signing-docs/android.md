# Android 签名

## 一次性准备

```bash
keytool -genkeypair -v -keystore release.keystore -alias wxlike \
  -keyalg RSA -keysize 4096 -validity 10000
base64 -w0 release.keystore    # 写入 ANDROID_KEYSTORE_BASE64
```

把 keystore 和两个口令存入密钥管理器；GitHub 环境 secret 按下表配置：
`ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS`、
`ANDROID_KEY_PASSWORD`。

## CI 做了什么

`ci/jobs/mobile-release/job.yml` 的 `android-release`：

1. 任一 secret 为空 → `::error::` 并失败；
2. 解码 keystore 到 `$RUNNER_TEMP`，生成 `apps/mobile/android/key.properties`；
3. `flutter build apk --release --obfuscate --split-debug-info=...` 和
   `flutter build appbundle --release`；
4. `check_apk.py --require-release-key`：版本必须与 `pubspec.yaml` 一致，
   签名 DN 不能包含 `Android Debug`，符号文件必须存在；
5. 上传 APK/AAB/符号/证据，保留 30 天。

## 本地复核

```bash
apksigner verify --print-certs app-release.apk
aapt dump badging app-release.apk | head -1
```

如果启用了 Google Play App Signing，上传密钥签名与商店分发签名不同：CI 验证的是
**上传密钥**；安装包最终签名以 Play 控制台证书为准。此差异必须在发布记录中写明。

## 版本

版本来自 `apps/mobile/pubspec.yaml` 的 `version: x.y.z+build`。发布 job 不修改
版本号；改版本是代码变更，必须走 PR。
