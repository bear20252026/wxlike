# UP-OPENIM → `crates/search/src/query.rs`

- 上游：`openimsdk/open-im-server` @ `9502a1a15fe63f6342fb1e2d8068346a82f278d8`
- 上游文件：`pkg/common/storage/database/mgo/msg.go`（`(*MsgMgo).searchMessage`，约 324–397 行）
- 交叉核对：`bluexsx/box-im` @ `58be3e1fc48fee2f6655576f9aefc80a491dc1e5`，
  `im-platform/src/main/java/com/bx/implatform/service/impl/GroupMessageServiceImpl.java`
  （`loadHistoryMessage`，约 348–372 行）+ 同仓 `GroupMessageHistoryDTO.java`
- 决策：`adapted`，许可证 `Apache-2.0`

## 借了什么（能在上游原文逐行对上）

1. **一天 = 半开区间 `[dayStart, dayStart+24h)`。** 上游把客户端的
   `req.SendTime`（一个 `time.DateOnly` 日期串）解析后写成
   `send_time >= sendTime.UnixMilli()` **且** `send_time < sendTime.Add(24h).UnixMilli()`。
   本文件的 `TimeRange` 因此只有半开语义，`contains()` 右端不收；
   `TimeRange::from_date()` 就是这条规则的 Go→Rust 改写。
2. **日期解析失败 = 拒绝，不是"不加约束"。** 上游 `time.Parse` 报错时返回
   `errs.ErrArgs.WrapMsg("invalid sendTime", ...)`，请求直接 400。
   本文件对应 `QueryError::MalformedTimeRange`（新增变体），
   `from_date` / `between` 都走它。这一条是移植的真正动机：我们原来没有
   时间窗参数，如果按"解析不了就当没传"写，`?sendTime=` 手滑会拿到全库结果。
3. **facet 非零才加进 filter = 缺省不加约束。** 上游 `searchMessage` 里
   `RecvID / SendID / ContentType / SessionType / SendTime` 全部是
   `if req.X != "" { filter[...] = ... }`；box-im 的 DTO 更直白：
   `maxSeqNo` 注释写着"0或负值表示不限制"。两个上游独立给出同一口径，
   所以 `TimeRange::UNBOUNDED` 与 `Option<TimeRange>` 的缺省被定义成
   **不加约束**，并在 doc 里明写它**不等于放宽可见范围**（放宽只由授权裁决决定）。
4. **窗口下界由服务端往"现在"收。** box-im 取
   `minDate = max(now - MAX_OFFLINE_MESSAGE_DAYS, member.CreatedTime)`，
   即客户端给的起点不能把下界往前推。本文件的 `TimeRange::narrow_to`
   就是这个 `max` 再加跨度上限与"未来不检索"上界的组合。

## 改了什么

- 上游是 Mongo filter DSL，这里是类型：新增 `TimeRange`（`start_ms/end_ms` 私有字段 +
  `between / from_date / today_until_now / contains / narrow_to / intersect / UNBOUNDED`）。
- 上游用 Go 标准库 `time.Parse(time.DateOnly)`；本仓不引 `chrono`
  （根 `Cargo.lock` 是串行热点，不允许为检索层加依赖），
  所以 **`days_from_civil` / `civil_from_days` 该处自研**：算法是公知的
  civil↔fixed 互换（Hinnant 式 153 天月/146097 天世纪整除折叠），
  但代码是本仓写的，不是从任何上游复制的。负日序刻意走地板除，
  用 `1969-12-31 == -1` 这条用例把截断除法的错钉死。
- 加了上游没有的显式拒绝：`between(end <= start)` 报错。上游对
  `startTime/endTime` 不做区间合法性检查（空区间就静默返回 0 条）。
- `MAX_RANGE_SPAN_MS = 92d` 是自研的服务端常量（上游没有对应物），
  用来把"检索最近十年"这种请求砍到可控跨度。
- `today_until_now` 的上界收到 `now`：上游只加 24h，不看时钟。

## 没借什么

- 上游 `RecvID` 展开成 `$or(recv_id, group_id)`（一个公开 id 同时覆盖单聊与群聊两种
  形态）。这条属于**消息域的 provider 实现**，落在 G02 的 crate 里，本文件无处安放，
  因此没有移植；已在 PR #365 评论里作为给 G02 的建议提出。
- 上游的 `doc_id` 前缀粗筛（`^sg_` / `^si_`）是 Mongo 分片键优化的产物，
  我们的存储是 SQLite/tantivy，形状不对应，未借。
- 上游 `Pagination`（`pageNumber/showNumber`）是 offset 分页。本文件不引入 offset：
  检索侧继续用 keyset 游标（`parse_cursor` 原有语义不变），offset 会翻重复页。

## 哪个测试覆盖

`crates/search/src/query.rs` 内 `#[cfg(test)] mod tests`（`cargo test -p wxlike-search`）：

- `time_range_is_half_open_and_strict` —— 左闭右开 + 13 个畸形日期串（含 `2026-02-29`
  平年、`2026-11-7` 单位数月、带 `T` 的混合串、首尾空白）一律
  `Err(MalformedTimeRange)`；`2024-02-29`/`2000-02-29` 放行。
- `civil_day_math_matches_known_epochs` —— 自研日期算法对齐真值
  （`1970-01-01 == 0`、`1969-12-31 == -1`、`2000-03-01 == 11017`、`2026-11-07 == 20764`）
  并与 `civil_from_days` 互逆。
- `inverted_or_empty_window_is_refused_not_empty_result` —— 反区间/空区间拒绝；
  完全在未来的日期拒绝；now 落在当天时右端收到 now。
- `server_clamp_only_ever_narrows` —— 无界请求被 `narrow_to` 收成
  `[floor, cap)`；跨度上限以 `end` 为锚往前砍；`cap < floor` 不退化成反区间。
- `intersect_reports_empty_as_none` —— 求交空集报 `None`，无约束求交是恒等。
