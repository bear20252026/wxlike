# H-42：逐会话事件游标与重放

审计条目：`H-42 客户端全局 cursor 与 per-conversation seq 不匹配`（潜在/API；Web 当前被 C-04 挡住）。

## 修复

1. `crates/types/src/cursor.rs`
   - 引入 v2 游标：`2:<kind>:<conv>:<seq>`，由 `Cursor::at_conv` 生成。会话 id
     允许包含 `:`，解析时从右侧拆出 `seq`。
   - v1（`1:<kind>:<seq>`）继续可解，`Cursor::at` 仍输出 v1 形状，避免消息历史
     等既有调用方与 fixture 被破坏。v1 解码后 `conv = None`。
   - `CursorValue` 新增 `conv: Option<ConvId>`；失去 `Copy`，保留 `Clone`。
2. `crates/client-core/src/ports/paging.rs`
   - `cursor_floor` 改为 `event_cursor_floor(cursor, conv)`：游标必须是事件游标，
     且其绑定会话必须与读取目标相同，否则报 `Corrupt`。
   - `check_cursor_advance` 解码当前值与新值后，先比较 `kind` 和 `conv`，作用域
     不同直接拒绝；只在同一作用域内比较 `seq`，不再允许用 A 会话的高位序号
     把 B 会话游标“推过去”。
   - `page_events` 增加 `conv` 参数，按会话与 `seq > after` 同时过滤。
3. `crates/client-core/src/ports/store.rs`
   - `events_after(conv, cursor, limit)`：`Some` 只读该会话，`None` 只读全局事件
     （presence 等），不是“所有会话”。
   - `cursor(conv)` 按会话读取；`put_cursor` 按游标自带的会话身份分键。
4. `crates/client-core/src/runtime/subscription.rs`
   - 持久化游标时按 `event.conv` 生成 `Cursor::at_conv`；全局事件仍写全局槽。
5. `crates/client-core/src/runtime/session.rs`
   - `replay(conv, limit)` 按会话重放；`None` 只重放全局事件。调用方需要按会话
     分别调用，不能把不同会话的 seq 混在一起排序。
6. `crates/client-native/src/store.rs`
   - `events` 表增加 `conv TEXT` 列与 `events_conv_seq(conv, seq)` 索引。
   - 旧库迁移：检测 `pragma_table_info('events')` 是否已有 `conv`；没有则
     `ALTER TABLE` 补列，再用 `json_extract(body, '$.conv')` 从已存事件 JSON
     回填。回填失败会让开库失败，而不是把全部事件当全局事件。
   - 游标键：`cursor:conv:<id>` / `cursor:global`。旧 `cursor` 键故意不复用：
     旧值是多会话混用的全局序号，继续读会跳过新会话的事件。
7. `crates/client-web/src/idb.rs`
   - IndexedDB 游标键：`m:cursor:<conv>` / `m:cursor:global`；旧 `m:cursor` 同样
     不复用。
   - `events_after` 先按作用域取游标下限，再 `page_events` 过滤会话。
8. `crates/client-web/src/ws.rs`
   - 建连时不再把单一 cursor 的 seq 套给 `request.convs` 里的每个会话。
     带会话的 v2 游标只用于它绑定的会话；无会话的 v1 游标仅在订阅恰好一个
     会话时兼容套用；多会话且游标无会话身份时不初始化 `Resume`，由各会话
     自己的事件推进。

## 为什么不是“把全局 cursor 换成 Map 就完了”

- 只换存储结构、不改游标协议，服务端/浏览器仍会把一个不带会话的 token
  回传；重连恢复时无法判断它属于哪个会话。
- 只给 cursor 加会话、但 `check_cursor_advance` 仍按 `seq` 比较，A 会话收到
  较大的 seq 后，B 会话较小的 seq 会被判成回退，表现为误报 `Corrupt` 或丢
  事件。
- 旧本地库的全局 cursor 如果继续复用，升级后会用旧的多会话最大 seq 去读
  单个会话，直接跳过该会话早期事件。故旧键保留但不再读取。

## 测试与证据

- `cargo test -p wxlike-types --lib cursor`：7 passed，新增/覆盖：
  - v2 游标可往返包含 `:` 的会话 id；
  - v1 token 仍解码为无会话游标；
  - 每种 kind 往返后 `conv == None`；
  - 未知 kind 仍报 `UnknownKind` 而不是猜。
- `cargo test -p wxlike-client-core`：全部通过（`ports_contract` 12、
  `runtime_contract` 21）。
  - `event_cursors_advance_independently_per_conversation`：A1、B10、A2、B11
    乱序消费后，A 游标停在 2、B 停在 11，两个会话分别重放都为空。
  - `replay_only_returns_the_requested_conversation`：A1、A3、B2 中只重放
    请求的会话。
  - `a_scoped_cursor_is_never_squeezed_into_one_global_slot`：写 A 游标后全局
    槽为空；拿 A 游标读 B 报 `Corrupt`。
- `cargo test -p wxlike-client-native --test sqlite_contract`：10 passed。
  - `scoped_cursors_and_replay_are_per_conversation_across_reopen`：A1、A2、
    B10 写两个会话游标，关库重开后 A/B 游标与各自重放结果保持独立，全局槽
    仍为空。
  - `a_legacy_events_table_gains_its_conversation_column_on_open`：手工建旧
    `events` 表（无 `conv` 列）并写入带 `conv` 的 JSON，开库后能按会话读回，
    验证 `ALTER TABLE` + `json_extract` 回填。
- `cargo check -p wxlike-client-web --target wasm32-unknown-unknown --all-targets`
  通过；真实浏览器用例 `idb_cursors_and_replay_are_per_conversation` 由 CI
  的 `wasm-pack test --headless --chrome crates/client-web` 执行。
- `cargo fmt --all -- --check` 通过。
- `cargo clippy --workspace --all-targets -- -D warnings` 通过；
  `python3 scripts/count_unwraps.py --fail-on-increase` 通过；
  `python3 scripts/platform_surface_audit.py --max-pub 0 --check-facades` 通过。
- 受影响 crate 的测试全绿（`wxlike-types`、`wxlike-client-core`、
  `wxlike-client-native`）；workspace 全量测试由 CI 的
  `cargo test --workspace --locked` 在 Linux/Windows/macOS 上执行。

## 边界

- 订阅请求目前仍只有一个 `cursor` 字段；本次修的是“一个游标不能冒充所有
  会话的游标”。若产品需要一次订阅多个会话并从各自断点恢复，需要协议层
  增加逐会话 resume 映射，或由调用方为每个会话分别建订阅。
- 会话内 `seq` 仍是唯一排序依据；本修复不改变服务端对 `seq` 单调性的保证。
- 旧本地库的 `cursor` / `m:cursor` 键不迁移、不删除：它们不再被读取，但
  保留原值不影响新键；彻底清理可另做一次性迁移。
- 没有在真实移动端/浏览器上跑长时间断线重连压测；进程内契约测试与 CI
  浏览器用例覆盖了游标作用域、乱序、重放与旧库迁移。
