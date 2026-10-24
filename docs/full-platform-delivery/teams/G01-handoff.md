# G01 交付说明（Rust 共享核心与绑定）

组跟踪 [#82](https://github.com/bear20252026/wxlike/issues/82) ·
总跟踪 [#80](https://github.com/bear20252026/wxlike/issues/80)

本文件是 G01 的**交接说明**：做了什么、边界在哪、下一组接什么。任务单本身在
[`docs/full-platform-delivery/teams/G01.md`](../../docs/full-platform-delivery/teams/G01.md)，
跨组改动在 [`integration-requests/G01/`](../../integration-requests/G01/)。

## 一句话

把"客户端"从**三份各写一遍的实现**收成**一份核心 + 三个平台适配**：

```text
        UI（React / Flutter / 将来的其它）
                    │  dispatch(ActionRequest) / subscribe(cursor)
                    ▼
        crates/client-core        ← 业务编排：路由、重试、先落盘后回话
          │  ports（六个端口）
   ┌──────┼────────┬──────────┬──────────┬──────────┐
   ▼      ▼        ▼          ▼          ▼          ▼
 Transport Store  Clock    Secrets   Platform  Notification
   │       │        │          │          │          │
   ├── client-native（ureq / SQLite / 文件锁 / 设备密钥）
   └── client-web（fetch / IndexedDB / WebCrypto / 租约锁）
                    │
                    ▼
              crates/ffi（CoreHandle：排队 + 拉取）
```

## 五张卡分别落了什么

| 卡 | 交付 | 关键文件 | 验收证据 |
|---|---|---|---|
| G01-01 | 共享契约类型 + 六个端口 | `crates/types/src/*`、`crates/client-core/src/ports/*` | `crates/types/tests/*`（fixture 三端一致、2^53 精度、依赖树护栏、与旧实现逐字 parity） |
| G01-02 | 原生核心：单实例会话、原子 outbox、事件先落盘后交付、退避补发 | `crates/client-core/src/runtime/*`、`crates/client-native/src/*` | `client-core/tests/runtime_contract.rs`（13 例）、`client-native/tests/*`（SQLite 真实现 + 崩溃重开） |
| G01-03 | 浏览器闭环：fetch / IndexedDB / 租约锁 / 生命周期 / 敏感缓存加密 | `crates/client-web/src/*` | `client-web/tests/browser.rs`（wasm-bindgen-test，真实浏览器）+ `policy.rs` 原生单测 |
| G01-04 | `CoreHandle`：投递 + 拉取、可取消、销毁后拒绝晚到、panic 不跨 FFI | `crates/ffi/src/core_handle.rs`、`tools/codegen/dart/` | `crates/ffi/tests/core_handle.rs`（5 例） |
| G01-05 | 一次生成 + `--check`、兼容性闸门、Action Catalog → TS/Dart/CLI/MCP | `tools/codegen/generate.py`、`tools/codegen/out/*` | `generate.py --check`；`contract.lock.json` 守破坏性变更 |

## 六条不变量（改动时别破坏）

1. **ID 不透明**：`UserId` 之类只能 `parse`，字段私有。传错类型 = 编译错误。
2. **金额与序列号是字符串**：`{"minor":"9007199254740993"}`。用数字会在 JS 的
   `2^53` 处静默丢精度，且没有任何报错。
3. **写命令必须先落盘再回话**：`submit_write` 的顺序是 `put_outbox → send →
   mark_done`。反过来的话，"界面显示已发送 + 杀进程"就会丢消息。
4. **事件先落盘再交付**：`EventSubscription::next` 里 `append_event` 在返回之前。
5. **错误映射只有一处**：`DomainError::from_http_status` 与
   `TransportError::into_domain`。别在调用点各写一遍。
6. **生成物不手改**：`tools/codegen/out/*`、`apps/mobile/lib/src/rust/*`。

## 明确的边界（没做的事，以及为什么）

| 没做 | 原因 | 谁接 |
|---|---|---|
| 原生 WebSocket 实时通道 | 桌面走 `ui/src-tauri` 的 WS，移动走推送 + 拉取；这一层不静默降级成轮询 | G04 / B4 |
| 浏览器强互斥（Web Locks） | 用带过期的 localStorage 租约；崩溃即自然过期。极端情况可能短暂双活，靠幂等键兜底 | 见 G01-03-R4 |
| 不可导出 `CryptoKey` | 密钥仍在 localStorage（同源可读）；安全边界是 CSP + 无 XSS | G17 |
| IndexedDB 事件索引分页 | 现在是 `getAll` + 上界 500 条；真相在服务端，事件是可重建缓存 | 后续卡 |
| 旧 Tauri command 删除 | 页面还在调，先薄适配器过渡 | G04 |

## 本地怎么验

```bash
cargo test -p wxlike-types -p wxlike-client-core -p wxlike-client-native -p wxlike-ffi
cargo check -p wxlike-client-web --target wasm32-unknown-unknown
node crates/types/fixtures/run_conformance.mjs
dart run crates/types/fixtures/run_conformance.dart
python tools/codegen/generate.py --check
```

浏览器行为（IndexedDB / fetch / 租约锁）：

```bash
wasm-pack test --headless --chrome crates/client-web
```

> 项目约定：**构建与验证走云端 CI**。上面这些命令本地只用来快速定位问题，
> 门禁以 PR 上的 CI 为准（见 `docs/task` 的 DoD 说明）。

## 分支与提交的说明

五张卡共享两个文件（`Cargo.toml` 的 `members`、`crates/client-core/src/lib.rs`
的模块声明），逐卡独立分支会让中间那次提交**编译不过**。所以本组按
`feat/FP-G01-01` 一个分支、五个提交交付，提交信息带卡号，评审时按卡看 diff。
