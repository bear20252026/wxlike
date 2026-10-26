# 交接任务书 — wxlike R5 审计整改 + 产品贯通（PR #288）

> 交接人：ZCode（自动化代理）｜日期：2026-10-26
> 分支：`fix/audit-round5-integration`（3 个提交在 PR #288 上，全部已推送）
> 仓库：https://github.com/bear20252026/wxlike ｜ PR：https://github.com/bear20252026/wxlike/pull/288
> 基线：main `e126222` ｜ 分支最新：`2f7fb191` ｜ 工作树干净（未提交内容为 0）

---

## 1. 任务背景（一段话）

用户要求对本仓库做全面审计（覆盖 100 大类、≥100 问题 + 100 提升点、并实施修复），
核心目标是**以 Rust 为核心，让新版 UI（apps/desktop-web，React）所有入口能完成真实操作**——
即打通「最新 UI → Rust 核心（core_dispatch）→ 真服务端 → 另一台客户端收到 → 重启后数据正确」。
六个已知阻塞：桌面入口未挂 core_dispatch、网页 wasm 核心未接、新 UI 七个页面未挂导航、
Android 默认旧壳、视频/支付链路断裂、CI 未覆盖新 UI。

## 2. 已完成（全部在本分支，E2E 已证明核心链路）

**关键成果**：`ui/src-tauri/tests/core_bridge_e2e.rs` 3/3 绿——新 UI 登录（错口令被拒）→
发消息 → 对端真客户端直连 QUIC hub 收到同一条消息 → 换实例恢复会话 → msg_id 前后一致 →
重启后继续收发。全程真进程内服务端，零 mock。

### 2.1 六大阻塞的处置（详见 AUDIT_REPORT_R5.md）

| 环节 | 状态 |
|---|---|
| 桌面入口 | ✅ `core_dispatch/core_subscribe/core_capabilities` + `core://event`，55 action 桥（`ui/src-tauri/src/core_bridge.rs`）；`tauri.conf.new-ui.json` + CSP + capabilities ACL |
| 网页核心 | ✅ wasm32 编译通过 + `wasm_bindgen WxlikeCore` 入口；浏览器适配器接线留 G01-03 收尾 |
| 页面导航 | ✅ video/wallet/files/mini/discover/moments/calls 七域挂载 + Alt+1..6 |
| Android | ✅ 新壳为默认（组合根）、Kotlin 平台桥、权限/深链清单、release 登录路径、锁屏修复 |
| 业务接口 | ✅ 视频五断点 + 钱包四处断点全修（幂等表、越权校验、查询路由） |
| 验收与发布 | ✅ 23-job 组合 CI 应用 + web-v2 job + release 门恢复 |

### 2.2 质量/安全整改

- 契约兼容层（types ↔ TS 往返测试）、ActionId::derive 兜底（去 panic）
- client-web：event_id 键、sensitive 缓存 AES-GCM、Closure 泄漏、RefCell panic
- 服务端：QR 登录六路由、HLS 分段路由、钱包 `Idempotency-Key` 幂等表、
  presence/metrics/endpoints 鉴权、5xx 脱敏
- 旧 UI：msgIndex 漂移修复、监听泄漏、wire.ts 对齐 Rust 11 变体
- 两个生产级死锁实锤并修复：open_session 的 if-let MutexGuard 自锁、
  local_history 持锁走 HTTP；另有 consume_qr_ticket 双重 db().lock()（今日发现）
- AUDIT_REPORT_R5.md：100 问题 + 100 提升全清单（每项带位置与状态）

## 3. ⚠️ 当前卡点：CI 还有 5 个红（接手者从这里开始）

最后一次推送后的检查状态：25 pass / 5 fail / 0 pending。
**红的原因全部是"强制版 CI 首次跑在新代码上"，属于存量暴露 + 少量未完成修复**：

### 3.1 android —— network_security_config lint（应已修，待确认）
我已给所有 `<domain>` 加了 `includeSubdomains="false"`（3 个 res/xml 变体），
但该推送后的 android job **又被新问题打红**：`apksigner found no signer certificate`
→ 我已实施修复：`ci/jobs/mobile/job.yml` 现生成临时 keystore 写 `key.properties`，
`app/build.gradle.kts` 读之（无则回退 debug）。**本地 gradle 已验证 BUILD SUCCESSFUL**。
若仍红：取 `gh api .../actions/jobs/<id>/logs` 查 keytool/gradle 步骤。

### 3.2 flutter —— g12 mobile_app_test（已修，待 CI 确认）
「微信」同时是 tab 标签与会话页标题 → 全局 `find.text` 撞 2 个；
且 `_group` 用 `Container(color:)` 包 `ListTile` 触发 9 个框架异常。
已修：测试改 `find.widgetWithText(NavigationDestination, ...)`；
`_group` 改返回 `Material(color:)`。本地 `flutter analyze` 无问题；
**flutter test 本机跑不了**（沙箱代理挡 flutter tester 的回环 HTTP），
需在 CI 确认。若再红：注意 account_page 还有一个 `Container(color:)` 包头像区
（line ~220，未触发异常不必改）。

### 3.3 coverage —— coverage 计数不达标
这个 job 在 main 上也是 non-blocking/边缘状态。接手者建议：先看
`coverage_summary.py` 输出与本 PR 新增行；我们的分支大量加了测试
（platform 21+21、types/cmd 206、host 12、E2E 3），若仍差，
属于**覆盖率基线也需要与 G 系列合并后的现实对齐**的问题，
与 unwrap 基线追平同理——按仓库流程再基线化，不要伪造覆盖。

### 3.4 desktop-package (linux/windows) —— tauri 打包新 UI 失败
`tauri.conf.new-ui.json` 指向 `apps/desktop-web/dist`，CI 的 desktop-package
步骤若用了这个配置，需要先跑 `apps/desktop-web` 的构建（web-v2 job 里有）。
**桌面 job 的 build 顺序可能缺一步 npm build**。接手者：检查
`ci/jobs/desktop*` 片段里 tauri build 前是否构建了 frontendDist；
或者在 job.yml 里把 `tauri.conf.json` 切回旧 UI、新 UI 配置留待后续切。
另外注意 `ui/src-tauri` 的 Cargo 在 CI 的 linux/windows 上是新增量
（wxlike-ui 依赖 tauri test feature 只在 dev-dependencies）。

### 3.5 audit —— 又有 +N（本轮残余）
闸门上次本机全绿（magic 160 < 162）。CI 新红可能是**最后一个推送
（authz-matrix.json 缩进改动）或 fmt 后漂移**。接手者直接：
`python scripts/quality-gate.py` 复现 → `git diff main --stat` 对比今天的
增量 → 通常是 1-2 个字面量漏命名。**不要**手动改基线 json——闸门
`--update` 只许收紧；要靠修代码。

### 3.6 e2e / doc / ingest-image / security —— 均已在最后三个提交修复
- doc：ffi `dispose` 文档的私有 intra-doc link 去掉括号（cargo doc -D warnings）
- e2e：`/metrics` smoke 改为「无 token 404 + 有 token 200」双断言（匹配加固后契约）
- ingest-image：`.dockerignore` 例外放行 THIRD-PARTY-NOTICES.md
- security：SBOM 重生成（sourceaudit sbom generate）、getrandom/wasm-bindgen
  登记 THIRD-PARTY-NOTICES、8 个新端点登记 authz-matrix
  （exchange 的 public_reason 已写明为何匿名）
最后一次推送后这些 job 尚无完整结论，以最新 run 为准。

## 4. 环境与工具备忘（接手者必读）

1. **本机代理**：这台 Windows 机器有 `HTTP_PROXY=127.0.0.1:57777`，
   会劫持所有回环 HTTP。已把 `crates/client` 的 ureq 设 `proxy(None)` 根治；
   但**测试里自己 new 的裸 ureq**（server/history_search_api、p2_messages 等）
   仍会被劫持——跑它们前 `export HTTP_PROXY= HTTPS_PROXY=` 或
   `NO_PROXY=127.0.0.1`。**这不是代码回归**（干净 main 同样红，p2 基线 0/3）。
2. **flutter test 本机不可用**：flutter tester 的回环 HTTP 被代理挡
   （"Connection closed before test suite loaded"）。只有 flutter analyze 可用。
3. **子代理配额**：ZCode 子代理有 5 小时用量上限（重置于每日 14:03）。
   大批量并行修复时注意，超限后子代理会中途死亡并留下半成品——
   务必 `git diff HEAD` 复核它们改了什么。
4. **质量闸门**：`python scripts/quality-gate.py`。基线
   `docs/bench/quality-baseline.json` 只许收紧（--update 拒绝变差）。
   基线本身落后于 main 现实——本次把 178 压到 160（净降 2）后通过。
5. **组合 CI**：改 job 要动 `ci/jobs/<组>/<job>.yml` 片段，然后
   `python ci/jobs/compose.py --check --check-live` 必须绿再提交。
   直接改 `.github/workflows/ci.yml` 会被 check-live 抓住。
6. **有用的诊断脚本**（Temp 目录，未入库，需要可重建）：
   `vs_main.py`（逐函数对比 main 找新增长/深函数）、
   `magic_diff.py`/`magic_files.py`（魔法数字逐文件逐值 diff）、
   `spans.py`、`extract_fn.py`、`find_block.py`（函数边界走查）。

## 5. 建议的接手顺序

1. `gh pr checks 288 --watch` 看最新 run；对每个红 job 取日志
   （`gh api repos/.../actions/jobs/<id>/logs`），先区分
   「本 PR 新增」vs「main 存量暴露」——判据是用干净 worktree 跑同一检查。
2. 按 §3 的分析逐个处理；android/flutter 的修复已推，很可能只差 CI 重跑。
3. coverage/desktop-package 是组合 CI 首次运行的暴露面，按 §3.3/3.4 处理。
4. 全绿后请 G00 复核 PR #288（含 AUDIT_REPORT_R5.md 的 100+100 清单）。
5. 后续大项（本 PR 未做，已有明确方案）：
   - commerce 挂载：需 `wxlike-server → wxlike-commerce` 依赖边 + commerce 增 axum
   - 浏览器 wasm：apps/desktop-web 的 selectCore 接 `WxlikeCore`（wasm-pack）
   - iOS 工程目录、OS 密钥库（token 迁出 kv）、旧 p2/ui_commands 全量 async 化

## 6. 关键文件地图

| 域 | 文件 |
|---|---|
| 新 UI 宿主桥 | `ui/src-tauri/src/core_bridge.rs`（53 个 act_* + 8 域路由 + dispatch） |
| 桥 E2E | `ui/src-tauri/tests/core_bridge_e2e.rs` |
| 死锁修复点 | core_bridge `open_session`（HTTP Mutex）；app_state `local_history`（local Mutex） |
| wasm 入口 | `crates/client-web/src/lib.rs`（WxlikeCore） |
| 契约兜底 | `crates/types/src/ids.rs`（ActionId::derive）、`crates/cmd/src/contract.rs` |
| 钱包幂等/整权 | `crates/platform/src/wallet.rs`、`qr_login.rs` |
| 授权矩阵 | `security/threat-model/authz-matrix.json`（新增 8 行） |
| CI 片段 | `ci/jobs/mobile/job.yml`（CI keystore 步骤）、`ci/jobs/build/web_v2.job.yml` |
| Android 签名 | `apps/mobile/android/app/build.gradle.kts`（key.properties 回退链） |
| 安全清单 | `THIRD-PARTY-NOTICES.md`（getrandom、wasm-bindgen/web-sys 已登记） |
