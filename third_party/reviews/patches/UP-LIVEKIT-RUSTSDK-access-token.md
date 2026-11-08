# UP-LIVEKIT-RUSTSDK · `livekit-token/src/access_token.rs` → `crates/rtc/src/token.rs`

- 上游：`livekit/rust-sdks` @ `052f089166b6864bc5f7e92906f6a7897b9f546c`
  永久链接：<https://github.com/livekit/rust-sdks/blob/052f089166b6864bc5f7e92906f6a7897b9f546c/livekit-token/src/access_token.rs>
- 交叉核对源（服务端，另一条 upstream）：
  <https://github.com/livekit/livekit/blob/94edd5da75246bd3c04becf88e3fda563e3d6e1c/pkg/service/auth.go>（`UP-LIVEKIT-SERVER`）
- 许可：Apache-2.0（许可不阻挡本项目，但来源必须可复查）
- 本地 sha256：`00fdaf23b9f779f06803a53e80262ded80e084b94a161988f37cee89afd2da51`
  （按 Git LF 字节计算；根目录 `.gitattributes` 把该文件钉成 `text eol=lf`，
  避免 Windows `core.autocrlf=true` 下检出成 CRLF 后指纹漂移——`sfu.rs` 就踩过这个坑。
  该 sha 是 `036e5c3`（清 CI 三个红项）之后的值：本文件只做了 `LiveKitIssuer::issue`
  拆成 `check_issue_input` / `access_grant` / `access_claims` 三段、`b64url_encode` 改用
  `div_ceil`、base64 的位移与码表偏移提成命名常量（`B64_TOP_SHIFT`、`B64_DIGITS_OFFSET` 等）、
  `AccessRequest.muted` 的 doc 链接换成 `crate::token::LiveKitIssuer::verify_for` 全路径，
  以及测试里的 `to_string` 去重与一处 `assert!` 改 `const { assert!(..) }`；
  签发与校验的判定顺序和结论都没动，registry 的 `local_sha256` 已同步）
- 形态：**adapted**（逐段照抄行为，Rust 化改写 + 去依赖 + 换错误类型；不是 copy-paste 改 import）
- 需求：#153 / ADR-25（媒体面 LiveKit sidecar，信令/token/房间审计留 Rust）

## 拿了什么（逐段对应）

| 上游 | 行为 | 本地落点 |
|---|---|---|
| `access_token.rs:29` `DEFAULT_TTL = 3600*6` | 默认寿命常量 | `LIVEKIT_DEFAULT_TTL_SECS`（只登记，签发仍受我们的上限收紧，见"改了什么"4） |
| `access_token.rs:43-96` `VideoGrants` | 15 个 grant 字段、camelCase 线格式、`skip_serializing_if = "is_default"` | `VideoGrant`（字段名/顺序/注释语义照抄；`room` 从 `Option<String>` 改回上游的 `String`） |
| `access_token.rs:98-100` `is_default` | "缺省即不序列化"的辅助函数 | 同名同实现 `is_default` |
| `access_token.rs:102-118` 四个取值方法 | `canPublish`/`canSubscribe` 缺省=允许、`canPublishData` 缺省跟随 `canPublish`、`canUpdateOwnMetadata` 缺省=不允许 | 四个同名方法，逐条照抄；这是"粒度语义"的核心，原来的 `bool` 字段把"没声明"读成"禁止"，与 LiveKit 服务端结论相反 |
| `access_token.rs:120-135` `SIPGrants{admin,call}` | SIP 授权面 | `SipGrant`（wxlike 目前没有 SIP，保留是为了与 sidecar claims 结构可比对，并让 Go 那两条判定能原样落地） |
| `access_token.rs:267-303` `Claims` | `exp/iss/nbf/sub/name/kind/video/sip/sha256/metadata/attributes` 字段集合 | `AccessClaims`（补齐原来缺的 `name`/`kind`/`sip`/`metadata`/`attributes`；`sha256` 从 `Option<String>` 改回上游的 `String`） |
| `access_token.rs:452-471` `to_jwt` | ① apiKey/secret 为空即 `InvalidKeys`；② `roomJoin && (sub 空 \|\| room 空)` → `InvalidClaims`；③ 只接受 HS256 | `LiveKitIssuer::issue` 开头两条同名检查（`EmptyApiKey` / `InvalidJoinClaims`），算法固定在 `encode`/`verify` 两侧 |
| `access_token.rs:496-511` `TokenVerifier::verify` | `validate_exp` + `validate_nbf` + `set_issuer(&[apiKey])` 三条校验 | `LiveKitIssuer::verify`：`exp <= now` 过期、`nbf`/`iat` 带 60 秒偏移上限、`iss` 必须等于本地 apiKey |
| `pkg/service/auth.go:63-120` `APIKeyAuthMiddleware` | `Bearer ` 前缀优先，否则退回 query `access_token`；**不读 form body** | `bearer_token()`（前缀不符即拒，不回退；空 header 视同没带） |
| `pkg/service/auth.go:45-49` 四个错误 | `permissions denied` / `invalid authorization token` / `invalid API key` / `ErrMissingAuthorization` | `TokenError::{PermissionDenied,NoGrants,MissingAuthorization,…}`，文案照抄 |
| `pkg/service/auth.go:172-259` 九个 `Ensure*Permission` | join / admin(带房间绑定) / create / list / record / ingressAdmin / sipAdmin / sipCall / destRoom(源+目标双绑定) | 九个同名 `ensure_*` 函数，判定条件与比较顺序逐条照抄 |

另外照抄了上游的两条测试思路：
`access_token.rs:685-712`（`test_defaults_are_not_serialized`，空 grant 序列化成 `{}`、
`Claims` 往返字节相等）与 `access_token.rs:663-666`（一枚裸令牌的 payload 键集合恰好是
`["exp","iss","nbf"]`）→ 本地 `video_grant_defaults_match_upstream_three_state_semantics`
和 `claims_omit_defaults_on_the_wire`。

## 改了什么

1. **去掉 `jsonwebtoken` 依赖**：HS256 由仓库已有的 `hmac`+`sha2` 直接算，base64url 手写，
   签名比较走常数时间 `constant_time_eq`。因此**没有**引入上游的
   `EncodingKey/DecodingKey/Header/Validation` 这层，`verify` 的三条校验改成显式代码；
   新增 crate 需要走 integration-request，本卡不动根 `Cargo.toml`/`Cargo.lock`。
2. **错误类型换成 `thiserror` 的 `TokenError`**（原来散在 `AccessTokenError` +
   `jsonwebtoken::errors::Error`）：所有变体 `PartialEq + Eq`，测试能逐条断言结论而不是
   `is_err()`。上游的 `InvalidEnv(#[from] env::VarError)` 没有移植——本仓库的密钥由调用方
   注入（`ApiKeyPair`），不在库里读环境变量。
3. **时间来源**：上游用 `SystemTime::now().duration_since(UNIX_EPOCH).unwrap()`。
   非测试禁 `unwrap`，且状态机要能喂确定性的时钟，所以 `issue/verify` 都收 `now_ms` 参数，
   内部一律 `saturating_*`。这是刻意的偏离。
4. **寿命策略收紧**：上游默认 6 小时；我们 `DEFAULT_ACCESS_TTL_MS = 60s`、
   `MAX_ACCESS_TTL_MS = 15min`，超出即 `InvalidLifetime` 拒签。理由写在 ADR-25：过期后必须
   回信令面重新过一次成员状态机与房间审计。`LIVEKIT_DEFAULT_TTL_SECS` 保留为对照常量。
5. **`Option<bool>` 的三态照上游，但签发时一律写显式 `Some(..)`**：我们的令牌必须自证权限面，
   不允许靠读侧缺省补齐；`server_side_mute` 的下发也因此写成 `canPublish:false` 而不是留空
   （留空在上游语义里是"允许"，那是个真漏洞）。这条由
   `livekit_grants_are_scope_derived_and_never_privileged` 与
   `server_side_mute_downgrades_the_grant_without_escalating` 钉住。
6. **`canPublishData` 的缺省语义与我们的策略冲突，显式覆盖**：上游会让它跟随
   `canPublish`（即通话令牌默认开数据通道），而 wxlike 的信令只走自己的 QUIC 面，
   所以 `grant_for` 显式 `Some(false)`，`verify_for` 用**取值方法**（不是字段）判升级。
   这是 ADR-25 下比上游更严的一处，不是移植失真。
7. **`VideoGrant`/`AccessClaims` 加了 `Default` 派生**并让 `AccessRequest` 也 `Default`，
   以便测试用 `..Default::default()` 构造，不靠手搓全字段。
8. **多签一枚 `wxlike` 私有 claim + `jti`**：见"没拿什么"最后一条。

## 顺手修掉的两个真缺陷（都是 WIP 提交留下的、不是上游带来的）

- **JWT 头部没有 base64url 编码**：`encode` 把 `{"alg":"HS256","typ":"JWT"}` 原文拼进
  token，产出的字符串看着是三段但不是合法 JWT，LiveKit 侧解不开，**我们自己的 `verify`
  也解不开自己签的令牌**（`{` 不在 base64url 字母表里，直接 `Malformed`）。现在头部走
  `b64url_encode`，由 `issued_token_is_a_real_three_part_jwt` 钉住（三段、字母表、无 padding、
  头解回 `alg=HS256`、签名 32 字节、自签自验通过）。
- **`b64url_decode` 的尾巴位移错**：不满 4 字符的一组按 `>>12 / >>6 / >>18` 取字节，正确是
  `>>16 / >>8 / >>0`。结果是"凡长度不是 3 的倍数的 payload 都解错"——包括 32 字节签名
  （43 字符，尾组 3 字符）与大多数 claims JSON。现在补了 `"_w" ↔ [0xFF]`、
  `"________" ↔ [0xFF;6]`、`"Zm9vYg"` 这类边界向量。
  这两个缺陷能活到评审，是因为 `fb77f31` 那笔 WIP 根本编译不过（`call.rs` 的 `Call`
  少了 `room` 字段），测试从来没跑过。本卡一并把 `room: None` 补上。

## 没拿什么（以及为什么）

1. **`Claims::from_unverified` / `jsonwebtoken::dangerous::insecure_decode`**
   （`access_token.rs:305-311`、`358-361`、`623-630` 的测试路径）：不落地。
   它把"未验签的 payload"变成一个能用的 `Claims`，我们的调用面里没有需要它的入口
   （信令面自己验签），多一个 dangerous API 只会给后来人留后门。刻意不借。
2. **`AccessToken` 的链式 builder（`with_*` 12 个方法）与 `AccessToken::new()` 读环境变量**：
   我们的 `LiveKitIssuer::issue(&AccessRequest, now_ms)` 是"房间已经裁决完再签"的单点入口，
   能力面**只能**由 `grant_for(scope)` 推导，不接受调用点手搓 grant —— 保留 builder 等于
   把 ADR-25 的签发闸拆掉。`jwt_provider::ensure_installed()`（进程级全局单例安装）也照
   上游自己的 AGENTS.md（"avoid singleton"）没有跟。
3. **`AgentGrants` / `AgentEndpointGrants` / `AgentThreadGrants` / `InferenceGrants` /
   `ObservabilityGrants`**（`access_token.rs:137-265`）：LiveKit Cloud Agents / Inference /
   可观测面，wxlike 不使用这些产品线；`agent` 布尔位**借了**（它在 `VideoGrants` 里，且
   `IsAgent()`/`IsDependent()` 判定要用），其余五组不借。
4. **`room_config: Option<livekit_protocol::RoomConfiguration>`**（`access_token.rs:302`）：
   字段真的存在，但类型是 `livekit-protocol` 的 protobuf 消息，引入它要新增
   `livekit-protocol`/`prost` 依赖——**新增依赖必须走 integration-request**，本卡不碰根
   `Cargo.toml`/`Cargo.lock`。已在 `integration-requests/G09/G09-06.yaml` 写清要加哪个
   crate、为什么、谁裁决。**这个字段现在是"有源但未落地"，不是自研，也不是遗漏。**
5. **`video.canPublishSources` 的判定逻辑**：字段照抄（`access_token.rs:75`），但**不提供**
   推断式访问器。上游注释说"一旦设定即取代 canPublish"，真正执行这条的是服务端里的
   `github.com/livekit/protocol/auth`（Go 模块，本 checkout 未含：`livekit_livekit` 的
   `pkg/` 全文 grep 不到 `CanPublishSources`）。没有源可抄的判定不写。
   同时 `assert_sidecar_grant_allowed` 明确拒签带 source 白名单的 grant——我们签不出
   自己无法复核权限面的令牌。
6. **`sha256` 的报文完整性校验**（`livekit-api/src/webhooks.go` 那一套）：上游用它给
   HTTP webhook 请求体做完整性签名。我们的信令面是 QUIC 二进制信封、自带 AEAD，
   这一位只**保留字段**（能读 LiveKit 签的令牌），不复算。
7. **`kind`/`jti`/`iat`/`wxlike` 中，`jti`、`iat`、`wxlike` 三枚是**自研**，LiveKit 的
   `Claims` 里没有这三个字段**（已逐行核对 `access_token.rs:267-303`）。它们存在的理由：
   LiveKit 的粒度只到"哪间房 + 能不能发"，而我们要绑定 call / device / scope / 成员代次 /
   引擎（冷备 str0m 签的令牌不得混进 LiveKit 房间）。`kind` 是上游真字段，照抄；
   我们额外用它补了 Go 的 `IsRecorder/IsAgent/IsDependent` 三条判据
   （`pkg/rtc/participant.go:605-622`）——因为只封 `video.recorder` 会留下 `kind="egress"`
   这条后门，而 ADR-25 要求服务端 Egress 自动录制必须关。

## 复核方式

`PYTHONPATH=tools/source-audit python -m sourceaudit provenance check` 核对 sha256；
文件头的来源声明段（"本节按 LiveKit 官方 livekit-token crate 移植/改写"）不得删除。
每条 `ensure_*` 上方都注了 Go 的行号，改上游即改测试。
