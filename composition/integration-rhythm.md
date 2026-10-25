# G00-05｜集成节奏、容量与发布标准（#107，W0–W5 持续型）

> 状态：**常设机制已建立**（本文件 + 既有门禁）；容量数据化与发布演练随波次推进。

## 1. 集成节奏（已运转的事实）

| 节拍 | 内容 | 载体 |
|---|---|---|
| 每卡 | 一任务一分支一 PR；PR 引用任务 Issue；CI 全绿才合并 | 18 组通用规则（#80） |
| 每日 | main 双 workflow（ci + audit）常绿即"可集成水位"；红 main 冻结新 PR 合并 | `gh run list --branch main` |
| 每波（W0–W5） | 波次收口：跨组 integration-requests 清账、基线对账（quality/unwrap/migrations）、进度刷 #81/#80 | integration-requests/** |
| 串行集成 | 公共清单（Cargo.lock / workspace members / 根 router / 迁移编号）只由 G00 改；他人走 integration-requests | owners.json |

**本轮事实数据**（截至 2026-10-24）：G00-01（#201）、G02 七卡（#205）、G01-01（#208）
已按此节奏并入 main；并行事故（三次工作树串写）全部以"远端 SHA 对账 + 只提交
显式路径"恢复，零丢失。

## 2. 容量标准（门禁已定，数据化进行中）

| 维度 | 门禁 | 数据化状态 |
|---|---|---|
| 正确性 | ci.yml 三平台全量测试（~900 测试函数） | ✅ 每 PR |
| 安全 | cargo audit（RustSec）+ unwrap 防回潮计数 | ✅ 每 PR |
| 代码质量 | quality-gate 五指标基线闸（函数长/嵌套/魔法数/重复窗） | ✅ 每 PR |
| 覆盖率 | llvm-cov 报告（非阻断，PR 可见） | ✅ 每 PR；阈值待两周期基线后设定（#64 约定） |
| 容量/性能 | `docs/bench/`（bench_group / bench_hub） | ⏳ W3 前出第一份容量报告（hub 连接数/投递延迟 p99/检索延迟） |

## 3. 发布标准（RC 门槛，对齐 #80 全范围关闭条件）

1. **功能**：全部产品入口按平台/variant 完成字面功能，状态真实持久化且跨端可验
   （入口总账 composition/entries.json 为核对底册，346 条）。
2. **质量**：100 任务通过真实集成 + 同 SHA 云 CI；BLOCKED_EXTERNAL=0。
3. **闭环项**：消息持久化 ACK、多设备密钥、真实音视频、标准扫码、账本/退款/对账、
   小程序隔离、AI 授权命令面。
4. **发布演练**：签名/发布/备份恢复演练完成（G16 协同）。
5. **来源许可**：AGPL-3.0-only 复核 + 对外文案（G17）。

## 4. 阻塞登记（当前）

| 项 | 阻塞 | 登记处 |
|---|---|---|
| G00-04 PG 迁移 | G01-01 ports 层 | integration-requests/G00/G00-04.yaml |
| lru 0.19 | tantivy 下个 minor | PR #79/#78 说明 |
| glib 0.22 | tauri 上游（无 3.x 稳定版） | PR #79 说明 |
