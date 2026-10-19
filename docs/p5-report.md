# P5 进度报告：直播控制面 + 小程序包签名（第一增量）

> 合同第五条 P5 是 8 周量级（Flutter 三端 / 直播 / 小程序 M1–M5 / 点播完整版 /
> Web 工程 / 多语言 / 数据导出 / 国内厂商推送）。本文件记录**已交付的第一增量**
> 与**明确的环境受限项**，不做"已全部完成"的表述。

## 1. 本轮交付（可执行、可验证）

| 交付 | 代码 | 测试 | 状态 |
|---|---|---|---|
| video 多分辨率阶梯（360/720/1080 + master playlist） | `crates/video/src/lib.rs` | 1（真实转码） | ✅ |
| 直播会话控制面（开播授权/停播/状态） | `crates/platform/src/live.rs` | 9 | ✅ |
| RTMP 推流凭据（HMAC 自包含、过期/篡改/停播即拒） | 同上 | 同上 | ✅ |
| 观众计数与峰值 | 同上（`live_viewers`） | 同上 | ✅ |
| 弹幕路由（只发在房观众，不落库、不占序列号） | 同上 | 同上 | ✅ |
| 小程序包签名（ed25519）+ 防降级 + 权限白名单 | `crates/container/src/package.rs` | 7 | ✅ |
| 消息索引：**借用 tweb `SlicedArray`**（历史分页合并 + 实时插入） | `ui/src/vendor/tweb/slicedArray.ts` · `ui/src/msgIndex.ts` | 7（node 断言） | ✅ |
| **通话房间 HTTP 控制面**（开播/进出/静音/全员静音/踢人/锁门/录制/移交/举手/出口分配） | `crates/platform/src/call.rs` · `crates/rtc/src/{room,sfu}.rs` | 13 | ✅ |
| 同上 **HTTP 层契约**（204/200/403/404/409/422 的真实状态码） | `crates/platform/tests/call_http.rs` | 3 | ✅ |
| **通话页 UI**（参与者网格 + 底部控制条 + 参与者菜单 + 4 s 轮询） | `ui/src/callpage.ts` · `ui/src/styles.css` | tsc + esbuild | ✅ |
| **设备验证页**（12 组安全码 + 身份指纹 + 标记已验证） | `ui/src/callpage.ts` · `ui/src/api.ts` · `ui/src-tauri/src/lib.rs` | tsc + esbuild | ✅ |
| **权限矩阵**（Rocket.Chat 的 roles × permissions：9 个权限点 / 3 个角色；提权仅群主、同级不可互踢） | `crates/platform/src/perms.rs` | 10 | ✅ |
| **通话信令中继**（offer/answer/ICE；`from` 强制改写、收件人必须在房、队列 64 条有界丢最旧） | `crates/rtc/src/{signal,room}.rs` · `crates/platform/src/call.rs` | 6（平台 5 + HTTP 1） | ✅ |
| 信令桥（Tauri 命令 + TS 封装，前端只透传 SDP） | `ui/src-tauri/src/lib.rs` · `ui/src/api.ts` | tsc + esbuild | ✅ |

**新增测试 17 项**（合同 P5 下限 15）。

```bash
cargo test -p wxlike-platform --test p5_live     # 9
cargo test -p wxlike-container --test package    # 7
cargo test -p wxlike-video -- --ignored          # 4（真实 ffmpeg 转码，含多档阶梯）
```

### 1.1 直播控制面

```bash
# 开播（频道必须 kind=live，调用者必须是社群 owner/admin）
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/<channel>/start \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"recording":true}'
# {"session":{...,"stream_key":"...","state":"live"},
#  "ingest_token":"<exp>.<hmac>","ingest_url":"rtmp://127.0.0.1:1935/live/<key>"}

# RTMP on_publish 回调（ingest 节点持共享 secret，无用户 token）
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/ingest/authorize \
  -H 'content-type: application/json' \
  -d '{"stream_key":"<key>","token":"<exp>.<hmac>"}'      # 204 / 403

# 观众进出与弹幕
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/<channel>/join  -H "authorization: Bearer $TOKEN"
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/<channel>/danmaku \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"text":"开场了"}'
# {"delivered":2}   ← 在房观众数（不含发送者）

# 停播（主播或社群管理员）；停播后凭据立即失效
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/<channel>/stop -H "authorization: Bearer $TOKEN"
```

三条关键断言（测试覆盖）：

1. `stop` 后即使 `ingest_token` 未过期，`verify_ingest` 也拒绝——凭据有效性同时
   取决于签名与**会话状态**；
2. 弹幕走 `high_priority`（`live.danmaku` 已在类型注册表登记），测试直接断言
   频道时间线为空（不变式 3）；
3. 弹幕过内容管线：硬违规拒绝、超长/空串拒绝。

### 1.2 小程序包签名

```rust
// 发布方签名（CI 持有私钥）
let sig = sign_manifest(&manifest_bytes, &signing_key, "k1");
// 宿主验签 + 安装
let granted = registry.install(&manifest_bytes, &sig)?;
```

- 签名覆盖 manifest 原文，改任一字段即验签失败；
- 注册表拒绝**同版本重复**与**版本降级**（签名合法也不行——旧版漏洞包无法回滚覆盖）；
- `im_bridge` 授予面 = 清单声明 ∩ 宿主白名单（`im.send_message` / `im.profile.read` /
  `im.conversation.read` / `pay.request`），白名单外权限记录在 `denied_permissions`
  供审计，但**永不授予**（不变式 6）；
- 清单校验拒绝 `../` 入口路径与非法版本号。

## 2. 未完成项与阻塞原因

| P5 条目 | 状态 | 原因 / 下一步 |
|---|---|---|
| Flutter Android/iOS + 鸿蒙（UniFFI 绑定） | ⛔ 未开始 | 本机无 Android SDK / Flutter / 鸿蒙工具链，无法产出可验证安装包；Rust 侧 API 面已稳定，绑定层可在有工具链的机器上一次性生成 |
| Web 独立工程 | ⛔ 未开始 | 与桌面端共用 `api.ts` 契约，建议直接抽取共享包后建 Vite 工程 |
| 直播媒体面（RTMP ingest + HLS 分发 + 连麦） | 🟡 控制面完成 | ingest 走外部 Xiu 进程、HLS 走静态分发；本仓库只提供授权/凭据/状态，不在进程内跑媒体服务器 |
| video-service 多分辨率 | ✅ 已完成 | 360/720/1080 阶梯（只降不升）+ master playlist + 档位分片路由；真实 ffmpeg 用例 `ladder_produces_multiple_renditions` 验证 720p 源产出 360/720 两档 |
| 小程序 M1–M4（逻辑层/Bridge/多引擎/沙箱） | 🟡 M5 签名层完成 | 逻辑层已有 Boa 宿主与 DPD1 装载；Wasmtime 沙箱未接 |
| 多语言 / 数据导出 | ⛔ 未开始 | 数据导出建议随 P6 账号注销一起做（同一份 dump） |
| APNs / 国内厂商推送通道 | ⛔ 未开始 | 需要厂商证书；适配器可先按 FCM 客户端形状写，证书由部署方注入 |

## 2.5 Telegram 代码借用（首次实际复制）

按"能搬不重写"的要求，本轮首次实际复制第三方源码：

- **来源**：`refs/tweb`（morethanwords/tweb，GPL-3.0，与本项目同许可）
  `src/helpers/slicedArray.ts` → `ui/src/vendor/tweb/slicedArray.ts`
- **借的是什么**：降序分块数组（`SlicedArray`/`Slice` 的 ends 追踪 + `insertSlice`
  跨页合并）。这正是消息列表虚拟化的核心结构，自研等于重犯它的边界。
- **替换掉的自研代码**：`ui/src/api.ts` 里手写的 `mergeMessages`（`Map` + `sort`，
  每页 O(n log n)）。现在历史分页与实时消息都走 `MessageIndex`
  （`ui/src/msgIndex.ts`），同 seq 覆盖、跨页去重、乱序插入由 tweb 的切片完成。
- **适配**（三处，写在文件头）：去掉 `MOUNT_CLASS_TO` 调试挂载；内联
  `indexOfAndSplice` 与 number-only `compareValue`；按本项目 `strict` 补变量初始化。
- **回归**：`bash ui/scripts/check-msgindex.sh`（esbuild 打包 + node 断言 7 条：
  乱序页、历史页、实时插入、同 seq 覆盖、幂等、窗口取尾）。

第二轮借用（同一来源，`ui/src/vendor/tweb/listenerSetter.ts`）：

- **借的是什么**：`ListenerSetter`（监听器集中登记 + `removeAll` 一次性清理）。
- **修掉的问题**：消息菜单与贴纸面板各自手写 `document.addEventListener`
  + 命名函数自清；菜单被切视图/重渲染关掉时监听器会残留，越积越多。
  现在两处都登记到同一个 `menuListeners`，`hideMsgMenu()` 统一清理。
- **适配**：去掉 tweb 内部类型 `EventListenerBase`；`strict` 下补
  `let listener: Listener | undefined`。

## 3. 设计说明（为什么这样切）

- **弹幕不进时间线**：合同不变式 3 规定只有 normal 通道占用序列号。弹幕高频、
  无历史价值，落库只会把频道序列号打爆。用 `high_priority` + 在房观众收件集，
  离线者拉历史时看不到三小时前的弹幕——这正是产品预期。
- **推流凭据自包含**：ingest 节点（Xiu）与平台可能不在同一进程/机器，回调验签
  不能依赖数据库。`<exp>.<hmac>` 让 ingest 侧只需共享 secret；会话状态检查留在
  平台侧，避免 ingest 被停播后的旧凭据复用。
- **签名与包体分离**：包体按内容寻址（sha256），签名旁挂 `(key_id, sig)`，
  密钥轮换不影响已发布包；注册表单独负责防降级，这是验签挡不住的一层。
