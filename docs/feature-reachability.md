# 特性域可达性账（issue #295 · G00 · P1）

> 2026-10-28，基线 `1bb72786`。
>
> 这份账只回答一个问题：**五个特性域离"跑在 server 里"还差哪几行**。
> 本轮**不挂任何路由、不改任何 Cargo.toml**——仓库自己的规矩是：入口实现由域在自己的卡里
> 先落地，G00 才加依赖边与路由（`docs/full-platform-delivery/00_master_guide.md:184`
> 「根文件改动写 `integration-requests/Gxx/<task>.yaml`，含依赖、route 挂载、command 挂载、
> 配置、migration 提案；G00 在对应串行集成 PR 执行」，同义条款见
> `docs/full-platform-delivery/teams/G00.md:77`、公共文件归属见
> `docs/full-platform-delivery/00_master_guide.md:153`）。
>
> 所以本账的价值不在"谁做完了"，而在**每一格都能被命令复验**。判定只认代码，
> 每条都带 `文件:行号`；跑不动、看不见的，一律写 **未证实**，不猜。

---

## 一、判定口径

| 判定 | 含义 | 判据 |
|---|---|---|
| `READY_TO_MOUNT` | 入口实现已落在域自己的 crate 里，G00 可以按集成请求装配 | crate 内有真实 axum `Router` 构造点且被本 crate 测试打过 |
| `NEEDS_PROVIDER` | 入口实现不存在，G00 **不得**先加依赖边 | 无 `Router`／只有命令分发函数／只有 dev 工具 |
| `DUPLICATE_OF:<位置>` | 同一能力在别处已挂载，直接挂会出现两份事实 | 路径或数据表与已挂载实现重叠 |
| `NOT_NEEDED` | 这个域按设计不进 server 组合根 | 域卡的集成请求本身声明 `router: none`，且能力面在宿主/客户端侧 |

重叠但不同时，主判定按上表取，重叠风险写在同一行的 `DUPLICATE_OF:` 后缀里——
**不把"重复"当"缺失"混过去，也不把"存在文件"当"存在入口"**。

---

## 二、运行中的 server 到底挂了什么（唯一事实来源）

`server/src/lib.rs:438-467` 的 `build_app` 是**全部**挂载点，一个不多：

| # | 挂载 | 出处 |
|---|---|---|
| 1 | `/health`、`/metrics`、`/endpoints` | `server/src/lib.rs:439,443-449,452-459` |
| 2 | `wxlike_platform::Platform::router` | `server/src/lib.rs:460` → `crates/platform/src/lib.rs:468-512` |
| 3 | `wxlike_video::routes` | `server/src/lib.rs:461` |
| 4 | ws / presence / history / web 静态 / dev / turn | `server/src/lib.rs:462-467` |

`server/Cargo.toml:12-22` 的依赖闭包只有
`net/mq/platform/message/risk/moderation/report/core/push/video/search`
（**+ `media`，2026-10-27 挂上，见下**）。根 `Cargo.toml:3` 把这些域列为 workspace 成员
（`crates/features/{miniapps,commerce,identity,media,relations,security,discovery}`），
所以它们**天天被 CI 编译和单测**（`ci/jobs/policy/paths.json:18` 的 `crates/**` 触发 Rust 作业），
但除了 media 之外**没有任何运行路径**。「测试绿」≠「线上可达」，这是本账存在的原因。

零反向依赖，可复验（本轮实跑，`--offline`）：

```text
$ cargo tree --offline -i -p wxlike-commerce   -e normal,dev
wxlike-commerce v0.1.0 (…/crates/features/commerce)     ← 只有它自己
$ cargo tree --offline -i -p wxlike-discovery  -e normal,dev   同上
$ cargo tree --offline -i -p wxlike-miniapps   -e normal,dev   同上
$ cargo tree --offline -i -p wxlike-security   -e normal,dev   同上

# media 是 2026-10-27 起唯一有运行路径的那个（#295 后半，PR #303）：
$ cargo tree --offline -i wxlike-media -e normal,dev
wxlike-media v0.1.0 (…/crates/features/media)
└── wxlike-server v0.1.0 (…/server)                      ← 真的被 server 依赖了
```

对照：`wxlike-identity`、`wxlike-relations` **不是**孤儿——`crates/platform/Cargo.toml:17-18`
依赖它们，建表在 `crates/platform/src/db.rs:53-54`，路由在 `crates/platform/src/lib.rs:479`。
这两条正是 G02 走通的正确姿势，本账拿它当基准。

---

## 三、总账（一行一域）

| 域 | crate 路径 | 有没有真实入口（file:line） | 谁依赖它 | 要挂进 server 缺什么 | 对应开发卡 | 当前判定 |
|---|---|---|---|---|---|---|
| 交易/卡券 | `crates/features/commerce/` | **没有 HTTP 入口**。`src/http.rs:22` 是 `dispatch(&Commerce, actor, action, payload)` 的 match 表（43 个动作，`src/http.rs:25-67`）+ `src/http.rs:76` `webhook()`；全 crate `grep axum` **零命中**，`Cargo.toml:7-16` 无 axum | 无（`cargo tree -i` 只返回自己；`server/Cargo.toml:12-22` 无此路径） | ① 域内先写 axum 层（现在没有）② axum 依赖 ③ server→commerce 依赖边 ④ **鉴权**：`dispatch` 的 `actor` 是入参，契约明令"由 session 推导，禁止从 payload 读取"（`contracts/domains/commerce/actions.yaml:15`）⑤ **风控/审计不落**：`risk_mw::gate` 与 `command_audit` 挂在 `Platform::router` 内层（`crates/platform/src/lib.rs:505-511`），server 顶层不套任何 layer（`server/src/lib.rs:438-467` 无 `.layer(`）⑥ **迁移未登记**：`schema/registry/migrations.json:11-21` 只到 M008，commerce 的 19 张表（`src/schema.rs:37-327`）无编号；且 `schema`/`store` 是私有模块（`src/lib.rs:21-22`），建表只在 `Commerce::open` 内部发生（`src/store.rs:14-17`），**与 G06-01 请求里"与业务表共用同一连接"矛盾**（`integration-requests/G06/G06-01.yaml:38`）⑦ 幂等：写动作要求 payload 带 `idempotency_key`（`src/http.rs:106-108`），而 server 侧现有口径是 **`Idempotency-Key` 头**（`crates/platform/src/wallet.rs:634-636`、表 `wallet_idem` 见 `crates/platform/src/db.rs:75-82`），两套要先归一 | G06-01 #127 / G06-02 #130 / G06-03 #133 / G06-04 #134 / G06-05 #137 | **`NEEDS_PROVIDER`**（另注 `DUPLICATE_OF:crates/platform/src/wallet.rs:53-58` 的转账/红包/余额/流水——已挂载且线上在用，挂 commerce 前必须先定替换或分层） |
| 发现/扫码 | `crates/features/discovery/` | **没有**。全 crate `grep -ri axum`、`grep -rn Router` 均零命中；只有域服务（`src/geo.rs:106` `GeoService`、`src/matching.rs` `ShakeMatcher`、`src/qr/`）。卡自己承认：`integration-requests/G07/G07-01.yaml:25`「**本分支未实现 HTTP 层**；前缀为契约占位，由 G00-04 按模板装配」，`:27,29` 工厂位写死 `PENDING_G00_ASSEMBLY` | 无；且**只有本 crate 的测试引用它**（`tests/contact_qr.rs:5-10`、`tests/geo_matching.rs:4-6`、`tests/location_share.rs:4`） | ① HTTP 层 ② **持久化**：crate 无 rusqlite 依赖（`Cargo.toml:7-21`），`GeoService` 的 presence 是 `Mutex<HashMap>`（`src/geo.rs:109,121`）——重启即清空；`schema/proposals/G07/discovery.sql` 只是提案（`integration-requests/G07/G07-03.yaml:23-24`）③ **防重放**：`NonceStore` 只有 `MemoryNonceStore`（`src/crypto.rs:39,45`），必须落库版 ④ 请求里声明的 `wxlike_discovery::register_commands` **不存在**（全 crate grep 零命中，`integration-requests/G07/G07-01.yaml:30` 注明"待 G00 装配时补该符号"——**这个动作归 G07 不归 G00**）⑤ 迁移编号 ⑥ MAP 供应商 `BLOCKED_EXTERNAL_OR_NOT_YET_VERIFIED`（`docs/full-platform-delivery/provider-register.csv:13`） | G07-01 #136 / G07-03 #140 / G07-08 #142 / G07-09 #138 | **`NEEDS_PROVIDER`** |
| 对象/文件/转码 | `crates/features/media/` | **有，且是唯一的真入口样本**：`src/http.rs:101` `MediaApp::router()`，`src/http.rs:104-131` 28 条 `/api/v1/...` 路由，`Cargo.toml:11` 有 axum，`src/http.rs:139` 自己实现 `FromRequestParts` 主体抽取；`tests/http_api.rs` 用 `tower::ServiceExt` 打真路由（`:68,79,121,245`）。`/api/v1` 前缀只有 media 一家在用（`crates/`+`server/`+`apps/` grep 无第二处），挂上去不与现有面撞路径 | 无（同上空树） | ① server→media 依赖边 + 组合根装配（请求已写好：`integration-requests/G04/G04-01.yaml:20-25` REQ-G00-server-mount）② 迁移编号：schema 直接内嵌提案文件 `src/db.rs:15` `include_str!("../../../../schema/proposals/G04/0001_media.sql")`，开库即建表（`:47`），**`schema/registry/migrations.json` 里没有它**（REQ-G00-migration-number，`G04-01.yaml:26`）③ 生产鉴权：`SessionVerifier` 是端口（`src/http.rs:46-48`），现成实现只有 `StaticBearer`/`fixture_verifier`（`:63-78`、`:677`）——**开发配置，不能上生产**；请求把它派回 G02（`G04-01.yaml:36`）④ `AudiencePort` 同样待 G02/G03 真实现（`G04-01.yaml:40`）⑤ 密钥 `WXLIKE_MEDIA_SECRET`：deploy/containers/ci 全量 grep **零命中**，即装配起来会缺配置（REQ-G00-secret，`G04-01.yaml:30`）⑥ 生产对象存储只有 `LocalObjectStore`（`src/lib.rs:54`），REQ-G00-object-store（`G04-01.yaml:44`）⑦ 与已挂载的 `/api/media` 三路由并存（`crates/platform/src/media.rs:47-51`），需定退役关系 | G04-01 #117 / G04-02 #119 / G04-03 #120 / G04-04 #122 / G04-05 #125 | **`MOUNTED`**（2026-10-27 #295 后半，PR #303：`server/src/media_mount.rs` 挂 `MediaApp::router`，鉴权复用 `Authed::bearer_from` + `Platform::lookup_token` 那一张 `sessions` 表，`StaticBearer`/`fixture_verifier` 不再被引用；迁移登记为 **M009**；`WXLIKE_MEDIA_SECRET` 改成没配就拒绝启动，装配处另有 ≥32 字节长度闸）**——剩余的 ⑥ 生产对象存储仍是 `LocalObjectStore`、⑦ 与已挂载的 `/api/media` 三路由的退役关系未定，属生产前置，不阻塞"能跑" |
| 小程序 | `crates/features/miniapps/` | **server 侧没有，且设计就不该有**：`src/` 下 `axum`/`Router` 零命中；唯一"服务"是回环 dev 服务器 `src/toolchain.rs:257` `DevServer`（`serve`/`serve_once`，`:305,314`）与开发 CLI `src/bin/mini.rs`（`dev-serve` 子命令 `:61,138`）。三张卡的请求都显式写 `router: none`（`integration-requests/G08/G08-01.yaml:19`、`G08-04.yaml:52`、`G08-05.yaml:50`） | 无（同上；`src/bin/mini.rs` 只是本 crate 的 bin target，`Cargo.toml` 无 `[[bin]]`，靠自动发现） | server 侧**无需**装配。真缺口在别处：① 目录命令要挂宿主命令面，请求里 owner 写的是 **G11**、文件 `ui/src-tauri`（`G08-04.yaml:43-51`）② `CatalogSource` 生产实现按 `src/catalog/source.rs:3-6` 的说法来自"server-side published catalog (G01/G02)"，但**没有任何卡认领这个服务端发布方**，crate 里只有 in-memory double ③ crate 无 rusqlite（`Cargo.toml:7-16`），安装态是文件系统（`G08-04.yaml:61-66`） | G08-01 #141 / G08-04 #149 / G08-05 #150 | **`NOT_NEEDED`**（server 侧）；`<- 未指派`：服务端目录发布方无卡，建议 G00 另开 |
| 安全/密钥 | `crates/features/security/` | **没有**：无 axum（`Cargo.toml:7-17`）；`src/device_keys.rs:101,175,211,230,290,338,351` 全是 `fn(&Connection, …)` 域函数。但 `integration-requests/G02/G02-03.yaml:14-20` 把这些**域函数直接当 handler 登记**为 `router_mount`——这就是"文件在≠入口在" | 无（同上空树）。`tests/security/Cargo.toml:2` 那个 `wxlike-security-tests` 是**独立 workspace**（`:8-13` 注释说明），依赖的是 platform/video/dpage/container/risk/core，**不依赖本 crate** | ① HTTP 适配层（把 `&Connection` 域函数变成有主体、有错误形状的路由）② **路径冲突**：请求要挂 `POST /api/keys`（`G02-03.yaml:17`，并写明"替换 platform::keys 的每用户路径"），而 `/api/keys` 已挂载在线（`crates/platform/src/keys.rs:66-68`，模型是**每用户一份** `prekey_bundles`；security 是**每设备一份** `security_device_key`，见 `src/lib.rs:56-68`）——替换/并存/双挂三选一必须先由 G02 定 ③ 迁移：`src/lib.rs:94` `pub fn migrate(conn)` 存在且幂等（`:55-91`），但 `crates/platform/src/db.rs:46-56` 的建表序列里没有它，`schema/registry/migrations.json:11-21` 也没有 → **规则只有一份、执行点为零** | G02-03 #111 / G02-04 #113 / G02-05 #114 | **`NEEDS_PROVIDER` + `DUPLICATE_OF:crates/platform/src/keys.rs:66-68`** |

---

## 四、为什么不能只看文档

### 4.1 commerce：一句"已备好"把 0 说成 1

`AUDIT_REPORT_R5.md:293` 原文：

> **commerce 挂载**：需要 `wxlike-server → wxlike-commerce` 依赖边 + commerce 的 axum 依赖
> （本轮禁改 Cargo.toml；**代码已在 `crates/features/commerce/src/http.rs` 备好**）。

打开那个文件，括号里的暗示不成立：

- `src/http.rs:22` 的签名是 `pub fn dispatch(commerce: &Commerce, actor: &str, action: &str, payload: &Value) -> Result<Value>`
  —— 一个按字符串动作名查表的函数，**没有 handler、没有 `Router`、没有状态注入**。
- `crates/features/commerce/src/http.rs:1-5` 的模块头自己就写着：「这里只做参数解析、鉴权主体透传与稳定 JSON 形状；
  **HTTP 路由、鉴权中间件与 OpenAPI 由 G00 组合根挂载**」——域卡把写路由这件事推给了 G00。
- `crates/features/commerce/Cargo.toml:7-16` 无 `axum`；`grep -ri axum crates/features/commerce/` **零命中**。
  即"只缺 Cargo 依赖边"这个说法连累带漏了"还缺整个 HTTP 层"。
- 契约文件同样明说这不是路由清单：`contracts/domains/commerce/actions.yaml:3-4`
  「来源：`crates/features/commerce/src/http.rs::dispatch`。**G00 负责把 action 挂到路由/命令注册表**；
  本文件只声明语义，不是路由清单」。

于是缺的不是"两行 Cargo.toml"，而是**一整个入口实现 + 一个未决定的归属**：
按 `00_master_guide.md:184` 的规矩，路由该由 G06 先落，而 `G06-01.yaml:23-30` / `G06-05.yaml:11-42`
把 `/api/v1/commerce/actions/<action>` 的装配写进了 G00 的请求里。**规矩与被挂载的假设互相矛盾，
必须先裁定归属，再谈依赖边。**

对照组：同一天交付的 `crates/features/media/src/http.rs:101` 是**真** `Router`，
`crates/features/media/ACCEPTANCE.md:5-6` 写"axum HTTP 层"、`:12` 逐卡列实现，
文档与代码一致。可见这不是风格问题，是 commerce 单独越过了事实线。

### 4.2 文档说的"mounted"是 UI 壳，不是 server 路由

R5 的 PR 正文（提交 `dd353c5d`，PR #288）写：
「desktop-web: **7 orphaned domains mounted** (video/wallet/files/mini/discover/moments/calls)」。
"mounted" 指的是 `apps/desktop-web/src/features/{commerce,files,mini,discover,…}` 进了前端路由壳。
server 侧对应路径一条都不存在（`grep -rn '"<路径>' crates/ server/ --include=*.rs` 全空）：

| UI 调用 | 出处 | server 侧 |
|---|---|---|
| `POST /api/payment-intents`、`/api/payments/{id}/refund`、`/api/withdrawals`、`/api/topups`、`/api/cards`、`/api/entitlements` | `apps/desktop-web/src/features/commerce/api.ts:118,125,139,146,154,162,166` | **404** |
| `/api/files*`、`/api/backups/plan*` | `apps/desktop-web/src/features/files/api.ts`（见文件头 `:8-12`） | **404** |
| `/api/miniapps*`、`/api/mini-grants*`、`/api/mini-installs*` | `apps/desktop-web/src/features/mini/api.ts` | **404** |
| `/api/services`、`/api/orders*`、`/api/quotes`、`/api/vision/*` | `apps/desktop-web/src/features/discover/api.ts` | **404** |

必须说清：这四份前端**不是造假**——`api.ts` 文件头自己标了
"Contract-gated（上游未开通）"（`commerce/api.ts:7-9`、`files/api.ts:8-12`、`mini/api.ts:3-6`、
`discover/api.ts:2-4`），并有 `isUpstreamMissing()` 把 404/501 原样渲染成上游状态
（`commerce/api.ts:170-172`）。**要纠正的是文档侧的 "mounted" 措辞**，它让读者以为链路已通。
`AUDIT_REPORT_R5.md:287`「7 域挂载 | `apps/desktop-web/src/{app.tsx,shell/router.tsx,…}`」同样只列前端路径。

### 4.3 中间件不是免费的：顶层 merge 拿不到风控与审计

`crates/platform/src/risk_mw.rs:1-16` 的立项理由就是"漏掉是结构性必然"，
它的 (method, path) 表（`action_for`，`:37-45` 起）与 `command_audit` 只覆盖 `Platform::router` 内层
（`crates/platform/src/lib.rs:505-511`）。`build_app` 没有全局 layer
（`server/src/lib.rs:438-467`，无 `.layer(`）。所以谁在顶层 merge 一个新写面，
那个写面就**默认不过风控闸门、不落命令审计**——`/api/v1/commerce/actions/*` 与 `/api/v1/uploads`
都会踩到这条。这一格必须进每域的挂载清单，不能"挂上了"就算完。

---

## 五、必须先关的卡（G00 的挂载前置）

**共同前置（不关，任何域都别挂）**

| 卡 | 为什么挡路 |
|---|---|
| G00-03 契约版本与组合根冻结 #101 | 裁定"入口由域自己落、G00 只装配"这条与 `G06-01.yaml:23-30`/`G06-05.yaml:11-42` 的冲突；它同时是迁移编号与 `integration-requests/G00/G00-03.yaml` 的所有者 |
| G01-01 共享类型与 ports #102 | `composition/action-registry.rs:1-8` 至今是**未挂载的自包含文件**（REQ-G01-01-mount-registry，`integration-requests/G00/G00-03.yaml:13-20`）；commerce/discovery 请求里的 `register_commands` 命令面没有落点 |
| G00-04 服务端数据库生产迁移 #104 | `schema/registry/migrations.json:11-21` 只到 M008、`next_id` 写 M009；五域的表一张都没登记，PG 切换也归它（`G06-01.yaml:52` 把 G00-04 列为验收依赖） |
| G02-01 账号会话与设备生命周期 #103 | 挂路由就要有"主体从 session 推导"的来源，两个域在请求里点了名：commerce（`integration-requests/G06/G06-01.yaml:51`「G02-01 (#103) 真实身份主体，用于把 actor 从 session 推导」、`G06-05.yaml:71-73`）与 media（`G04-01.yaml:36-39`，target 只写 `G02`）。security/discovery/miniapps 无 HTTP 面，暂时轮不到它们 |

**分域前置**

| 域 | 还必须先关 |
|---|---|
| commerce | G06-01..05（#127/#130/#133/#134/#137）**新写一张"HTTP 层"子任务**（或裁定 G00 例外代写）；与 `crates/platform/src/wallet.rs:53-58` 的重叠结论；PAYMENT 供应商 #130/#133 现为 `BLOCKED_EXTERNAL_OR_NOT_YET_VERIFIED`（`docs/full-platform-delivery/provider-register.csv:2`） |
| media | G02-01 #103 + G02-02 #108 + G03-03 #121 —— 请求只写 `target: G02` / `target: G02/G03`（`G04-01.yaml:36,40-43`），**具体卡片是按 `SessionVerifier`/`AudiencePort` 的能力推定的，未证实**；OBJECT_STORAGE #117 为 `BLOCKED_EXTERNAL_OR_NOT_YET_VERIFIED`（`provider-register.csv:15`）；`/api/media` 退役口径 |
| discovery | G07-01 #136 + G07-03 #140（HTTP 层 + 持久化 + `register_commands` 符号）；MAP #140 |
| miniapps | G08-04 #149 的服务端目录发布方**目前无卡**；宿主命令面归 G11（`G08-04.yaml:43-51`） |
| security | G02-03 #111（HTTP 层 + 路径冲突裁定）；G02-04 #113、G02-05 #114 |

---

## 六、复验命令

```bash
# 1) 零反向依赖（本账的核心断言）
for p in commerce discovery media miniapps security; do
  cargo tree --offline -i -p wxlike-$p -e normal,dev; done   # 期望：只打印它自己

# 2) server 只挂这些（对照 §二）
sed -n '438,467p' server/src/lib.rs
grep -n "wxlike-" server/Cargo.toml

# 3) "有 axum" 只有 media 一家
for p in commerce discovery miniapps security media; do
  printf '%s: ' $p; grep -ric axum crates/features/$p/Cargo.toml; done

# 4) UI 调了哪些 server 不存在的路径
grep -rn '"/api/\|`/api/' apps/desktop-web/src/features/{commerce,files,mini,discover}/api.ts

# 5) 迁移登记表覆盖到哪儿
python -c "import json;print(json.load(open('schema/registry/migrations.json',encoding='utf-8'))['next_id'])"

# 6) 闸门
typos && python scripts/quality-gate.py
```

## 七、本轮结论与待纠正清单（不改他人文档，只登记）

- 五域**没有一个是"只差依赖边"**：`READY_TO_MOUNT` 只有 media 一个；
  commerce / discovery / security 为 `NEEDS_PROVIDER`；miniapps 在 server 侧 `NOT_NEEDED`，
  真缺口是无卡认领的服务端目录发布方。
- `AUDIT_REPORT_R5.md`（归其它组纠正，本轮**未改**）待纠正两处：
  1. `:293` "代码已在 `crates/features/commerce/src/http.rs` 备好" → 应为
     "命令分发表已备好，**axum 路由/鉴权中间件/OpenAPI 未实现**（该文件 `:1-5` 自述、无 axum 依赖）"；
  2. `:44` 与 `:287` 的"挂载/已解决"口径 → 需注明只覆盖前端壳与 `/api/wallet` 老面，
     `/api/v1/commerce/*` 与 `/api/v1/uploads` 在 server 侧仍无注册点。
- R5 PR 正文（#288，提交 `dd353c5d`）"7 orphaned domains mounted" → 需限定为
  "mounted **into the desktop-web shell**"，并列明 §4.2 表中所列路径当前在 server 侧无任何注册点。
- 附带发现（不在 #295 范围，登记给 G00）：
  `crates/admin` 同为无入边的 workspace 成员（根 `Cargo.toml:3` 列了它），而且**它和 media 一样已经有真 `Router`**（`crates/admin/src/http.rs:143` `pub fn router`；`cargo tree -i -p wxlike-admin` 同样只返回自己，`crates/admin/Cargo.toml:2,8`），却不在 `server/Cargo.toml:12-22` 里；
  `crates/platform/src/db.rs:53-56` 的 `migrate_identity` / `migrate_relations` / `migrate_round5`
  三段建表**未登记进** `schema/registry/migrations.json`（`:11-21` 止于 M008）——
  登记表与执行点已经对不上，这是比"某域没挂"更早的债。
