# 视频面：播放面与控制面（issue #7 / #8）

视频服务原本把所有路由一股脑合并进公开 HTTP app：上传、建作业、重试、播放
全在同一个面上，**没有任何认证**。审计（A05/A06）指出两个后果：

1. 匿名请求可以占满上传带宽、内存、磁盘与转码 CPU；知道 `job_id` 就能查/重试
   别人的作业；
2. 客户端给的 `callback_url` 只校验了 `http(s)://` 前缀，而 worker 用**服务端
   网络**去 POST——这是标准的 SSRF 入口。

这份文档说明现在的边界在哪、怎么配、为什么这么切。

## 一、两个面

| 面 | 路由 | 认证 |
|---|---|---|
| 播放面 | `GET /api/video/{id}`、`/playlist.m3u8`、`/seg/{name}`、`/play.mp4`、`/thumb.png`、`/select` | **匿名** |
| 控制面 | `POST /api/video`（同步上传）、`POST /api/video/jobs`、`GET /api/video/jobs/{id}`、`POST .../retry` | `Authorization: Bearer <token>` |

播放面保持匿名是**故意的**，不是漏了：`<video src>` 与 HLS 播放器不会给分片
请求带 `Authorization`（与 #14 的媒体面同一个约束）。真要给播放加鉴权，正确解
是短期签名 URL，那是另一条任务。

控制面的闸门是 **axum middleware**（`wxlike_video::policy::guard`），在**解析
请求体之前**就认出调用者。只在 handler 里检查是不够的：`Multipart` extractor
先跑，匿名的 `POST /api/video/jobs` 会先回 400，而且一个合法的 500 MiB
multipart 会被完整读进内存。

认证器由装配层注入（`VideoPolicy::auth`），`None` 的语义是**拒绝一切**，与 hub
的 `MqHubOptions.auth` 同一约定——忘了装是"控制面不可用"，不是"谁都能用"。
`server` 用平台会话表实现它（`SessionVideoAuth` → `Platform::lookup_token`），
和 hub 用同一份凭据，视频面不该有第二套"谁能上传"的答案。

## 二、归属

| 表 | 用途 |
|---|---|
| `video_jobs.owner` | 作业提交者。查/重试只对本人开放 |
| `video_owners(video_id, owner, size)` | 同步上传的归属（多对多：内容寻址去重后同一份字节可能被多人上传） |

别人的作业一律 **404**，不是 403：403 等于确认"这个 id 存在"。
重试是写操作，只认 `video_jobs.owner`；查作业额外允许"同一份内容的另一次上传者"。

老库迁移：`video_jobs.owner` 用 `pragma_table_info` 判断后 `ALTER TABLE` 补列
（SQLite 没有 `ADD COLUMN IF NOT EXISTS`），补不上就报错，不静默跳过。

## 三、配额

| 项 | 默认 | 说明 |
|---|---|---|
| 单用户排队作业数 | 4 | `state IN ('pending','running')` 计数 |
| 单用户排队字节 | 2 GiB | 同上，按 `source_size` 求和 |
| 全站排队字节 | 32 GiB | 磁盘预算，超出回 429 |
| 单用户累计上传 | 2 GiB | 同步上传按 `video_owners.size` 求和 |

配额在**提交时**判掉，不等 worker 转完再说。转码 worker 是**串行**的
（`run_worker_round` 一轮逐个领），所以"并发"由配额而非信号量控制——4 个排队
作业已经足够压满一台小机器。

拒绝的形状是 `Rejected { kind, message }`：`Quota` → 429，`BadCallback` → 400。
混成一个 `anyhow` 字符串的话，HTTP 层只能靠 grep 消息文本猜状态码。

## 四、回调目标（SSRF）

默认策略：**不接受客户端给的 `callback_url`**。服务端自己配的
`WXLIKE_VIDEO_CALLBACK_URL` 不受白名单约束（那是运维写的，不是用户输入）。

| 环境变量 | 作用 |
|---|---|
| `WXLIKE_VIDEO_CALLBACK_ALLOW=host1,host2` | 允许客户端把回调发到这些主机（小写、不含端口） |
| `WXLIKE_VIDEO_CALLBACK_ALLOW_PRIVATE=1` | 额外允许私网/回环（**只用于本地联调**） |

校验分两处，因为只查一次是可以绕过的：

1. **提交时**（`CallbackPolicy::check`）：协议必须是 http/https、不能带
   `user@`、主机必须在白名单里、主机若是 IP 字面量则必须是公网地址；
2. **发送前**（`CallbackPolicy::resolve`）：重新解析 DNS，**任何一个**解析结果
   落在私网就整体拒绝——混合解析是 DNS 重绑定的常见形态。

拒绝的地址族：回环（127/8、::1）、私网（10/8、172.16/12、192.168/16、fc00::/7）、
链路本地（169.254/16、fe80::/10）、CGNAT（100.64/10）、组播、保留段（240/4）、
`0.0.0.0`、以及 v4-mapped IPv6（`::ffff:a.b.c.d` 按内嵌 v4 判——这是绕过 v4
检查的老手法）。

重定向：`deliver_callback` 用 ureq 的默认 agent，POST 的 3xx 一律判**永久失败**
（不跟随）。跟随重定向等于把"目标白名单"交给对端决定。

回调**带共享令牌**：`x-wxlike-video-token`，值来自 `WXLIKE_VIDEO_CALLBACK_TOKEN`
（与平台侧 `/api/video/callback` 是同一个变量）。平台侧的口径是"未配置令牌 =
接口关闭"，所以视频侧不带这个头的话，配了令牌的部署里回调会一直 401——两边
必须成对配置。

## 五、验收用例

`crates/video/tests/video_access.rs`：

- `control_plane_without_authenticator_is_closed` —— 没认证器 = 401
- `anonymous_cannot_touch_jobs` —— 匿名查/重试/建作业/上传全部 401
- `a_cannot_operate_on_bs_job` —— 别人的作业 404，本人 200
- `queue_quota_rejects_before_the_worker_runs` —— 作业数配额，且按人隔离
- `byte_budget_rejects_oversized_queue` —— 字节预算
- `client_callback_url_is_refused_by_default` —— 默认不接受用户地址
- `callback_allowlist_and_private_address_rules` —— 白名单 / 白名单外 / 回环 / 显式放开

`crates/video/src/policy.rs` 的单元测试覆盖 URL 解析与 IP 分类（含 `::ffff:127.0.0.1`）。
