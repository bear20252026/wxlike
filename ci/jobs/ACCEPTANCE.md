# G16 验收对照

状态：**2026-10-24**。只把实际执行过的结果写成“已验证”；没有运行条件的项目
明确标为“未执行”，并由 `policy/known-gaps.json` 或发布门禁负责阻断。

## G16-01 统一云 CI 门禁与缓存

| 验收点 | 证据 | 状态 |
|---|---|---|
| C0–C11 全部由片段生成 | `ci/jobs/workflows/*.workflow.yml` + `compose.py` → `generated/ci.yml`（22 jobs）、`generated/release.yml`（4 jobs） | 已验证（`compose.py --check` 通过） |
| action 全部 40-hex 且与策略一致 | `policy/action-pins.json`；`check_pins.py` 反向索引校验 SHA | 已验证（0 violations） |
| 合同变更强制全矩阵 | `tests/test_ci.py::test_contract_change_forces_full_matrix` | 已验证（4/4 path tests） |
| 未知路径失败安全 | `test_unknown_path_forces_full_matrix` | 已验证 |
| 文档改动不触发代码任务 | `test_docs_only_selects_nothing` | 已验证 |
| 分支保护清单与 gate 一致 | `policy/required-checks.json`；`gate.py` 缺失/失败即红 | 已验证（GateTest 4/4） |
| 缓存按 lock/toolchain/OS/用途隔离 | 各 job 的 cache key 均含 OS+lock+toolchain+purpose | 静态检查 |
| 无 `continue-on-error` 逃逸 | `check_pins.py` 递归扫描 required jobs | 已验证（策略检查通过） |
| 每日绿、发布红 | `check_gaps.py --release`；e2e 中 blocked 不算 pass | 已验证（7 个 release-blocking gap） |

未执行：G00 尚未把 `generated/` 应用到 `.github/workflows/`（`--check-live` 会红），
这是跨组请求 `integration-requests/G16/G16-01.yaml`。

## G16-02 服务端和桌面跨系统构建

| 验收点 | 证据 | 状态 |
|---|---|---|
| 服务端三平台构建/测试 | `build/job.yml` 的 `test`/`native` matrix | 未在本机执行（CI 专属） |
| 镜像非 root、只读、无 secret、健康检查 | `containers/server/Dockerfile`、`verify-image.sh`、`smoke.sh` | 未执行：本机 Docker daemon 不可用 |
| FFmpeg 来源/许可/NOTICE | `containers/live-ingest/*`、`deploy/third-party/*`、`check_ffmpeg_notice.sh` | 静态检查 |
| 桌面三平台安装包 | `desktop/job.yml` 的 `desktop-package` | 未执行（CI 专属） |
| updater 篡改必须被拒 | `check_updater.py --self-test` | 未执行：本机无 minisign；脚本缺工具即失败 |
| 生产 Compose 只接受 digest | `containers/server/compose.prod.yml` + 发布清单 | 静态检查 |
| 生产环境变量模板完整 | `deploy/production/wxlike.env.example` | 静态检查（无真实值） |

## G16-03 Android/iOS 云构建、签名与设备证据

| 验收点 | 证据 | 状态 |
|---|---|---|
| Android release 构建 + 符号 | `ci/jobs/mobile/job.yml`、`check_apk.py` | 未执行（本机无 Flutter/Android 构建条件） |
| release APK **与 AAB** 必须打进 `libwxlike_ffi.so` | `mobile-release/job.yml` 的 `cargo ndk` + `check_apk.py --require-native-lib`/`--aab` | 门禁双向已验证（`test_ci.py::NativeLibGateTest`：缺库红、补齐放行）；真包待 CI |
| 发布签名必须非 debug key | `mobile-release/job.yml --require-release-key` | 未执行（无 signing secret） |
| IPA 必须分发签名 | `check_ipa.py` | 未执行：`apps/mobile/ios` 不存在，job 显式失败 |
| 真机证据 schema + 校验 | `deploy/signing-docs/device-evidence.schema.json`、`check_device_evidence.py` | 校验器逻辑已验证（单测接受完整证据、拒绝 fail/占位） |
| 真机结果 | — | **缺失**，`MOBILE-DEVICE-EVIDENCE` 保持 open |
| 密钥清单 | `deploy/signing-docs/secret-inventory.md` | 静态检查 |

## G16-04 端到端、故障与容量门禁

本机使用真实 `target/debug/wxlike-server.exe` 与临时 SQLite 执行。

| 场景 | 结果 |
|---|---|
| 成功：注册/登录/多设备会话/撤销旧 token | pass |
| 好友申请/接受 | pass |
| 发送 + 幂等重放（同 key 同 msg_id、单条落库、原文不被覆盖） | pass |
| 撤回 | pass |
| 钱包充值/转账/余额/流水/对账 | pass（750/250，supply=1000，`ok=true`） |
| 空输入/超长/重复注册/错密码/越权会话/未知路由 | 422/409/401/403/404 全部符合 |
| 超时：黑洞连接 | 客户端按 timeout 分类 |
| 取消：半截 HTTP 请求后断开 | 服务端继续健康且可注册新用户 |
| 重启恢复：SIGTERM → 同库重启 | 登录、消息、好友、钱包、对账全部保持 |
| 故障：坏 SQLite 启动 | 进程退出 1，拒绝启动；迁移/回滚说明见 `deploy/runbooks/migration.md` |
| 权限：非成员读/发/改角色 | 全部拒绝 |
| 支付回调重放 | **blocked**：`POST /api/payment/callback` → 404，G06 缺口 |
| RTC 弱网 | **blocked**：无 TURN/SFU，G09 缺口 |
| 容量：16 并发 × 400 次发送 | p95 155.66ms、p99 365.21ms、242.5 rps、0 错误、400 个唯一 msg_id，预算全通过 |

原始证据在本地 `/tmp/g16-e2e3/*.json`、`/tmp/g16-perf/performance.json`（未提交）。
CI 中由 `e2e` / `performance` job 生成并保留 14 天。

## G16-05 生产可观测、恢复与发布演练

| 验收点 | 证据 | 状态 |
|---|---|---|
| 告警包含 `absent()` 缺失检测 | `deploy/production/prometheus-alerts.yml` | 静态检查 |
| 看板与告警同源 | `grafana-dashboard.json` | 静态检查 |
| 日志脱敏检查器 | `check-log-redaction.py --self-test` | 已验证（3 条良性通过，4 类泄露检出） |
| 备份一致性与完整性 | `backup.sh` 实测：1 用户/1 消息、schema 7、SHA256、`integrity_check=ok` | 已验证（合成库，验证机制） |
| 恢复演练与实测 RPO/RTO | `restore-drill.sh` 实测：`rpo_seconds=10.2`、`rto_ms=27712`、计数一致 | 已验证（合成库；生产快照演练仍待做） |
| 发布清单与签名/SBOM/digest | `ci/jobs/release/*` | 未执行：无 registry/OIDC/Docker daemon、无签名密钥 |
| 回滚手册 | `deploy/runbooks/rollback.md`、`deploy/runbooks/migration.md` | 静态检查 |
| trace/request id | — | **缺失**，`OBS-TRACE-ID` 保持 open |
| 生产 PG 备份 | — | **缺失**，`G00-04-PG` 保持 open |

## 明确未验证的边界

1. **Docker/容器**：本机 Docker daemon 不可用，未构建镜像、未跑 `smoke.sh`、
   未跑 `verify-image.sh`。CI 的 `image` job 会执行这些步骤。
2. **桌面打包**：未执行 Tauri 三平台打包与 minisign 验签。
3. **移动端**：未构建 APK/IPA，未持有签名密钥，无真机证据。
4. **发布**：未 push 镜像、未创建 GitHub Release、未生成真实 SBOM/attestation。
5. **跨组依赖**：G00 应用 workflows、G06 支付回调、G09 TURN/SFU、G03 trace/readiness
   均未完成，发布门禁保持失败是预期行为。

## 一键复核

```bash
python -m unittest discover -s ci/jobs/tests -v
python ci/jobs/compose.py --check
python ci/jobs/audit/check_pins.py \
  --workflow ci/jobs/generated/ci.yml \
  --workflow ci/jobs/generated/release.yml
python deploy/production/check-log-redaction.py --self-test
cargo build --locked -p wxlike-server
WXLIKE_SERVER_BIN=target/debug/wxlike-server bash tests/e2e/ci.sh
python tests/perf/run.py --server-bin target/debug/wxlike-server \
  --budgets ci/jobs/performance/budgets.json --evidence-dir /tmp/perf
```
