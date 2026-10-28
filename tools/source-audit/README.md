# tools/source-audit — 来源、漏洞、越权与发布门禁（G17）

一个只用标准库的 Python 工具。CI 的 quality job 不装第三方包，门禁工具自己也不该
引入依赖树——"检查供应链"的工具自己带着 20 个传递依赖，说服力是负的。

许可证这道闸已删除（G00 决定：本项目只做内部研究、不对外分发）——
`licenses.py`、`notice.py` 与 `third_party/reviews/license-policy.json` 都已移除，
provenance 也不再按许可判定"能不能 vendor"。

```
tools/source-audit/
├── sourceaudit/
│   ├── cli.py          # 子命令与退出码
│   ├── provenance.py   # registry ↔ 本地文件核对（sha256 / 永久链接 / patchlog）
│   ├── sbom.py         # CycloneDX 1.6（cargo/npm/pub/docker）
│   ├── advisories.py   # 公告 → 可达性 → 门禁（G17-02）
│   ├── authz.py        # 源码路由 ↔ 授权矩阵 ↔ 用例（G17-03）
│   └── release.py      # 证据包 → pass/block（G17-05）
└── tests/              # unittest（零第三方依赖）
```

## 跑法

```bash
cd tools/source-audit
python -m unittest discover -s tests -v        # 工具自身的测试
python -m sourceaudit provenance check         # 来源登记
python -m sourceaudit sbom check               # SBOM 漂移
python -m sourceaudit advisories check         # 漏洞准入（--offline 可断网）
python -m sourceaudit authz check              # 授权矩阵三方核对
python -m sourceaudit release check            # 发布评审
python -m sourceaudit all --strict             # CI 一次跑完（warn 也阻断）
```

## 退出码（CI 只认这个）

| 码 | 含义 | 典型原因 |
|---:|---|---|
| 0 | 通过 | 检查真的跑了且没有阻断项 |
| 2 | 用法/参数错误 | `--root` 指错、子命令拼错 |
| 3 | 策略不通过 | 有 FAIL；`--strict` 下 WARN 也算 |
| 4 | 数据缺失/网络不可达 | registry 不存在、JSON 坏了、联网超时 |
| 130 | 被取消 | Ctrl-C：**不写半截产物、不当作通过** |

把 3 和 4 分开是刻意的：混在一起时，"工具坏了"会被当成"检查没过"，反过来也一样。

## 三条设计约束

1. **政策在 JSON，判定在代码。** `third_party/reviews/exclusions.json`、
   `security/advisory-policy/policy.json` 是可评审的数据；
   改政策不需要改代码，也就不需要重新 review 代码。
2. **例外必须能追责。** 任何豁免都要 `owner` + `issue` + `expires`，过期自动变红
   （`EXC-EXPIRED`）。不允许"先把日期改远一点"。
3. **空结果不是通过。** registry 为空 → `PROV-EMPTY` 警告；SBOM 为空 → 数据错误。
   空表无法区分"没引入"和"忘了登记"。

## 加一个第三方片段（正确姿势）

```bash
# 1) 在 third_party/reviews/registry.json 的 files 里登记：upstream / upstream_path /
#    commit（完整 SHA）/ decision / local_sha256 / patchlog
python - <<'PY'
import hashlib; print(hashlib.sha256(open('ui/src/vendor/x.ts','rb').read()).hexdigest())
PY
# 2) 写 third_party/reviews/patches/<UP>-<file>.md（拿了什么 / 改了什么 / 没拿什么）
# 3) python -m sourceaudit provenance check
```

`decision` 的四种取值：`ported`（原样复制）、`adapted`（改过，必须有 patchlog）、
`behavior_reference`（只借设计，无代码）、`excluded`（明确不引入，必须命中
`exclusions.json` 的规则 id）。

## 与 CI 的关系

独立 `.github/workflows/security.yml` 在 PR / main 上执行工具回归、provenance、
SBOM、离线 advisories、authz，以及独立 Rust 安全测试。
每日任务扩大变异 fuzz 至 200000 次并联网复核公告；手工勾选 `release_review`
会在前置检查成功后执行发布评审，存在真实阻塞就返回失败。
`all` 遇到数据缺失或网络异常返回 4，不会将门禁不可用降成警告。

新增接线与负向验收证据见 `security/release-reviews/REVIEW-2026-10-25.md`。

联网公告依照 OSV batch 契约逐生态/逐查询翻页；未知分级按实际 `policy.json` 阻断。单独与 all 两种入口的网络/数据异常均不能返回成功。
