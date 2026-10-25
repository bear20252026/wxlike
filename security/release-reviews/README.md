# security/release-reviews — 独立发布评审（G17-05）

发布评审不是"最后一关卡人"，是**把"我们说的"和"我们验过的"摆在一起对比**。
所以这里只有两样东西：证据包（`evidence/*.json`）和阻塞清单（`blockers.json`），
判定交给工具（`sourceaudit release check`），人只负责提供证据与决定例外。

## 判定规则（工具实现，不看人情）

| 规则 | 含义 | 违反时 |
|---|---|---|
| 必需项非 pass 即阻断 | `required: true` 且 `status != pass` | `REL-REQUIRED-FAIL` |
| 自报不算验证 | 必需项 `kind: checklist` 即使写 pass 也不行 | `REL-SELF-REPORT` |
| 声明必须被覆盖 | `claims.platforms` 里每项都要在 `platforms` 里 `verified: true` | `REL-CLAIM-UNVERIFIED` |
| 标 verified 要有证据 | `platforms[].verified` 必须带 `evidence` | `REL-PLATFORM-EVIDENCE` |
| 阻塞必须可追责 | 每条 open blocker 要 `owner` + `issue` | `REL-BLOCKER-UNTRACKED` |
| 外部阻塞照样阻断 | `external: true` 只是"不计完成"，不是"可以放行" | `REL-BLOCKER-EXTERNAL` |
| 崩溃样本必须登记 | `corpus/manifest.json` 里 `blocked` 的样本要出现在 `blockers.json` | `REL-CORPUS-UNTRACKED` |
| 例外必须到期 | 任何 `exceptions[]` 都要 `owner` + `issue` + 未过期的 `expires` | `REL-EXC` / `EXC-EXPIRED` |

## 证据的 `kind` 决定分量

| kind | 含义 | 能不能让必需项通过 |
|---|---|---|
| `test` | 自动化用例（能跑出红/绿） | 能 |
| `artifact` | 可复查的产物（SBOM、报告、契约文件） | 能 |
| `device` | 真机记录（截图/录屏/日志，含机型与版本） | 能 |
| `external` | 外部供应商/第三方回执 | 能 |
| `checklist` | 自报"我做完了" | **不能**（必需项直接判 `REL-SELF-REPORT`） |

## 一轮评审怎么做

```bash
# 1) 先让各门禁给出自己的结论（这些结论会作为证据写进 evidence/*.json）
cd tools/source-audit
python -m sourceaudit provenance check
python -m sourceaudit advisories check --offline     # 冻结前换成联网版
python -m sourceaudit authz check
cargo test --manifest-path ../../tests/security/Cargo.toml

# 2) 写/更新 evidence/latest.json（每条证据都写"证据在哪"，不写形容词）
# 3) 判定
python -m sourceaudit release check
```

退出码：0 = PASS；3 = BLOCK；4 = 证据包缺失/坏了（**没结论 ≠ 通过**）。

## 阻塞项的生命周期

```
open ──(补丁合入 + 测试转绿)──> closed（必须带 closed_evidence）
  │
  └──(确实发不了版但要放行)──> exceptions[] 里加一条带 owner/issue/expires 的例外
                              到期自动变回 BLOCK
```

`closed` 不是改个字段就完事：`closed_evidence` 要写补丁 commit 或测试链接，
评审的人会点开看。**"我已修复"不是证据。**

## 文件

| 文件 | 作用 |
|---|---|
| `evidence/latest.json` | 当前这轮的证据包（发布判定读它） |
| `evidence.schema.json` | 证据包结构说明（给写证据的人看，也给下游工具用） |
| `blockers.json` | 唯一阻塞清单（每条带 owner + issue） |
| `REVIEW-2026-10-24.md` | 第一轮评审记录（人读版，含逐项结论与翻案条件） |

当前复核见 `REVIEW-2026-10-25.md`，五项 open 阻塞仍使发布为 BLOCK。历史证据不覆盖。`closed` 缺证据、blocked 语料引用 closed 阻塞均为硬错误。
