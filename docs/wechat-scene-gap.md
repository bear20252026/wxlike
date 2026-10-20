# 微信客户端 scene 目录 × wxlike 能力差距（四向研究）

> 数据来源：`../source_recovered/analysis/netscene/scenes_catalog.tsv`（308 条）与
> `../source_recovered/analysis/rtti/rtti_wechatwin.tsv`。
> 采样对象：**WeChatWin 3.9.12.56**（`work/exe1/extracted/[3.9.12.56]/WeChatWin.dll`，**32 位**，75 MB）。
> 本文所有 wxlike 侧结论都经本机 grep 复核，标注 `文件:行`。

---

## 0. 这份数据的性质与边界（先说清，避免误用）

| 项 | 说明 |
|---|---|
| 为什么可信 | scene 名来自 MSVC RTTI `TypeDescriptor` 字符串——**编译期由源程序写死的原文**，不是 Ghidra FID 猜测。对比：`work/ghidra_out/WeChatWin_named.c` 的 14 万"具名函数"里 `crypto` 类只有 4 个（见其 `slices/README.md`），那份是噪声 |
| 为什么只有 308 条 | 原始 842 条含 `::$NetSceneBaseEx` / `::V<lambda_1>` / `::UResponseData` 等编译器合成伪条目，已剥离去重 |
| **不能推什么** | ① 这是**桌面端**：移动端独有 scene 不在此列；② 支付域只有 5 条，因为 3.9 桌面支付走小程序容器，**不能据此认为微信支付能力弱**；③ 广告类 scene 是商业投放接口，与产品能力无关 |
| 不可再生性 | 4.1.15.53（Weixin.dll）RTTI 被剥离，仅剩 133 个可用 vtable → **这份对象模型只有 3.9 有** |

---

## 1. scene 目录 → wxlike 覆盖度

159 条 HTTP 路由（platform 143 / video 8 / server 8）+ 18 条 agent 命令 + 67 条 Tauri 命令为 wxlike 侧基线。

| scene 域 | 条数 | wxlike | 判读 |
|---|---:|---|---|
| `cdn_transfer` | 64 | ❌ 仅 `/api/media` 上传+下载 2 条（[media.rs:39](../crates/platform/src/media.rs)）·`/api/video` 8 条 | **最大差距**。微信把分片/秒传(Md5)/大文件/断点/校验拆成 64 个动作；wxlike 是单接口一坨 |
| `group_enterprise` | 32 | ✅ 基本齐（groups/groups_admin/perms） | 已覆盖，企业号(KeFu/Partner)分支无 |
| `miniprogram` | 23 | ❌ 容器有雏形，**0 条 RPC** | 对齐 ADR-14；`NetSceneWxaJsApiRequest`/`WeAppPCBaseComponent` 正是 publish/invoke Bridge 的服务端那一半 |
| `account_device` | 23 | ✅ register/login/2fa/sessions/devices | 缺 `GetCert`/`ListenItem` 这类引导与长连探活 |
| `voip_rtc` | 21 | ✅ call.rs 14 条 + rtc crate | 缺 `VoipStatReport`/`VoipILinkComm` 质量上报链 |
| `contact_profile` | 19 | ✅ friends/relations | 缺 `BatchGetHeadImg`/`HDHeadImg` 头像多档 |
| `finder_feed` | 16 | ⚠️ 有 live，无信息流 | 蓝图已排 P6+ |
| `telemetry_misc` | 19 | ❌ 仅 `/metrics`（hub 指标） | 客户端上报链（`Oplog`/`MulMediaReport`/`KVCommonReport`）无 |
| **`config_release_ops`** | **12** | ❌ **完全没有** | **最意外的空白**：`GetDynamicCfg`/`GetCliKVStrategy`/`ReportExptAppInfo`（动态配置+AB实验+客户端策略下发）、`CheckResUpdate`/`GetUpdateInfo`/`GetPackageList`（资源与增量升级）——没有这一层，P6 的发布工程与灰度都无从谈起 |
| `sync_roaming` | 9 | ✅ sync/unread/read | 齐 |
| `search_link` | 9 | ⚠️ 有搜索，**无链接网关** | `GetA8Key`/`CheckShortLink` = 短链与外跳安全校验，wxlike 零对应（安全含义见 threat-model） |
| `moments` | 9 | ✅ moments 7 条 | 齐 |
| `translate_accessibility` | 8 | ❌ 蓝图 P4 列了"语音转文字"，**代码 0** | `CheckVoiceTrans`/`GetVoiceTransRes`/`TranslateImageProc`/`OCRImage`/`DoImageTranslate`/`PcWebTranslateRequest`——一个域六种输入形态 |
| `favorites` | 7 | ✅ favorites 4 条 | 缺 `GetFavSizeInfo` 配额维度 |
| `sticker_store` | 6 | ❌ 能**发**贴纸，无**商店** | `GetEmotionList`/`ExchangeEmotionPack`/`BackupEmojiOperate` |
| `payment` | 5 | ⚠️ wallet 5 条 | 见 §0 边界，桌面端支付本就薄 |
| `official_account` | 2 | ❌ **无公众号/服务号概念** | 结构性缺失，非遗漏；`BizProfileV2`/`TimelineGetOftenReadBiz` |

**不该抄**：`NetSceneAd*`（6 条，广告投放）、`GetAndroidIapPackage`（渠道分成）。

---

## 2. 三个模式的实地对照（其中一个结论要更正）

### 2.1 `StageAudit` —— "状态机自带阶段审计"

二进制里 `Audit` 类**只出现在 mmtls**：`mmtls::StageAudit`、`mmtls::W4HandShakeStage::$Audit`、`mmtls::W4enCriticalAudit::$Audit`。注意 `W4` 前缀是 MSVC 对 **enum** 的标记 → 即"按阶段枚举 + 按关键事件枚举"两套内建审计点。

**来源核实（重要，勿当免责依据）**：曾假设该概念来自开源 Tencent/mars（Apache-2.0）的 stn 阶段打点。
`gh api search/code?q=StageAudit+repo:Tencent/mars` → **命中 0**；全 GitHub C++ 检索的 25 条命中全是无关字符串误配。
本二进制的 `mars::stn` 侧也没有同名类。所以 **`StageAudit` 目前是 mmtls 专有命名，没有公开出处可参照**。
结论：只可作为**概念级启发**（"状态机自带阶段审计"这个思想），实现必须独立设计；
不要按类名照搬结构，也不要在本文件之外声称"参照开源"。
顺带纠正上一轮的一处口误：本二进制 `mars::stn` 的高频虚槽来自 `XVShortLink`/`MMStnCallbackBridge`
等回调与链接类，**与 StageAudit 无关**。

wxlike 现状（grep 复核）：审计是 **HTTP 形状**的——`Platform::audit()`（[platform/src/lib.rs:445](../crates/platform/src/lib.rs)）fire-and-forget，加上只在带 `X-Wxlike-Command` 时才记录中间件（[audit_mw.rs:23](../crates/platform/src/audit_mw.rs)）。三个真正跑状态机的地方**零审计零指标**：

| 状态机 | 位置 | 实测 |
|---|---|---|
| RTC 通话 | [rtc/src/state.rs:151](../crates/rtc/src/state.rs) `apply(Event)->bool` | `grep -c "tracing::\|audit"` = **0**；非法转移塞进 `last_refused` 后**无人读取** |
| 红包生命周期 | [wallet.rs:447](../crates/platform/src/wallet.rs) `open→finished`、`:526` `→expired` | `grep -c "audit("` = **0**，只有 ledger 行 |
| 信箱/回执 | `mq_hub.rs` | 无 Platform 句柄，**不写任何审计**，错误走 `eprintln!` |

**待办（P4）**：给这三处加**转移级**记录——`{machine, from, to, stage_enum, actor, at_ms}`，与运行日志分表（不变式 7 已有此约定）。范式抄 report crate 已有的自查证做法：`Transition` 追加进 `Ticket.history` 落 `reports.history_json`（[report/src/lib.rs:111](../crates/report/src/lib.rs)），把它提级成通用件。

### 2.2 凭据三层拆分 —— **更正：我上轮对照错了对象**

上轮说"对照 `core::crypto::Identity` 把密钥与凭据混在一起"——**那个类型是无人引用的脚手架**（workspace 内 grep `core::crypto|crypto::Identity|crypto::seal` 除自身文件外 **0 命中**，[core/src/crypto.rs](../crates/core/src/crypto.rs) 自述 "Scaffold level"）。产品路径是 **vodozemac 的 [core/src/e2ee.rs](../crates/core/src/e2ee.rs)**。

真正可对照的是**缺失的持久层**，四条都有二进制侧的对应物：

| wxlike 缺口（已 grep 复核） | mmtls 对应分层 |
|---|---|
| Olm `Account` **不落盘**：`to_blob/from_blob/pickle/Zeroize` 全 workspace **0 命中**；identity key 随进程死，每次重启重发 bundle（`ui/src-tauri/src/lib.rs:71-73` 自述"key persistence lands with P4"） | `CredentialStorage` / `ClientCredStorage` —— 存储是独立一层，不混在握手里 |
| `prekey_bundles.updated_at` **写了但从不当过期读**（[keys.rs:68](../crates/platform/src/keys.rs)） | `NewSessionTicket` + `Psk` —— 凭据自带生命周期 |
| 秘密聊天失败**静默降级明文**（`ui/src-tauri/src/lib.rs:776-780`） | `Alert` + `enCriticalAudit` —— 降级是关键事件，必须上报 |
| 服务端会话无轮转：`sessions` 表只有惰性过期、无 refresh、无滑动续期（[lib.rs:355](../crates/platform/src/lib.rs)） | `ClientPsk` / `HandShakeState` —— 恢复与重签分离 |

**待办（P4）**：① Account pickle + `Zeroize`；② bundle TTL 真正生效 + 陈旧驱逐；③ "降级明文"改为显式失败或 `e2ee.degrade` 审计事件；④ 会话轮转/滑动续期。

### 2.3 `HttpHandler` —— 载体兜底，但 wxlike 的问题更基本

`mmtls::HttpHandler` 也是 `DataPackInterface` 实现者（虚槽在 `0x1300eae0`/`0x1300f100`/`0x1300eb10`）——**握手报文可以直接封进 HTTP**，用于代理/仅 80·443 出站的环境。

wxlike 侧：**没有 `Transport` trait**（ADR-8 的说法与代码不符）。实际抽象是两个单方法 trait `FrameTx`/`FrameRx`（[net/src/conn.rs:22-31](../crates/net/src/conn.rs)），4 个实现（QUIC×2 + WS×2），QUIC 用长度前缀、WS 用"一条二进制消息=一帧"。

复核出的三个硬问题，比"补 HTTP 兜底"优先级更高：
1. **库里没有 WS 客户端**：`client.rs` 直接持 `quinn::SendStream/RecvStream`，`grep -rn "WebSocket\|tungstenite" crates/net/src/client.rs` = 0 → `conn.rs:14-15` 宣称的"换载体不动其他层"**只对入站成立**。
2. `/endpoints` 只广播 `quic` + `alpn`，**不广播 ws URL**（[server/src/lib.rs:232-239](../server/src/lib.rs)）→ 客户端无法协商载体。
3. `net/src/transport.rs` 名字误导：它是 QUIC/rustls 证书+TOFU，不是抽象层（`lib.rs:13/26` 却如此 re-export）。

**待办（P4）**：先补 WS 客户端 + `/endpoints` 广播与载体协商（这是"能降级"的前提），再谈 HTTP 长轮询/报文封装。**不要**在没打通 WS 客户端时加第三种载体。

---

## 3. 建议的 P4/P5 增量（按杠杆排）

1. `config_release_ops` 空白 → 新增动态配置+AB 上报+资源升级一组接口（**12 条 scene 的域，P6 发布工程的前置**）。
2. CDN 传输面 → 把 `/api/media` 拆出分片/秒传/校验（差距条数最大，64 vs 2）。
3. `translate_accessibility` → 语音转文字 + 图片翻译（蓝图已承诺，成本可控）。
4. 审计三层落点（§2.1）+ 凭据持久层四件（§2.2）——**这两条是真 bug 级，不是新功能**。
5. WS 客户端 + 载体协商（§2.3）——多设备 Web 端可用性前提。
6. `sticker_store`、`favorites` 配额、`search_link` 外跳校验（`A8Key` 类比）——按 P5 排。
7. 公众号体系：结构性缺失，需单独立项讨论，不进 P4。
