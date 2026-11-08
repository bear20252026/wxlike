# UP-OPENIM → `crates/search/src/trends.rs`

- 上游：`openimsdk/open-im-server` @ `9502a1a15fe63f6342fb1e2d8068346a82f278d8`
- 上游文件：`pkg/common/storage/database/mgo/msg.go`
  （`(*MsgMgo).RangeUserSendCount` 约 434–681 行、`RangeGroupSendCount` 约 683 行起）
- 相关上游文件（未登记为来源，仅口径核对）：
  `internal/rpc/msg/statistics.go`（`GetActiveUser` / `GetActiveGroup`，
  `time.UnixMilli(req.Start)` / `time.UnixMilli(req.End)` 直传）、
  `pkg/common/storage/controller/msg.go`（`SearchMessage` 的 IsRead/Revoke 投影）
- 决策：`adapted`，许可证 `Apache-2.0`

## 借了什么

1. **先排名、后切片。** 上游聚合管道把 `$sort: {count: sort}` 放在 `$group` 之后，
   最后一步才 `$slice: {"$users", pageNumber - 1, showNumber}`。
   于是 `msg_count`、`user_count`、`dateCount` 三个返回值**始终是整窗的总量**，
   翻页只切 `users` 这一段列表。本文件的 `rank_trends_paged` + `TrendStats`
   就是这条分界的 Rust 版：`collect()` 一次算完全量排名和基数，
   `skip/take` 只作用于 `TrendRanking::terms`，`stats` 与页号无关。
   （这条界线不写清楚就会长出经典错误：翻到第 3 页时"总热度"也跟着变小。）
2. **按 UTC 日历日分桶，键是 `%Y-%m-%d`。** 上游
   `$dateToString: {format: "%Y-%m-%d", date: {$toDate: "$$item.msg.send_time"}}`。
   本文件 `utc_date_key(ms)` 生成同样的 `YYYY-MM-DD` 键（日序用地板除，
   1970 之前的时间戳也落对）；`TrendStats::date_counts` 对齐上游的 `dateCount map`。
3. **日桶计数不套 owner_cap。** 上游 `dates` 的 `$sum: 1` 作用在 unwind 之后的
   原始行上，而按人计数的 `users` 才是排名主体——所以刷量者的 500 条会把
   当日活跃曲线顶到 500，但压不动榜单分数。我们照这个口径实现，并在 doc
   里写明"曲线压了就不可信"是错的：曲线要的就是真实流量，分数才是抗刷量的那个。
4. **空窗口返回零值而不是报错。** 上游 `if len(result) == 0 { return 0, 0, nil, nil, errs.Wrap(err) }`
   （此处 `err` 为 nil，即返回全零 + 无错误）。本文件对"交集为空"的窗口
   返回 `TrendRanking{terms: [], stats: 全零, ranked_total: 0}`，不抛错。
5. **时间窗半开 `[start, end)`。** 上游 `$gte: start, $lt: end`（文档级 `$match` 和
   元素级 `$filter` 两处都是同一对边界）。本文件的窗口一律走
   `TimeRange::contains`；既有 `window_ms` 语义用 `[now-window, now+1)` 表达，
   保住"恰好在 now 这一毫秒的行也算数"这条原有用法。

## 改了什么

- `rank_trends` 保留原签名与原行为（不传窗口 = 只用服务端窗口），内部改走 `collect()`。
- 新增 `TrendStats` / `TrendRanking` / `rank_trends_paged` / `utc_date_key`。
- 客户端窗口与服务端窗口取**交集**，交集为空即空榜——上游没有这层服务端窗口，
  它信 `req.Start/req.End` 直传，客户端可以要"全历史"。这条收紧是本仓不变式
  （"窗口是服务端决定的，客户端不能调成全历史"），属于自研叠加。
- `TrendOptions::limit` 在分页入口下变成**整榜长度上限**，`page_size` 管这一页几个，
  切片发生在 limit 之内。
- `page` 传 0/负数钳到首页；`page_size` 传 0 按 1。
- **上界新增**：`created_at > now` 的时钟超前行不再入场。旧实现只有下界
  （`created_at < cutoff` 才排除），未来行会常驻榜单——这是本次移植顺带修掉的缺陷。
  因此调整了两处既有测试种子的 `now_ms`（`single_author_cannot_flood_the_board`
  取最后一篇的时刻、`limit_and_min_authors_are_honoured` 同理），断言本身没改。

## 没借什么

- **`ase`（排序方向）由调用方传。** 上游把它一路带到 `$sort: {count: sort}`。
  不移植：榜单顺序在本仓是服务端不变式，允许客户端选升序等于给它一个
  "从最冷的一头把榜枚举完"的入口，也破坏跨进程稳定排序。
- **`group bool` 的 `doc_id` 前缀 `$or` 粗筛**（`^si_` / `^g_` / `^sg_`）。
  那是 Mongo 文档形态的产物，我们的行形状里没有对应物。
- **`$slice` 的负偏移行为**。上游对 `pageNumber` 不校验，传 0 会算出 `start = -1`，
  交给 Mongo 的"从尾部数"语义——这是个真缺陷，我们钳住而不是照抄。
- **`SetAllowDiskUse(true)`**：纯函数聚合，无对应物。
- **`RangeGroupSendCount` 的群维度榜**：本仓热搜目前只有"词条榜"，没有
  "活跃群榜"这个产品面；没有需求就不造轮子，未移植。

## 哪个测试覆盖

`crates/search/src/trends.rs` 内 `#[cfg(test)] mod tests`（`cargo test -p wxlike-search`）：

- `ranking_pages_but_stats_stay_global` —— 同一份 6 篇文档翻第 1/2/3 页，
  `stats` 三次完全相等（`total_docs = 6`、`distinct_owners = 2`、`ranked_total = 6`），
  页与页之间不重不漏。这是"分页只切列表、不动基数"的直接证明。
- `date_counts_bucket_by_day_and_ignore_owner_cap` —— 跨两个 UTC 日子分桶为 5/1，
  而榜单分数侧仍受 `owner_cap = 1` 约束（`doc_count = 2`）；曲线与分数两套口径。
- `utc_date_key_is_the_upstream_bucket_label` —— `%Y-%m-%d` 对齐，含
  `-1ms == 1969-12-31` 的地板除边界与日界两侧。
- `client_window_can_only_narrow_the_server_window` —— 客户端只要第一天 ⇒ 基数 1；
  要"全历史" ⇒ 被服务端窗口收住；要一整段未来 ⇒ 交集空，返回全零而非报错或全量。
- `future_dated_docs_do_not_enter_the_board` —— 时钟超前的行不入榜（回归新修缺陷）。
- `page_number_zero_is_clamped_not_wrapped` —— 页号 0 == 首页；翻过头是空页，
  `ranked_total` 照旧。
- 既有 `window_excludes_stale_posts`、`single_author_cannot_flood_the_board`、
  `repeat_keyword_inside_one_doc_counts_once`、`non_public_never_trends`、
  `more_authors_beat_more_docs`、`limit_and_min_authors_are_honoured` 全部仍通过。
