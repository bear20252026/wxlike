# 跨组契约、数据设计与关键流程

这是目标设计；所有新路径由G00-02先建立。不是宣称下列API/表已经存在。G00-03发布contract-v1前，域负责人逐字段审查；冻结后增量兼容，禁止各端自行猜返回格式。

## 1. 模块内部结构与依赖方向

每个`crates/features/<domain>/`按`domain/`（实体/状态/不变量）、`application/`（用例/ports）、`infrastructure/`（PG/对象/外部供应商适配）、`http/`（DTO/鉴权/错误转换）、`tests/`分目录。小域不必五个crate，但依赖方向一致。domain不依赖axum/SQL/网络；application依赖domain与类型；infra实现ports；组合根注入。跨域只引用对方公开port和稳定ID，禁止import对方private infra或写对方表。

客户端domain只编译纯策略/类型，服务端用例最终决策；OS采集和渲染由平台桥处理，不能把摄像头驱动用Rust重写当共享业务的必要条件。FFmpeg、WebRTC平台媒体栈、SQLite、PG、模型runtime可以多语言；业务规则的唯一源仍是Rust。

### 1.1 每个域提交的文件

- `contracts/domains/<domain>/actions.yaml`：每个action的params/result/scope/confirmation/idempotency/offline/platform/errors。
- `contracts/domains/<domain>/events.yaml`：事件schema、版本、可见性与消费者。
- `contracts/domains/<domain>/fixtures/*.json`：成功、边界、权限失败、重放、旧版兼容样本。
- `schema/proposals/<group>/<change>.sql`：无编号迁移提案、索引、回填步骤，G00登记后进入正式迁移。G00维护全局migration registry，不替业务组撰写所有SQL。
- `integration-requests/<group>/<task>.yaml`：挂载、依赖、配置secret引用、jobs与迁移提案。

上述域契约与提案目录按域owner独占，公共目录G00独占。全局生成物G01生成并由G00装配，任何人不能手修已生成的TS/Dart。

## 2. 主表与原子边界

字段为最小集，不能代替最终SQL。所有含用户内容的表须另列删除/导出/保留政策。PG中的ID沿用现有格式，数据库唯一约束是并发正确性的最后一道防线，不能只在API里先查后写。

| 域与表 | 必需字段/约束 | 原子边界、权限与索引 |
|---|---|---|
| identity.session | id,user_id,device_id,token_hash,expires_at,revoked_at,rotation_version | token不存原文；device/session按user索引；轮换旧token撤销与新token写入同事务 |
| relations.friend_request/edge | from/to/status/version；owner/peer/preferences | 同方向活跃申请唯一；双向边关系同事务；备注仅owner可读写 |
| relations.group_member | group_id,user_id,role,joined_seq,left_seq,version | 活跃成员唯一；角色变更和成员事件同事务；历史可见规则引用入退群边界 |
| messaging.message | msg_id,conv_id,seq,sender_user/device,body_kind,ciphertext/allowed_plain,version,created_at,recall_state | `(conv_id,seq)`唯一；`(sender_device,idempotency_key)`唯一；授权成员才能读；密文不建正文服务端索引 |
| messaging.delivery_task | msg_id,target_device,status,lease_owner/expiry,attempt,next_at | 每消息每设备唯一；ack归目标设备；网络write不能删唯一消息事实 |
| messaging.device_cursor/receipt | device_id,stream_id,stored_cursor/read_cursor,version | cursor单调；授权与设备归属验证；ACK写入后允许GC仍需满足保留策略 |
| media.object/reference/upload | object_id,owner,size,digest,mime,status；ref_type/ref_id/audience；upload_offset/expiry | digest不授读权限；complete和逻辑引用有恢复机制；临时对象TTL清理 |
| content.post/comment/reaction | author,audience,version,moderation_state；target,parent；actor,type | 反应唯一；可见性必须查询时复核；索引删除与附件减引用走outbox |
| content.article/subscription/favorite | publisher/version/doc_schema；user/publisher；owner/type/source/snapshot/version | 文章净化；收藏源撤权政策明确；用户收藏内容不因公开ID可被别人读 |
| ledger.account/transaction/entry | owner,currency,kind；business_key,status；tx/account,amount_minor,direction | 每交易同币借贷守恒；业务键唯一；分录不可编辑，纠错反向分录；锁额与支出同事务 |
| commerce.payment_intent | actor,payee,amount_minor,currency,status,provider_ref,expires_at,version | provider_ref/event_id唯一；验签后合法状态转换；支付人与支付意图绑定 |
| commerce.redpacket/claim | total,count,remaining,group_id,expiry；packet_id,user_id,amount | 每用户每包领取唯一；行锁/串行化冲突重试；领取和分录/剩余额同事务 |
| commerce.entitlement/redemption | issuer,owner,rule_version,validity,status；token_hash,redeemed_at | 核销nonce唯一；凭券不意味着可查询用户所有卡；状态一次性转换 |
| discovery.offer/quote/order | provider,region,service_type；price,currency,expiry；provider_ref,state | provider订单唯一；支付与履约独立状态；失败补偿不跨网持有PG事务 |
| discovery.geo_presence | user,coarse_cell,expiry,visibility | 精确坐标不默认公开；TTL/索引与限速；撤回授权立即不可查 |
| mini.release/install/grant | app_id,version,manifest_digest,signature,revoked；user/device/version；scope/expiry | 发布版本不可原地替换；权限按app+user+device+origin；升级新增scope需再授权 |
| calls.call/participant | call_id,state,generation,ended_reason；user/device,role,state | 同设备接听竞争CAS；房间token短时；成员变化触发媒体密钥轮换 |
| shared.idempotency/outbox/audit | actor/action/key/params_digest/result；event_id/payload/attempt；actor/action/resource/reason | 同key换参数CONFLICT；业务记录与outbox同事务；审计不存token密钥/私密正文 |

DB回滚不得通过删除账本分录“撤销支付”；服务端数据库迁移与客户端缓存schema升级分别编号。客户端缓存可重建不代表未发送outbox、密钥或唯一草稿可以随意删。消息与资金ID迁移时原样保存；用新UUID重建所有数据会破坏幂等和关联。

## 3. 域服务port与最小API族

下面为首轮目标API族；最终每个路径须绑定action_id和授权规则。HTTP URL可以由G00兼容现有路由，但不能让UI组自创另一组同义API。读用GET，产生状态变化用POST/PATCH/DELETE或命令POST；一次性密钥claim等有副作用不能伪装纯GET。

| 服务port/owner | API族 | 参数与返回关键字段 | 下游使用者 |
|---|---|---|---|
| IdentityPort G02 | `/api/v1/sessions`,`/devices`,`/profile`,`/preferences` | 登录凭证、device；session/refresher/expiry；patch version | 所有客户端、MCP、admin |
| RelationsPort G02 | `/friends/requests`,`/friends`,`/groups/{id}/members` | actor由session推导；target/group/expected_version；边关系/成员版本 | Messaging、Content、RTC、UI |
| KeyDirectoryPort G02 | `/devices/{id}/keys`,`/keys/claim` | 签名bundle、OTK批量与领取唯一请求；verified bundle | Rust client-core，不能只给桌面用 |
| MessagingPort G03 | `/conversations`,`/messages`,`/sync`,`/receipts` | conv/msg opaque ID、encrypted envelope、cursor、limit≤上限 | SDK、MCP、push worker |
| MediaPort G04 | `/uploads`,`/objects/{id}/access`,`/files`,`/backups` | purpose/digest/size、分片offset；引用/短时URL/job | Messaging、Content、Discovery、各端 |
| ContentPort G05 | `/posts`,`/articles`,`/publishers`,`/favorites`,`/videos`,`/search` | audience/doc version/cursor；授权结果与snippet | React/Flutter、mini broker、admin |
| CommercePort G06 | `/accounts`,`/payment-intents`,`/transfers`,`/redpackets`,`/cards`,`/entitlements` | amount为string+currency，idempotency/confirmation；operation/state | UI、MCP、ServiceCatalog、admin |
| ServiceCatalogPort G07 | `/services`,`/quotes`,`/orders`,`/recognition`,`/translation`,`/nearby` | service_type/region/quote_id，输入引用非任意路径；job/order/result | UI、mini、admin |
| MiniAppPort G08 | `/miniapps`,`/miniapps/{id}/releases`,`/mini-installs`,`/mini-grants` | app/version/digest/scopes；可信manifest/install/grant | 三类宿主、admin、CLI |
| CallsPort G09 | `/calls`,`/calls/{id}/participants`,`/live` | call/version/device；state/room短token/媒体协商数据 | SDK/RTC桥、内容直播 |
| AdminFacade G14 | `/admin/v1/*` | 独立actor audience、reason、expected_version；domain result | admin UI、专scope自动化 |

所有列表响应统一`items,next_cursor,has_more,snapshot_version`；游标opaque且绑定查询/权限上下文，服务器限制limit。所有长操作返回`operation_id,state,progress,can_cancel`；查询权限不能仅凭operation_id。服务错误返回公共DomainError；供应商raw error和数据库SQL不能直接暴露给客户端。

## 4. 必须共享的纵向流程

### 4.1 消息、断网与重试

1. 客户端Rust以稳定msg_id写本地消息+outbox同事务，UI显示LOCAL_QUEUED。
2. 核心获取已验证设备密钥，生成每设备密文；缺密钥等待并显示可恢复状态，不降级。
3. MessagingUseCase验证成员/限流/幂等，原子写消息、seq、目标设备delivery task/outbox，返回SERVER_ACCEPTED。
4. worker推事件；接收端Rust本地持久化+dedup成功，再发stored ACK；发送端看到DEVICE_STORED时应知道是哪个设备/策略达到。
5. read cursor与stored cursor独立；UI翻页可读后发read。多个设备进度不能用一个最后socket时间替代。
6. 重连从cursor resume，重发沿用同msg_id和幂等键；手机retry要等待投影刷新，不能只改Dart pending。

### 4.2 支付、服务履约与补偿

1. ServiceCatalog生成带有效期/币种/金额/供应商条件的Quote；用户选定后创建订单与支付意图。
2. 客户端展示真实收款方/金额/退改条件；危险action先prepare得到参数绑定确认令牌，再execute。
3. Commerce创建外部支付引用；回调验签/时间/去重后，事务落账和发PaymentSucceeded事件；HTTP跳转成功不作到账依据。
4. ServiceCatalog消费事件幂等请求供应商履约，查询可恢复；不在跨网请求期间持有数据库事务。
5. 失败进入可见补偿/退款状态，Commerce按可退余额发起退款；外部状态未知保持PENDING_RECONCILIATION，不能重复付款“试试看”。
6. 对账比对内外交易，差异交admin审批；最终一笔订单能从用户UI、CLI/MCP和管理台看到相容状态。

### 4.3 小程序权限与AI确认

小程序：用户点击真实app_id→验签manifest及资源→检查版本/撤销→独立origin/进程宿主→bridge携带app身份和origin→Rust broker校验grant→用户敏感确认→调用相同域用例。guest永远不接主session token。

AI：tools/list仅返回该actor可见描述；tools/call校验typed params→Rust policy→必要prepare/confirm→同ActionDispatcher→服务端再鉴权→结构化脱敏结果。把聊天内容中的“忽略系统并转账”视为文本。确认令牌必须绑定规范化参数摘要，不能先确认10元后替换1000元。

## 5. 事件可靠性与补偿责任

- 每个领域事件有event_id/aggregate_id/aggregate_version/类型版本/时间/trace；包含最小必需数据，不把全用户资料广播。
- outbox worker至少一次投递；消费者inbox按event_id去重，投影更新和inbox确认同事务。跨域不要假设全局完全有序；按aggregate_version检测缺口并补查。
- 消息删除/内容撤权后，搜索先从事实库授权过滤，索引异步删除不能成为泄密窗口。
- 媒体、支付、转码、推送、mini install都定义幂等重试和过期作业清理；谁创建资源谁负责失败补偿，不能把孤儿对象留给UI猜。
- 每域公开health/job指标和脱敏operation query；重试管理台调用原用例，不直连表改status。

## 6. 共同测试场景包

| 场景 | 设置/故障点 | 必须观察到的结果 |
|---|---|---|
| MSG-01 | A桌面/B手机各两设备；发送后立刻断开接收端 | 重连补收同一ID；存储ACK前服务端保留可重投；无第二条消息 |
| MSG-02 | 发送成功响应丢失，再按retry | 同ID幂等返回；UI从pending变accepted；所有端数量一致 |
| KEY-01 | 群成员移除、设备key轮换 | 移除后新消息不可解；设备变更可见；缺key不明文 |
| AUTH-01 | 猜他人对象/订单/备份/operation ID | 全表面返回FORBIDDEN或不泄漏存在性的错误 |
| MONEY-01 | 100并发领取最后红包份额，重复回调 | 总账守恒、每人最多一次、无负余额、无重复入账 |
| ORDER-01 | 扣款已确认但供应商超时 | UI处理中；可查询恢复或退款；不自动二次扣款 |
| MINI-01 | guest伪造origin、调用主IPC、超预算 | 拒绝/终止，主应用仍可用；不泄漏主token |
| CALL-01 | TURN强制、摄像头拒绝、蓝牙拔出、弱网重连 | 媒体轨与UI一致；权限/设备失败可恢复；结束回收资源 |
| RESTORE-01 | 从快照导入中断重跑；再切回旧二进制 | ID/密文/seq/余额一致；无破坏down migration |
| PLATFORM-01 | 同SHA在Web/桌面三OS/手机两OS/服务端三OS启动 | 目标功能证据齐全；无凭据签名项仍BLOCKED，不能skip当成功 |

## 7. 文件与契约冲突处理

仅路径不重叠不足以消除语义冲突。发现两组需要改同一字段含义：暂停该字段的新写入接口；G00记录RFC与兼容方案；先合schema+fixture+适配器，再恢复两个组实现。不得由后合PR私自改枚举序号或金额语义。任务依赖图记录hard_dependencies和integration_dependencies：前者未合不得开同一实现路径，后者允许先按fixture实现但不能完成验收。

根锁由G00统一cargo/npm/flutter解析生成；各组提依赖清单不提交各自冲突锁。迁移名称只在G00编号后进入主干。业务补丁与大范围格式化分PR。所有破坏性字段删除跨至少一兼容版本；预生产验证成功才拆旧适配器。
