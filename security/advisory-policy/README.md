# security/advisory-policy — 漏洞与依赖准入门禁（G17-02）

`cargo deny` / `cargo audit` 已经在 CI 里跑，但它们回答的是"锁文件里有没有命中公告"。
本卡补的是另外三个问题：

1. **哪一份锁？** 根 workspace 与 `ui/src-tauri` 是两份独立解析的锁，同一 crate 可以
   一个已修、一个没修（本仓库首跑就命中：`ctutils` 根 0.4.5 / 桌面 0.4.3）。
2. **可不可达？** 可选 feature 未启用、组件根本没安装（例如没装 `@vitejs/plugin-rsc` 的
   React SPA），都不该按"有漏洞"拦。
3. **例外到期了没有？** 违规豁免一旦写下来，没人会记得回来删。这里带
   `owner + issue + expires`，到期自动红。

## 文件

| 文件 | 作用 |
|---|---|
| `policy.json` | 门禁规则：哪些严重级阻断、可达性规则、例外登记、扫描面清单 |
| `advisories.json` | 机器可读的公告表（来自总册第 6 节，含受影响区间与修复版本） |
| `report-2026-10-24.md` | 本轮四分类报告（已修复 / 候选新增 / 不适用 / 待核实） |

## 跑法

```bash
cd tools/source-audit
python -m sourceaudit advisories check --offline     # 日常提交：只用本地公告表
python -m sourceaudit advisories check --timeout 20  # 冻结/发版前：联网复核 OSV
python -m sourceaudit advisories check --input audit.json   # 复用 cargo audit --json
```

- `--offline`：不联网，按 `advisories.json` 判定。**PR 门禁用这个**——结果稳定，
  不会因为上游公告库刷新而让一条无关的 PR 变红。
- 联网：把锁文件里的 871 个组件批量问 OSV，发现本地表里没有的公告。
  **冻结依赖/发版前必须跑一次**（`gate.require_online_before_freeze`）。
- 断网/超时：退出码 4，不是 0。宁可"没结论"，也不要"没查就当没事"。

## 判定顺序（不能换）

```
在依赖图里吗？──否──→ not_applicable（不报，但写清"锁文件里没有"）
     │是
版本在受影响区间吗？──否──→ fixed
     │是
可达吗（feature / 安装面 / 代码路径）？──否──→ not_applicable（写依据）
     │是
按严重级：critical/high → 阻断；medium/low → 警告
    例外（owner+issue+未到期）→ 放行但计入 exempt
```

## 加一条例外的正确姿势

```json
{
  "id": "RUSTSEC-2026-0385",
  "owner": "G00",
  "issue": "#239",
  "expires": "2026-11-30",
  "reason": "为什么现在不能修、谁在修、修完怎么验证"
}
```

禁止两种"例外"：

- **"看起来是误报"** → 走 `not_applicable`，给出不可达的证据；
- **"上游还没修"** → 说明这个版本不可用，换版本或换实现。

## 与 `cargo deny` 的分工

| 工具 | 管什么 |
|---|---|
| `cargo deny check advisories` | Rust 生态的 RustSec 公告（RUSTSEC-*），CI 每次跑 |
| `cargo audit` | 同上，周跑，输出更易读 |
| **本门禁** | 跨生态（cargo / npm / pub / 容器镜像）、按锁文件分面、可达性判定、例外到期 |

两者不重复：`cargo deny` 不知道桌面锁与根锁的区别，也不知道 `@vitejs/plugin-rsc` 装没装。

2026-10-25：#239 的 ctutils 例外已移除，根/桌面锁均为 0.4.5。上面的例外 JSON 仅为字段示例，实际生效项以 `policy.json` 为准。最新离线复核见 `report-2026-10-25.md`，历史联网记录保留。

2026-10-25 联网政策收紧：`unverified=block`。查询按 crates.io/npm/Pub 分生态并处理分页；响应缺项及详情不可用返回 4。同一公告的多个受影响版本全部保留。不用粗略 CVSS 向量猜分级，未知等级须复核。历史文档中“未知仅警告”口径由本政策替代。
