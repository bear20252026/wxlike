# H-44：release workflow 输入未校验、可绕过主门禁

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `3f2fea3`。

审计原文：`AUDIT-60895e18.md:467-472`。`workflow_dispatch.version` 未校验就写
`GITHUB_OUTPUT`、插进 shell/Python argv；`release` 只依赖桌面产物，不等 main
CI / security gate。

## 修复

### 输入校验

新增 `ci/jobs/release/release_meta.py`：

- 严格 SemVer 2.0.0 正则：数字段无前导零，pre-release/build 标识符合规；
- 先拒绝 `\n`/`\r`，杜绝 `GITHUB_OUTPUT` 多行注入；
- 长度上限 96；
- dispatch 输入优先；否则 tag 必须 `v<semver>`；
- 只把校验后的 version/publish/image 写入 `GITHUB_OUTPUT`；
- `publish` 只由 `push` 或布尔输入决定，image 由 `GITHUB_REPOSITORY` 派生。

release workflow 的 `resolve version` 改为 `python3 ci/jobs/release/release_meta.py`，
不再用 shell `if` 拼接原始输入。

### 参数化传 argv

- 桌面 manifest 版本核对：`RELEASE_VERSION` 环境变量 → `python3 - "$RELEASE_VERSION"`。
- dry-run 镜像 inspect：`$IMAGE:$RELEASE_VERSION`。
- release manifest 组装：`--version "$RELEASE_VERSION"`、`PUBLISH` 环境变量。
- GitHub release tag/title：`v$RELEASE_VERSION`。
- Docker `tags`/label 仍用 `steps.meta.outputs.version`，但那是已校验的
  SemVer，进入 action input 而不是 shell 拼接。

### 主门禁

- `ci.yml`、`security.yml` 增加 `workflow_call`。
- release 新增 `ci-gate`、`security-gate` 两个 reusable workflow 调用。
- `release` 的 `needs` 加入二者，`if` 要求两者 result 都是 `success`；
  桌面产物仍需成功。Android/iOS 仍允许 skipped（两个 job 当前 `if: false`），
  但桌面发布会等完整 main CI 与 security gate。

## 证据

`python3 -m unittest ci.jobs.tests.test_release_workflow -v`：7 passed。

- 合法：`0.2.0`、`1.0.0-rc.1`、`1.0.0-alpha.1+build.20261127`、`10.20.30`；
- 拒绝：换行/回车注入、`;`、`$(id)`、`v0.1.0`、`01.2.3`、`1.2`、
  `1.2.3-01`、全角数字等；
- `GITHUB_OUTPUT` 只写三行已校验字段；
- YAML 结构断言 release 依赖 `ci-gate`/`security-gate`，两个被调 workflow 都有
  `workflow_call`；
- 所有 `run:` 行都不含 `inputs.version` / `steps.meta.outputs.version`。

## 边界

- 本地只做静态/单元验证；没有真实 GitHub Actions run（推送仍受 OAuth App
  缺 `workflow` scope 阻断）。reusable workflow 的调用权限、tag push 的
  `github.event.before` 全量矩阵选择需要在远端跑一次确认。
- `security.yml` 的 `release-review` 仍是 dispatch-only；release 调用的是
  `static`+`regression` 的 security gate，不含 online advisory review。
- CI 全矩阵被 release 调用会显著增加 release 时长；这是用成本换"发布不得绕过
  主门禁"。
