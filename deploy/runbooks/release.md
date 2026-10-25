# 发布

流水线：`.github/workflows/release.yml`（由 `ci/jobs/compose.py` 从片段生成，
G00 负责应用）。触发：`v*` tag 或手动 `workflow_dispatch`（`publish=false` 为演练）。

## 阶段

| 阶段 | 产物 | 失败即停止 |
|---|---|---|
| `android-release` | 签名 APK/AAB + 符号 + 证据 | secret 缺失/调试签名 → 失败 |
| `ios-release` | 分发签名 IPA + 证据 | `apps/mobile/ios` 缺失/secret 缺失 → 失败 |
| `desktop-release`（三平台） | 安装包 + `.sig` + 每平台清单 | 缺签名 → 失败 |
| `release` | 镜像 digest、SBOM、cosign 签名/证明、合并清单、GitHub Release | 任一校验失败 |

`release` job 在 `production-release` 保护环境运行，需要人工批准。

## 发布前

1. 版本一致：`ui/src-tauri/tauri.conf.json`、`apps/mobile/pubspec.yaml`、
   tag `v<version>` 三者相同；脚本会拒绝不一致。
2. `ci/jobs/policy/known-gaps.json` 中所有 `blocks_release` 为 `closed`；
   否则 `make_release_manifest.py --require-clean` 失败。
3. `deploy/signing-docs/device-evidence.json` 存在且通过
   `check_device_evidence.py`（真机 push/call/WebView/权限/重连）。
4. 最近一次 `restore-drill.sh` 在 7 天内。

## 演练（不发布）

```bash
gh workflow run release.yml -f version=0.2.0 -f publish=false
```

演练会构建、签名、验签、生成 SBOM 与清单，但**不 push 镜像、不创建 Release**。
清单中 `published=false`，并列出所有阻塞项。演练不能替代发布门禁。

## 正式发布

```bash
git tag v0.2.0 && git push origin v0.2.0
```

或 `gh workflow run release.yml -f version=0.2.0 -f publish=true`。

发布 job 会：

- 构建并 push `ghcr.io/<repo>/wxlike-server:<version>`，记录 `sha256` digest；
- 生成 SPDX SBOM；
- `cosign sign` 并立即 `cosign verify`（keyless，OIDC 身份限定本仓库）；
- 生成 build provenance attestation；
- 合并三平台 updater 清单并逐包验签；
- 写 `release-manifest.json`（含 gap 状态、移动端/真机证据哈希）；
- `gh release create/upload` 附加安装包、清单、SBOM。

## 发布后验证

```bash
cosign verify ghcr.io/<repo>/wxlike-server@sha256:<digest> \
  --certificate-identity-regexp '^https://github.com/<owner>/<repo>/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
curl --noproxy '*' https://<host>/health
```

再确认监控 10 分钟无新告警，对账 `ok=true`。

## 证据保存

`release-evidence` artifact 保留 90 天：`release-manifest.json`、SBOM、
`gaps.json`、移动端校验结果。GitHub Release 中同时保留一份，便于审计。
