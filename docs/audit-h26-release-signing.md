# H-26：release 缺签名不再回退 debug

审计条目：`H-26 Release 缺 signing properties 回退 debug 签名`（条件集成）。

## 修复

1. `apps/mobile/android/app/build.gradle.kts`
   - 只有 `storeFile`、`storePassword`、`keyAlias`、`keyPassword` 四项齐全且
     `storeFile` 指向真实文件时，才创建 release signing config。
   - `release` build type 不再取 `signingConfigs.getByName("debug")`。
   - `gradle.taskGraph.whenReady` 检查：只要 task graph 里出现名字含 `Release`
     的任务且签名材料不完整，就在执行前抛 `GradleException`，并列出缺失项。
2. `apps/mobile/build_android.sh`
   - release 模式在生成绑定前检查 `key.properties` 存在且四个键非空；
     缺任何一项直接退出，不再走到 Gradle 才发现。
3. `ci/jobs/mobile/check_apk.py`
   - 从 `apksigner --print-certs` 同时解析 DN 与证书 SHA-256（兼容
     `Signer #1 ...` / `V2 Signer: ...` 两种标签）。
   - 新增 `--expected-cert-sha256`：提供后必须匹配，否则非零；evidence 增加
     `signer_sha256`。
4. `.github/workflows/release.yml` 与 `ci/jobs/mobile-release/job.yml`
   - `C8 require signing material` 把 `ANDROID_CERT_SHA256` 纳入必需 secret。
   - `C8 verify release signature` 传 `--expected-cert-sha256`，发布签名证书
     指纹与配置不一致时 job 失败。

## 测试与证据

- `python3 -m unittest ci.jobs.tests.test_ci`：66 tests OK（包含两种
  apksigner 证书标签解析）。
- `python3 ci/jobs/compose.py --check --check-live`：generated 与 live 均 ok。
- `bash -n apps/mobile/build_android.sh` 通过。
- 生成文件 `ci/jobs/generated/release.yml` 与 `SHA256SUMS` 已同步。

## 边界

- 本机没有 Flutter/Android SDK，未执行真实 release APK 构建；Gradle 的
  `taskGraph` 拒绝路径尚未在真机构建中触发。最终以 CI/mobile 或 release 的
  Android job 为准。
- `ANDROID_CERT_SHA256` 是发布环境 secret，仓库内不存值；未配置时 release
  job 会按 fail-closed 失败，这是 H-26 的预期行为。
- CI 的普通 mobile job 使用每次现生成的 CI keystore，只校验“非 debug 签名”；
  生产指纹校验在 release job 执行。
