# tests/security — 安全回归套件（G17）

独立 workspace（`[workspace]` 空表），不打桩：全部打**真实的域实现**——平台路由、
风控引擎、页包解析、容器宿主。原因是这些用例要证明的正是"鉴权/限速/解码上限真的
在链路上生效"，mock 掉就等于把要测的那一层删了。

根 `Cargo.toml` 由 G00 独占，所以这里不登记成 workspace 成员；接进 CI 的申请写在
`integration-requests/G17/G17-03.yaml`。

## 跑法

```bash
cargo test --locked --manifest-path tests/security/Cargo.toml     # 全部 51 项，无 ignore
cargo test --manifest-path tests/security/Cargo.toml --test sweep # 只跑全端点未认证扫描
```

不需要数据库、不需要网络、不 sleep：时间通过引擎的 `now_ms` 入参注入。

## 文件

| 文件 | 内容 | 对应任务卡 |
|---|---|---|
| `tests/authorization.rs` | 越权回归 14 例：跨用户/退群/撤会话/管理面/私有对象/幂等/拉黑/凭证泄露 | G17-03 |
| `tests/abuse.rs` | 滥用限速 8 例：spam、好友枚举、验证码爆破、ICE 突发不误伤、桶隔离 | G17-03 |
| `tests/sweep.rs` | 全端点未认证扫描：矩阵里每条私有端点都必须 401/403 | G17-03 |
| `tests/fuzz.rs` | 解码面：截断/坏魔数/长度不符/解压炸弹/路径穿越/深度炸弹/语料回放 | G17-04 |
| `tests/web.rs` | 出站 URL（SSRF）、路径穿越、JSON 而非 HTML 的响应面 | G17-04 |
| `tests/sandbox.rs` | 小程序沙箱：死循环预算、能力清单缺失、`call_global` 不 eval、生命周期 | G17-04 |
| `tests/common/mod.rs` | 夹具：真 `Platform::router` + 真注册 + multipart 上传 | — |

## 三条约定

1. **拒绝的形状要窄。** 越权必须是 401/403/404，不能是 200（数据给出去了）、
   不能是 500（抛错而不是拒绝）、也不能是 422（参数校验先跑了——那说明鉴权顺序不对）。
2. **断言效果，不只断言状态码。** "返回非 2xx"可能是"返回成功但什么都没做"：
   撤销他人会话之后必须验证**受害者仍然可用**（AUTHZ-006）。
3. **不硬编码阈值。** 限速用例读 `RiskEngine::config()`，阈值调整时用例不用改，
   但边界仍然被盯住。

## 2026-10-25 主干集成

#229 / #253 / #230 的产品补丁已进入主干，三条 `#[ignore]` 已取消。
`direct_conv_mailbox_requires_participation` 覆盖 unread / read / sync 三个面；
`bridge_state_is_bounded` 检查最新 256 行、单行 4096 字节、Unicode 与 drain；
`overflowing_payload_len_does_not_panic` 及 s03 语料要求 Err。
变异 fuzz 对任何 panic 都失败，不再允许“已知 overflow”。

PR 默认 2000 次变异；每日任务 `WXLIKE_FUZZ_ITERS=200000`，失败输入写入
`WXLIKE_FUZZ_CRASH_DIR` 并上传 artifact。迭代数必须在 1000..=200000。
管理面 #245 的两项矩阵 exempt 仍保留为明确发布阻塞；不能把 51 项通过当作全产品安全验收。

云端接线及证据见 `security/release-reviews/REVIEW-2026-10-25.md`。
