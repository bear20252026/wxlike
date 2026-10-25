# 借用输入投递留档（2026-10-24）

> 本文件是 GitHub 侧投递的本地留档：评论在远端 issue/PR 上，内容要点在此可查。

## GitHub 评论（5 条，已发出）

| 目标 | 链接 | 性质 |
|---|---|---|
| #150 G08-05 | https://github.com/bear20252026/wxlike/issues/150#issuecomment-6392801518 | 开工前决策：vendor glass-easel |
| #153 G09-02 | https://github.com/bear20252026/wxlike/issues/153#issuecomment-6392802047 | 开工前参照：TRTC_Web 状态机 |
| PR #223 G03-04 | https://github.com/bear20252026/wxlike/pull/223#issuecomment-6392805128 | 评审输入：mars 参数校准 |
| PR #225 G06 | https://github.com/bear20252026/wxlike/pull/225#issuecomment-6392805910 | 验收输入：红包/转账官方规则 |
| #80 总跟踪 | https://github.com/bear20252026/wxlike/issues/80#issuecomment-6392826702 | 汇总索引 |

## 本地 YAML（7 份，未提交，待集成方统一 commit）

- G08/borrow-glass-easel.yaml · G08/api-surface-subset.yaml
- G09/trtc-signaling-reference.yaml · G03/mars-backoff-calibration.yaml
- G06/scenario-acceptance.yaml · G17/borrow-public-research.yaml
- G00/reference-tier0-additions.yaml

## refs/ 镜像状态（2026-10-24 补齐）

- ✅ refs/glass-easel（新克隆，--depth 1，5 MB）
- ✅ refs/TRTC_Web（新克隆，--depth 1，53 MB）
- ✅ refs/dimina（**已有**：滴滴星河小程序容器，Apache-2.0，四端运行时，
  编译并运行小程序于 Android/iOS/Harmony/Web——比 glass-easel 更完整的
  生产级开源容器参照，G08 各卡均可对照；其内核亦基于 glass-easel 体系）

## 遗留给集成方

1. commit 本目录新增 YAML（主检出停在 feat/FP-G00-02，带未提交改动，未动 git）
2. G00 认领 reference-tier0-additions.yaml（Tier 0 增补 + refs 已就位）
