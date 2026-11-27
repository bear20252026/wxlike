# H-16：directed-share audience 存而不用

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `c9d73a0`。

审计原文：`AUDIT-60895e18.md:238-243`。`create_share` 把 `Audience` 写进
`media_share`，但 `resolve_share` 只读 object/expiry/revoked，不检查 caller；
HTTP 下载接受 token 后也不校验 audience。指定给某用户/会话/群的分享实际是
bearer public share。

## 修复

`crates/features/media/src/files.rs::resolve_share(token, caller)`：

1. 查询增加 `owner` 与 `audience` 两列；过期/撤销仍在 audience 检查之前返回
   `Gone`/`NotFound`。
2. 每次解析都反序列化 audience 并判定：
   - `Public`：允许匿名；
   - `User { id }`：caller 必须等于 id；
   - `Conversation { id }`：`AudiencePort::may_read_conversation(caller,id)`；
   - `Group { id }`：`AudiencePort::may_read_group(caller,id)`。
3. 非受众返回 `Forbidden`；owner 始终可以打开自己的分享（便于发送前预览）。
4. audience 反序列化失败返回 `Tampered`，不降级成 public。

`crates/features/media/src/http.rs::download` 用注入的 `SessionVerifier` 解析
`Authorization`，把 caller 传给 `resolve_share`。匿名访问只对 `Public` 分享有效；
带 token 但非受众得到 403。

## 证据

- `cargo test -p wxlike-media --test g04`：26 passed。
  - `directed_shares_check_the_audience_on_every_resolve`：
    - `Audience::User{u2}`：匿名失败、u3 失败、u2 通过、owner u1 预览通过；
    - `Audience::Group{g1}`：u2 通过、u3 失败、匿名失败；
    - 撤销后 u2 再解析失败。
  - `shares_are_revocable_bearer_references`：public 分享匿名可读，撤销后不可读。
- `cargo test -p wxlike-media --test http_api`：5 passed。
  - `http_directed_share_requires_the_audience`：`tok-b`（u2）下载 200 且拿到
    bytes；owner `tok-a` 可预览；匿名请求同一 URL 返回 403。

## 边界

分享 URL 仍携带 bearer token；audience 是额外的访问约束，不是把 token 变成
不可转发。真实会话/群成员判定由 composition 注入的 `AudiencePort` 提供，测试用
fixture；生产接 G02/G03 后需要真账号回归。过期时间仍由 `expires_at` 控制，未在本项
新增短时刷新或一次性语义。
