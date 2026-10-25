# G01 集成请求（交 G00 串行执行）

本目录是 G01 的**跨组改动清单**。G01 只写自己归属的六个路径
（`crates/types`、`crates/client-core`、`crates/client-native`、
`crates/client-web`、`crates/ffi`、`tools/codegen`），凡是需要改根清单 /
CI / 别人目录的，一律登记在这里由 G00 执行。

执行顺序与卡片一致：G01-01 → G01-05。每份 YAML 的 `status` 字段说明现状：

| status | 含义 |
|---|---|
| `applied-pending-ratification` | 为了让 CI 能验证，G01 已临时落地；请 G00 复核后转为正式改动 |
| `requested` | 尚未执行，等 G00 |
| `done` | G00 已执行 |

## 为什么有 `applied-pending-ratification`

新 crate 不加进 `Cargo.toml` 的 `members`，CI 的 `clippy/test/doc` 根本看不到
它们——那是一堆"没人编译的代码"，比不写更糟。所以 G01 先加进去让 CI 能验证，
同时在 R1 里登记，等 G00 复核。G00 若不同意，回退这半行即可（不影响 crate 内容）。
