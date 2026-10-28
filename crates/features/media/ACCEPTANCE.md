# G04 媒体域验收说明（#117 / #119 / #120 / #122 / #125）

## 本卡实现了什么

新 crate `wxlike-media`（`crates/features/media/`），SQLite 元数据 + 内容寻址对象存储 +
axum HTTP 层。内容地址（SHA-256）**不作为授权凭据**：每次读取都走 owner → 活跃引用受众
（会话/群成员由注入的 `AudiencePort` 复核）→ 可撤销分享令牌的顺序鉴权；短时签名 URL 绑定
per-object `access_epoch`，撤权立即失效。

| 卡 | 模块 | 关键实现 |
|---|---|---|
| G04-01 | `access.rs` `uploads.rs` `http.rs` | 分片校验/断点续传/幂等完成；对象引用绑定 owner/audience/purpose；逐请求读取鉴权；1GiB capability；流式下载（Range/206/416）；撤权即失效 |
| G04-02 | `files.rs` | 授权投影列表（分页/排序/分类/文件夹）；类型与长度限制；可撤销分享（令牌只存哈希）；回收站 + 引用计数 GC |
| G04-03 | `processing.rs` | 任务 `(object_id,preset)` 幂等；崩溃重试/停滞回收/取消；FFmpeg 子进程无 shell、空环境、`-protocol_whitelist file`、rlimit（CPU/内存/输出体积）、墙钟超时；魔数嗅探拒绝可执行输入；衍生对象随源对象 GC |
| G04-04 | `backup.rs` | WXBK v1 容器（manifest + 逐对象校验 + SHA-256 尾）；导入去重不改历史 ID、逐条续跑；密钥只带 `key_ref` 走 G02 escrow；缓存 plan 先分类 recoverable/protected，唯一/未发送附件一律保护 |
| G04-05 | `platform.rs` | 共享请求/结果/权限错误契约；web 静默截屏与任意写路径拒绝；录音指示器状态机；缓存预算由 Rust 策略切分 |

## 测试命令与结果（本机 2026-10-24）

```text
cargo check -p wxlike-media
cargo test -p wxlike-media
#  lib:   23 passed, 1 ignored(需要 ffmpeg)
#  g04:   17 passed      （分片续传/取消清理/伪造与跨群拒绝/签名过期撤权/
#                          分页投影/不泄名/引用保活/分享撤销/附件限额/
#                          任务幂等与预算/备份往返+篡改拒绝+续跑/缓存保护/平台fixture）
#  http_api: 3 passed    （HTTP 上传-授权-下载-Range-撤权 / 伪造与越权 / 文件中心+平台契约）
cargo clippy -p wxlike-media --all-targets -- -D warnings   # 通过
cargo fmt --check -p wxlike-media                            # 通过
python scripts/quality-gate.py                               # 通过（魔法数 159→154，其余持平）
typos                                                        # 通过
cargo deny check advisories bans sources                     # 通过（许可证不在闸内）
```

## 待验清单（本机物理上跑不了，不做假绿）

- [ ] 真机/CI 跑 1GiB 单文件流式上传-完成-下载（含断点续传中断恢复），确认内存峰值有界。
- [ ] 带 ffmpeg 的 CI 机器：`cargo test -p wxlike-media -- --ignored ffmpeg_thumbnail_end_to_end`
      以及畸形图/压缩炸弹/超长视频按预算退出的实机复核。
- [ ] 真实 `AudiencePort`（G02/G03 成员资格）注入后的端到端越权用例——当前为 fixture。
- [ ] 真实 `SessionVerifier`（G02 会话）替换 `http::StaticBearer` 后的 401/403 链路。
- [ ] 真实 `ChatSnapshotPort`/`KeyEscrowPort`（G05/G02）下的备份恢复全链路与跨端消息 seq 校验。
- [ ] S3/OpenDAL 对象存储适配器（`ObjectStore` trait 已冻结）接入后的读写/GC。
- [ ] 服务端组合根（G00-02）挂载 `MediaApp::router()` 后的全仓 `cargo test` 与 CI 绿灯。

## 需要拍板的集成项

根 Cargo.toml member、server 挂载、迁移编号、`WXLIKE_MEDIA_SECRET` 的登记见
`integration-requests/G04/G04-0{1..5}.yaml`，全部交 G00 串行执行。
