# tests/security — 安全回归套件（G17）

独立 workspace（`[workspace]` 空表），不打桩：全部打**真实的域实现**——平台路由、
风控引擎、页包解析、容器宿主。原因是这些用例要证明的正是"鉴权/限速/解码上限真的
在链路上生效"，mock 掉就等于把要测的那一层删了。

根 `Cargo.toml` 由 G00 独占，所以这里不登记成 workspace 成员；接进 CI 的申请写在
`integration-requests/G17/G17-03.yaml`。

## 跑法

```bash
cargo test --manifest-path tests/security/Cargo.toml              # 全部（不含 #[ignore]）
cargo test --manifest-path tests/security/Cargo.toml --test sweep # 只跑全端点未认证扫描
cargo test --manifest-path tests/security/Cargo.toml -- --ignored # 连已知缺口一起跑（会红）
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

## 已知红项（`#[ignore]`，带 issue）

| 用例 | 原因 | issue |
|---|---|---|
| `direct_conv_mailbox_requires_participation` | 会话信箱未校验参与者归属 | #230 |
| `overflowing_payload_len_does_not_panic` | `STUB_LEN + payload_len` 整数溢出（debug 下 panic） | #229 |
| `bridge_state_is_bounded` | `wxhost.log` 缓冲无上限，页包可吃穿宿主内存 | #253 |

`#[ignore]` 不是"以后再说"：每条都写了 issue 号与取消条件，补丁合入后删掉
`#[ignore]` 就自动变成门禁。
