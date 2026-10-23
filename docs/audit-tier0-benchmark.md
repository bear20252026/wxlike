# 第四轮审计 × Tier 0 参考项目对拍（问题域基准核查）

> 2026-10-22。审计问题域（AR-1…AR-7 + 技术债 M 组）逐条对照合同 7.1 / `reference-mirror.md`
> 的 **Tier 0 八个最高优先级参考**：Zulip、Rocket.Chat、Mattermost、Signal-Desktop/Android、
> Element Web、Synapse、Nextcloud Talk。未镜像的六个经 GitHub 取证（gh api / raw），
> 每条给可核对的出处；本地镜像（element-web、spreed）直接取源码/文档。
>
> **勘误声明**：本文取代 2026-10-22 早些时候基于 `refs/` 微信生态语料的口头对拍——
> 那份用错了语料层级，应以本文为准。

## 1. 对拍总表

| 问题域 | Tier 0 参照与证据 | wxlike 状态 | 对拍结论 |
|---|---|---|---|
| AR-1 预密钥签名 | **Signal 协议**：X3DH 的 signed prekey 是协议标准件（预密钥必须由身份密钥签名）；**Matrix/Synapse**：设备密钥 Ed25519 自签名随设备上传，服务端只存储、验签在客户端与对端 | `e40a9ec`：双层 Ed25519 签名（identity 绑定 + 逐把 OTK），`encrypt_dm` fail-closed 验签 | ✅ 与两大协议同构；等于回归 Signal/Matrix 的既定做法，非自创 |
| AR-2 入口资源闸 | **Synapse** 联邦限流：并发上限 + 队列深度 `reject_limit` 双闸，超限 `LimitExceededError`→429 | `f53891d`（faucet 鉴权/额度、WS 帧上限、Redis 凭据脱敏） | ✅ 同一"入口必须有闸"哲学；Synapse 的闸比我们的更细（见 §3 升级项） |
| AR-3 凭证存储 | **Mattermost** `server/public/model/session.go`：`Session.Token` **明文存储**，PAT 有效期 `100*365*24` 小时（100 年）；**Synapse** `storage/databases/main/registration.py`：`get_user_by_access_token` 用 `WHERE token = ?` **明文等值查询** | `bd2fcc1`：token 列存 sha256，六处读写全部摘要比对 | ✅✅ 修复后严格于全部 Tier 0 参照（Mattermost/Synapse 均明文）；"库文件泄漏≠凭证泄漏"这一性质是 wxlike 现在独有的 |
| AR-4 不可信代码执行 | **Rocket.Chat apps-engine**：插件框架走 bridges 桥接 API，README 明言"由 host 实现者负责限制 server setting/环境变量访问"——README 层面**未承诺**资源限额；业界 vm2 已停止维护 | `cf8bd82`：Boa 循环限额 + dpage 64MiB 解压封顶 + 注入面消除 | ◐ 互补而非对齐：apps-engine 是进程外插件沙箱取向，wxlike 是进程内容器自设边界。**教训可抄**：隔离方案要选有维护的（vm2 之鉴），Boa 进程内反而无此风险，但需补内存配额路线（#28 遗留） |
| AR-5 限流放置 | **Zulip** `zerver/lib/rate_limiter.py`：GCRA（Redis + Lua），`RateLimitedObject` 按 domain **声明式**声明规则，键分 user/IP/endpoint 三类，支持 per-user 自定义规则、多规则取最严、可注入时钟；**Synapse**：context-manager 包裹请求处理，限流器带 `metrics_name` 进 Prometheus | `c532810`：`risk_mw::gate` 声明式 (method,path)→Action 映射，统一 429 | ✅ 同型（声明式+统一落 429）；Zulip/Synapse 成熟度高一级，升级路径见 §3 |
| AR-6 消息 TTL | **Signal** disappearing messages：协议层 `expiresInSeconds`，服务端不持久化历史（闭源），过期即无从投递 | `2fe4a41`：hub 补发路径丢弃过期秘密帧，回执/撤回豁免 | ✅ 同一目标不同位置：Signal 靠"服务端不存"，wxlike 服务端持有离线队列，故必须在**补发点**执行——是该架构下的正确对应物 |
| AR-7 TURN 凭据 | **Nextcloud Talk** `spreed/docs/TURN.md:37,66-69`：部署文档即 `static-auth-secret` 模式 | `c541b82`：coturn REST 规范端点（HMAC-SHA1+base64，按用户 1h） | ✅ 与参考部署实践完全一致 |
| M-11 hub 生命周期 | Tier 0 无同构参照：Mattermost 单体+HA、Synapse worker 共享 DB，都不是"有损连接网关"架构（最接近的是 goim 类 comet/logic，非 Tier 0） | `87dd471`+`40ace05`：leak/HubLease 注销 + 既有租约回收 | ✅ 无直接抄写对象；wxlike 的路由租约+显式注销自洽，多节点规模化时可再对照 WuKongIM cluster（Tier 1） |

## 2. 三个硬结论

1. **修复方向全部通过 Tier 0 佐证**——七条问题域的解法要么与 Tier 0 一致（限流声明式、
   TURN secret、TTL 语义），要么与 Signal/Matrix 协议标准件同构（预密钥签名）。
2. **凭证存储一项，wxlike 修复后严格于全部 Tier 0**：Mattermost 明文 token + 100 年 PAT、
   Synapse 明文等值查询——两处均为 2026-10-22 从各自 master/develop 分支实测取证。
   "参考成熟项目就不会有这个问题"在此不成立；反过来，这也是给 #32 M-2（设备密钥同盘）
   的参照：Signal-Desktop 的 sqlcipher 全库加密是现成模板。
3. **Tier 0 的真实优势在限流/协议层成熟度，而非安全存储**——对齐它们最有价值的具体
   升级项见下节，均为增量可做，不动架构。

## 3. 值得继续抄的 Tier 0 升级项（排期建议，非缺陷）

| 来源 | 抄什么 | 对应 wxlike 位置 |
|---|---|---|
| Zulip | GCRA 的 Redis+Lua 跨节点状态：risk 引擎目前进程内，多节点部署时限流会各算各的——Zulip 的 Lua 脚本与 `RateLimitedObject` 声明式规则是多节点化的现成设计 | `crates/risk`（其"时间由调用方传入"与 Zulip `_testing_clock` 同款，互相印证） |
| Synapse | 限流三态里的 **sleep 降级**（超限先延迟后拒绝）+ 限流指标带 `rate_limiter_name` 标签进 Prometheus | `wxlike_net::SendGate`（已有 Throttle 语义，补 sleep 与指标标签） |
| Element Web | 设备验证/安全码 UI 全流程（本地镜像 `refs/element-web` 可直接对照） | #25 遗留的客户端半边 |
| Mattermost | `Session.Sanitize()`：token 在任何序列化出口前置空 | sessions 接口已做（永不返回 token），保持 |
| Rocket.Chat | apps-engine 的 Pre/Post 事件阶段模型（破坏性操作只允许在 Modify 阶段） | `crates/container` 页面生命周期（wxhost 桥的事件时序可对照收紧） |

## 4. 取证方式说明

- 未镜像六个（Zulip/Rocket.Chat/Mattermost/Signal×2/Synapse）：`gh api` + raw.githubusercontent
  + GitHub 页面取证，日期 2026-10-22，均指向当日 master/develop 分支；code search 对
  Signal-Android/Rocket.Chat 未命中可用路径，相应条目只做协议层陈述并标注。
- 本地镜像两个（element-web、spreed）：直接读源码/文档；spreed 引用为 `docs/TURN.md` 行号。
- Mattermost/Synapse 的"明文"结论基于单文件取证（session.go、registration.py 前 1000 行），
  hash 若存在于其他层（如代理/企业版）未排除——如需仲裁请以全量克隆复核。
