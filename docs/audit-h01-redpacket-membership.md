# H-01 关闭记录：红包领取要求会话成员资格

- 审计基线：`60895e18`
- 问题：`crates/platform/src/domain/wallet.rs::claim_redpacket` 只校验红包状态、自领、领完和重复领取，不校验 `redpackets.conv_id` 的成员资格；知道红包 id 的任意认证用户都能领。
- 结论：**H-01 关闭**。

## 修复

1. `claim_redpacket` 先在事务外做一次平台成员判定（`conv_member_allowed`），非成员直接 `Forbidden`，不进入资金事务。
2. `claim_redpacket_tx` 再在**同一个资金事务连接**上复核：直聊按 `d-a~b` 解析，群按 `group_header` + `member_ids` 查成员；被踢出群与领取并发时，事务内这一眼会拒绝。频道没有群成员表，沿用事务外的平台判定。
3. `conv_id == 红包 id` 是“没有落到具体会话”的存量回退编码，不当作会话，避免把无会话红包误判成非法群。
4. 非成员统一返回 `Forbidden`（HTTP 403）；红包名额 `claimed` 和 `claimed_total` 都不动。

## 验证

```bash
cargo test -p wxlike-platform --lib
cargo test -p wxlike-platform --test wallet
```

- `domain::wallet::ephemeral_tests::non_member_cannot_claim_a_packet_bound_to_a_conversation`：非成员拒绝、名额与金额不变、成员仍可领。
- `non_member_claim_is_forbidden_over_http`：HTTP 403，随后成员领取 200。
- 既有并发/守恒/群卡片用例全部通过；无会话红包（回退到自身 id）仍可领。

## 边界

频道红包的成员判定仍在事务前（频道成员表不在 `group_members`）；直聊和群已在事务内复核。若以后频道红包成为主要路径，应把频道成员查询也下推到 `StorageConnection`。
