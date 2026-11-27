# H-34：令牌侧车缺失时 fail closed

- 审计基线：`60895e18`
- 修复提交：待填
- 审计条目：H-34「Token sidecar 缺失时 fail open」

## 根因

`crates/features/identity/src/session.rs` 的 `authenticate_with` 在查到访问会话后调用
`check_token_sidecar`。旧实现把 `sid` 为空、或 `identity_session_meta` 无对应行，都当作
“平台历史数据、没有额外约束”，直接返回 `Ok(())`。结果是：

- 侧车被误删/损坏时，令牌绕过全局序列号轮转和 auth level 检查；
- `sid` 为空的旧会话完全跳过撤销约束；
- 侧车行的 `user_id` 与访问会话不一致时也没有校验。

## 修复

- `sid` 必须存在且非空；否则按“无效或过期令牌”拒绝。
- `token_record_with` 返回 `None` 时直接拒绝，不再走 fail-open 兼容分支。
- 侧车 `user_id` 必须与访问会话的 `user_id` 一致，否则同样按无效令牌拒绝。
- 三种拒绝与“令牌不存在”使用同一错误文案，避免把内部状态变成可探测信号。

## 测试

`cargo test -p wxlike-identity --lib`：92 passed。

新增两条回归：

- `missing_token_sidecar_fails_closed`：创建正常会话后删除侧车行，原 access token 必须 `Unauthorized`；
- `token_sidecar_must_belong_to_the_same_user`：把侧车 `user_id` 改成另一个用户，原 token 必须 `Unauthorized`。

`cargo fmt --check`、`cargo clippy -p wxlike-identity --lib -- -D warnings` 通过。

## 边界

- 旧库若仍有 `identity_session_token` 历史行，启动迁移会先回填到
  `identity_session_meta`；回填不到、且 `sid` 缺失的会话现在需要重新登录，这是有意的
  fail-closed 取舍。
- 没有做“兼容窗口”配置；审计要求 legacy 兼容必须明确、短暂、可审计，本轮选择直接关闭
  兼容路径，避免再引入一个默认放行的开关。
- 本项只改认证侧车，不涉及 H-13 的 session token/TOTP seed 静态加密。
