# UP-OPENIM → `crates/search/src/unified.rs`

- 上游：`openimsdk/open-im-server` @ `9502a1a15fe63f6342fb1e2d8068346a82f278d8`
- 上游文件：`pkg/common/storage/database/mgo/msg.go`
  （`(*MsgMgo).searchMessageIndex` 约 262–322 行、`(*MsgMgo).searchMessage` 约 324–397 行、
  `(*MsgMgo).SearchMessage` 约 399–432 行）
- 交叉核对：`bluexsx/box-im` @ `58be3e1f…`，
  `GroupMessageServiceImpl.java` 第 534–535 行 `orderByDesc(SendTime).orderByDesc(Id)`
- 决策：`adapted`，许可证 `Apache-2.0`

## 借了什么

1. **同一个时间窗要量两道。** 上游管道对 `filter`（含 `send_time` 半开区间）先做
   **文档级** `$match`，`$unwind` 之后再用 `$filter`/`$match` 对**数组元素逐条**量一遍。
   这不是冗余：文档级命中只保证"这批消息里至少一条在窗内"，不保证"全在窗内"。
   本文件的对应落点是 `Lane::fill`：窗口既下传给 `Provider::candidates`（让存储层能走
   索引），也在归并层按 `candidate.created_at` 再 `contains` 一次。窗外的行
   **不进 `authorize`、不进 `project`、不占结果位**，但仍然推进游标——
   这一点是它不会死循环的原因，和上游 `$match` 之后继续往下翻同构。
2. **候选（无正文）与正文投影分两段。** 上游 `searchMessage` 只回
   `(doc _id, msgs[]._search_temp_index)` 这种**位置对**，`SearchMessage` 再按
   `_id` 逐条 `FindOne` 取正文。本文件的
   `Provider::candidates → authorize → project` 三段就是这个形状（这条本仓
   先前已有，移植后把它和窗口绑到同一条流水线上，见不变式 4）。
3. **批量下限。** 上游 `const maxDoc = 50`：一次聚合先取 50 个文档再在内存里凑页。
   本文件已有同构常量 `MIN_BATCH = 32`，语义一致，未改数值。
4. **排序键带 tie-break。** 上游按 `_id` 升序扫文档、按数组下标取行；
   box-im 独立给出 `send_time DESC, id DESC` 的复合排序。本文件的
   `(created_at DESC, doc_id DESC)` 与后者对齐（该处原本即自研，移植时核对上了口径）。

## 改了什么

- `Provider::candidates` 增加 `range: Option<&TimeRange>` 入参。全仓检索后确认
  该 trait 目前只有本文件的测试 `Fake` 实现（`server` 只用 `SearchIndex`，
  平台侧 `content_index.rs` 走自己的 SQLite 泳道），所以签名变更不跨组破坏。
- `SearchService::search_in(...)` 新增，`search(...)` 保留并委托为 `range = None`，
  避免把 6 参数强推给所有调用点。
- `Lane` 多带一个 `range: Option<TimeRange>`（`TimeRange` 是 `Copy`）。
- 测试 `Fake` 的粗筛**故意写成按天对齐**（`day_floor < end && day_floor + DAY > start`），
  为的是让"provider 下推不干净"这件事在单测里真的发生，而不是只在注释里声称。
- 窗外的行被丢弃时不改 `done` 判定：`done` 仍然只看
  `page.len() < batch`（存储层是否见底），避免把"这一批都不在窗内"误判成"翻到底了"。

## 没借什么

- **上游的 `total`。** `searchMessage` 返回的 `count` 是"扫到即止"的计数：
  循环在凑满一页后只多探 `push < -10` 那点余量就 return，所以它既不是全库命中数、
  也不是稳定值，同一个查询翻两页可能给出两个 total。在游标分页的对外契约里
  这种数会撒谎，因此**不移植**：`Page` 继续只有 `items/next_cursor/has_more`。
- **offset 分页 `(pageNumber-1)*showNumber`。** 上游是页号分页，我们是 keyset 游标；
  换过去会让"翻页期间新内容顶掉旧内容"重新出现（这正是本文件存在的理由，不变式 2）。
- **上游越界下标的静默跳过** `if i >= len(res.Msg) { continue }`。
  语义上对应我们的 `project()` 返回 `None` 即丢行；但上游那处是防御数组越界，
  我们这里是"索引里剩死行"，裁决点不同，没有代码可借。
- **`authverify.CheckAdmin(ctx)`。** 上游的 `SearchMessage` RPC 是**管理员专属**
  （跨全库消息检索）。我们的 `/api/search/content` 是登录用户检索**公开内容**，
  域完全不同，不能照搬 auth 等级；但这条已经在 PR 评论里作为
  `/api/search/scopes` 与授权矩阵判断的参照依据列出。
- **上游查不到用户资料时补桩保留行**（`GetActiveUser` 里
  `pbUser == nil → 造一个 UserID 当昵称`）。检索里死行必须消失，
  不能对外"现形"一条已删除内容的标题，故未借。

## 哪个测试覆盖

`crates/search/src/unified.rs` 内 `#[cfg(test)] mod tests`（`cargo test -p wxlike-search`）：

- `coarse_provider_window_is_narrowed_by_the_merge_layer` —— provider 按天粗筛放过
  `m0500`，归并层必须按半开区间丢掉它（不变式 4 的直接回归用例）。
- `absent_range_is_no_constraint_not_a_widening` —— 同一份数据不传窗口时三条都出，
  证明"缺省 = 不加约束"而不是"缺省 = 换一种放宽"。
- `window_fully_outside_yields_empty_not_everything` —— 窗外窗口回空页而非全量，
  并且候选只被取过一次（游标照旧推进，不死循环）。
- `window_survives_paging` —— 第二页仍然逐行量窗口；窗外行不会被当成 `has_more`。
- `one_day_window_is_half_open_across_the_boundary` —— 次日 00:00 不属于这一天。
- 既有用例 `invisible_candidates_do_not_truncate_the_page`、
  `cross_scope_merge_is_global_order`、`paging_covers_every_visible_row_exactly_once`、
  `repeated_paging_does_not_rescan_the_whole_index` 全部仍然通过（签名改动没改行为）。
