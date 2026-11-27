# C-03 关闭记录：服务目录不接外部 provider

- 审计基线：`60895e18`
- owner 决策：2026-11-27，C-03 按**内部零钱包自循环**处理，不接外部 provider
- 结论：**C-03 关闭（移出外部履约范围）**

## 事实

生产组合根 `server/src/discover_mount.rs::assemble` 硬编码：

- `Vec::new()` 供应商列表；
- `UnwiredPayment` 支付端口，任何 `create_intent` / `refund` 都返回 `Unavailable`；
- 目录默认空，由管理后台配置。

因此生产路径上：

1. 报价阶段 `OrderService::quote` 先 `require_provider`，没有 provider 即 `BLOCKED_EXTERNAL`（HTTP 501），不会产生可下单报价；
2. `create` 在 `place_order` 之前就被 provider 校验挡住，走不到“先履约”；
3. 即使未来有人误传 provider，支付端口仍是显式拒绝，不会伪造扣款；
4. 不存在 provider 配置项或环境变量。

“先履约后付款、补偿不完整”的代码仍在 `order.rs` 的条件集成分支里，但生产不可达。审计原文把它标为“条件集成；默认已安全”，owner 现在进一步决定**不集成**。

## 验证

```bash
cargo test -p wxlike-service-catalog
```

覆盖：无供应商凭据 → 501（`tests/http.rs`）、支付拒绝/退款失败进对账、订单事实落盘与重启恢复。`server/src/discover_mount.rs` 新增 `production_has_no_external_provider`：`production_providers()` 必须为空，生产装配的“空 provider + UnwiredPayment”由测试钉住。

## 重新引入的条件

若以后要接真实生活缴费/机酒/医疗供应商，必须先实现完整 saga：支付成功后再 `fulfill`、失败补偿可重试、对账终态可收敛，再打开 provider 接入。当前不接，故不把 C-03 留成“代码已修但未验收”的假关闭。
