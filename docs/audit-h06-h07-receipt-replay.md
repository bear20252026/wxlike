# H-06 / H-07：回执归属与重放完整性

审计基线 `60895e18`。

## H-06 receipt 可伪造并指定任意收件人

原 `handle_receipt` 只用 `refers_to` 找作者，不校验目标消息属于 `env.conv_id`，并把客户端 `env.to` 直接并入收件人。知道其他会话 `msg_id` 的认证用户可以在自己有权发送的会话里伪造回执，并让任意用户收到通知。

修复：

1. `Store::message_by_id(msg_id)` 取归档行（含权威 `conv_id`、`sender`、`to`）。
2. `handle_receipt` 要求 `original.envelope.conv_id == env.conv_id`，跨会话直接 drop。
3. 回执人必须是原消息的合法接收者：
   - 直聊：`env.sender` 必须是 `d-a~b` 的对端；
   - 群：`env.sender` 必须在服务端成员表/信箱成员快照里；
   - 无成员表的兼容直聊：`env.sender` 必须在原 envelope 的 `to` 里。
   原消息作者不能给自己回执。
4. 收件人只从归档行取作者，客户端 `env.to` **完全忽略**；伪造的 `to` 不再产生第二个通知。
5. 所有拒绝路径带原因日志（跨会话 / 非接收者 / 作者自回执 / 未知消息）。

测试 `crates/net/tests/reliable_msg.rs::receipt_is_bound_to_the_original_conversation_and_ignores_env_to`：合法 bob 回执即使 `to=[carol]` 也只通知 alice；mallory 在 `d-mallory~carol` 伪造指向 `conv1` 消息的回执被丢弃，alice/carol 都收不到。

## H-07 replay 冲突校验不完整

原 `check_quoted_replay` 只在任一方有 `quote_of` 时比较 sender/conv/body/quote；普通重复 `msg_id` 换正文/收件人也能通过。信箱镜像遇到重复 `msg_id` 还会把当前 `head_seq` 写给重放帧。

修复：

1. `replay_matches` 对重复 `msg_id` 比较规范化完整 envelope：
   `version`、`conv_id`、`sender`、`to`（排序去重后比较）、`kind`、`body`、`sent_at_ms`、`quote_of`、`receipt`；忽略服务端分配的 `seq` 和主键 `msg_id`。
2. `archive_content` 的唯一索引冲突和 `replay_content` 的 seen 命中都走 `check_replay`；任何字段差异返回 `QuoteReplayConflict`，不落库、不镜像、不投递、不分配新 seq。
3. `mirror_content` 重复分支改用 `mailbox.seq_of(owner, msg_id)` 取原消息序号，不再用 `head_seq`；并发 duplicate 不会拿到一个更晚的序号。
4. 新增集成测试 `conflicting_replay_keeps_the_original_and_adds_no_delivery`：同 id 改正文/收件人/时间戳后，原消息仍是唯一行，bob 收不到伪造帧。
5. 新增单测 `duplicate_mirror_keeps_the_original_mailbox_seq`：先写 seq 1、再写 seq 2，重复写第一条仍回填 seq 1。

## 验证

- `cargo test -p wxlike-net --test reliable_msg`：10 passed。
- `cargo test -p wxlike-net --lib mailbox_replay_tests`：1 passed。
- `cargo fmt --check` 通过。

## 边界

- 冲突在 hub 侧以 `QuoteReplayConflict` 错误返回并记录 drop；v3 线协议没有消息级错误帧，WebSocket 客户端当前只能通过“未收到投递 + 重试”感知，HTTP `HubHandle::submit` 调用方则直接拿到错误。后续若要给客户端明确错误码，需要新增线协议 `SendReject` 变体（会改动 C-04 的字节顺序契约）。
- v1 无 `msg_id` 的帧不参与去重/重放比较，沿用旧行为。
