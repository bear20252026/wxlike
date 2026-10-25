# third_party/reviews — 第三方来源登记（G17-01）

这个目录回答一个具体问题：**仓库里每一段不是我们写的代码，是从哪来的、按什么许可、改了什么。**

它不是"致谢名单"。`THIRD-PARTY-NOTICES.md` 才是对外义务声明（G00 维护）；
这里是对内的**可核对账本**，由 `tools/source-audit` 逐条验证。

## 文件

| 文件 | 作用 | 谁改 |
|---|---|---|
| `registry.json` | 上游仓 → 片段 → 本地文件 sha256 的三段式登记 | 引入代码的人 |
| `license-policy.json` | SPDX → allow/review/deny 的政策表 | 需要改政策时（附理由） |
| `exclusions.json` | "明确不引入"的规则（ee/、模型权重、研究锚点、第二 authority） | 同上 |
| `notice-waivers.json` | NOTICE 未覆盖的直接依赖，带 owner/issue/到期日 | G17 登记，G00 偿还 |
| `patches/*.md` | 每个 vendored 片段的改动说明 | 引入代码的人 |
| `sbom/wxlike.cdx.json` | 生成的 CycloneDX 1.6（cargo/npm/pub/docker） | `sbom generate` |
| `release/` | 发布时的来源快照与核对结论 | G17-05 |

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

1. **未知许可不得 vendor。** `NOASSERTION` 在政策里是 deny。想用就先逐文件读 LICENSE，
   在 registry 里按片段声明具体许可——"上游根许可是 MIT"不等于"这个文件是 MIT"。
2. **sha256 对不上就是红。** 改了 vendored 文件必须同时更新 `local_sha256` 和 patchlog。
   这不是形式主义：不更新的话，下次安全评审看到的来源和实际代码不是同一份。
3. **排除也要有规则。** `excluded` 必须引用 `exclusions.json` 里的 id。"我们没抄它"
   无法验证，"按哪条规则不抄"可以。

## 常用命令

```bash
cd tools/source-audit
python -m sourceaudit provenance check       # 这份账本是否与磁盘一致
python -m sourceaudit sbom generate          # 重新生成 SBOM（可复现）
python -m sourceaudit notice check           # NOTICE 是否覆盖直接依赖
```

SBOM 是**可复现**的：不写生成时间，`serialNumber` 由组件 purl 集合派生。同一份锁文件
跑两次字节相同，所以 `git diff` 里出现的每一行都是真的依赖变化。
