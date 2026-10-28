# third_party/reviews — 第三方来源登记（G17-01）

这个目录回答一个具体问题：**仓库里每一段不是我们写的代码，是从哪来的、改了什么。**

它不是"致谢名单"。`THIRD-PARTY-NOTICES.md` 是对外义务声明（G00 维护，
不再由门禁强制）；这里是对内的**可核对账本**，由 `tools/source-audit` 逐条验证。

## 文件

| 文件 | 作用 | 谁改 |
|---|---|---|
| `registry.json` | 上游仓 → 片段 → 本地文件 sha256 的三段式登记 | 引入代码的人 |
| `exclusions.json` | "明确不引入"的规则（ee/、模型权重、研究锚点、第二 authority） | 需要改规则时（附理由） |
| `patches/*.md` | 每个 vendored 片段的改动说明 | 引入代码的人 |
| `sbom/wxlike.cdx.json` | 生成的 CycloneDX 1.6（cargo/npm/pub/docker） | `sbom generate` |
| `release/` | 发布时的来源快照与核对结论 | G17-05 |

`license-policy.json` 与 `notice-waivers.json` 已删除：许可证不再是判据
（G00 决定，见下）。registry 里的 `license_spdx` / `license_evidence` 保留为信息字段。

## 现状（2026-10-24，首次全量盘点）

**实际复制进本仓库的第三方源码：3 个文件。**

| 本地文件 | 上游 | 许可 | 形态 |
|---|---|---|---|
| `ui/src/vendor/tweb/slicedArray.ts` | morethanwords/tweb @ `57ef3ed` | GPL-3.0-only | 改 3 处 |
| `ui/src/vendor/tweb/listenerSetter.ts` | morethanwords/tweb @ `ef41b29` | GPL-3.0-only | 改 2 处 |
| `crates/rtc/src/sfu.rs` | 8xFF/atm0s-media-server @ `0ffc95a` | MIT | 跨库重写 |

**只借设计、无代码：** `crates/platform/src/communities.rs`（Rocket.Chat 的 channel-role
形状）、`crates/mq`（box-im 的队列/路由设计）、Zulip 的"主题是一等属性"。

**逐块排除：** Rocket.Chat `ee/`、Medusa 的订单/支付模块、模型权重、
`upstream_manifest.json` 的 29 个研究锚点（manifest 自己写明"not approved vendoring"）。

## 三条硬规则

1. **许可不作判据。** 本项目只做内部研究、不对外分发，G00 已删除许可证这道闸：
   引入片段不需要声明许可、不需要 LICENSE 证据、不需要兼容 AGPL 的论证。
   登记里保留许可字段只是为了"以后想知道去哪看"，缺了不红。
2. **sha256 对不上就是红。** 改了 vendored 文件必须同时更新 `local_sha256` 和 patchlog。
   这不是形式主义：不更新的话，下次安全评审看到的来源和实际代码不是同一份。
3. **排除也要有规则。** `excluded` 必须引用 `exclusions.json` 里的 id。"我们没抄它"
   无法验证，"按哪条规则不抄"可以。

## 常用命令

```bash
cd tools/source-audit
python -m sourceaudit provenance check       # 这份账本是否与磁盘一致
python -m sourceaudit sbom generate          # 重新生成 SBOM（可复现）
```

SBOM 是**可复现**的：不写生成时间，`serialNumber` 由组件 purl 集合派生。同一份锁文件
跑两次字节相同，所以 `git diff` 里出现的每一行都是真的依赖变化。
