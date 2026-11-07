//! 热搜聚合（G05-05）。**纯函数**：输入是已经按时间窗取好的行，输出是排名。
//!
//! 刻意不碰数据库——榜单逻辑要能被显式种子的单元测试钉死（issue #135 的
//! "测试数据显式种子"），而"从哪张表取行、取哪些行"留给调用方。
//!
//! 去刷量的三层：
//!
//! 1. **窗口**：只有 `now - window_ms ..= now` 内的内容参与，旧帖不能常驻；
//! 2. **单作者贡献上限** [`TrendOptions::owner_cap`]：同一个人写 500 篇同一个
//!    词，最多只按 `owner_cap` 篇计入，其余全部砍掉；
//! 3. **按去重作者计分**：`score = distinct_authors * 100 + capped_docs`，
//!    热度首先看"多少人有话说"，不是"多少条记录"。
//!
//! 另外一篇文档内同一个词只算一次（[`crate::query::terms`] 已去重），
//! 所以"把关键词重复三十遍"不会提高任何分数。
//!
//! 聚合的**分页/统计基数**分界借自 OpenIM 的 `RangeUserSendCount` /
//! `RangeGroupSendCount`：它先把全窗的消息按人/按天聚完，再用 `$slice`
//! 只切**排名列表**，`msg_count`、`user_count`、`dateCount` 三个总数始终
//! 按整窗返回，不受分页影响。这条界线不写清楚就会长出一个经典错误——
//! "翻页翻到第 3 页时榜单总数也跟着变小"。见 [`rank_trends_paged`]。
//!
//! 没借的两处（都在同一个函数里，故意留着）：
//! * 上游把排序方向 `ase` 交给调用方传。我们不让：榜单顺序是服务端不变式，
//!   客户端可选升序等于给它一个"从最冷的一头把榜刷完"的枚举面。
//! * 上游对 `pageNumber` 不做校验（传 0 会算出 `start = -1` 交给 `$slice`
//!   的负偏移语义）。[`rank_trends_paged`] 直接 `max(1)` 钳住。

use std::collections::{BTreeMap, BTreeSet};

use crate::query::{civil_from_days, terms, DAY_MS};

/// 一条参与聚合的文档（只用到这四个字段，其余留在权威表）。
#[derive(Clone, Copy, Debug)]
pub struct TrendDoc<'a> {
    pub owner: &'a str,
    /// 只有 `public` 投影进热搜；好友可见/群可见/私密一律跳过。
    pub audience: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub created_at: i64,
}

#[derive(Clone, Debug)]
pub struct TrendOptions {
    /// 时间窗（毫秒）。窗口是**服务端**决定的，客户端不能传"全历史"。
    pub window_ms: i64,
    /// 榜长上限。
    pub limit: usize,
    /// 单作者对单个词最多贡献几篇。
    pub owner_cap: usize,
    /// 至少多少个不同作者提及才上榜（1 = 不因冷门而过滤）。
    pub min_authors: usize,
}

impl Default for TrendOptions {
    fn default() -> Self {
        Self {
            window_ms: 7 * 24 * 60 * 60 * 1000,
            limit: 20,
            owner_cap: 3,
            min_authors: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrendTerm {
    pub term: String,
    pub score: i64,
    pub doc_count: i64,
    pub distinct_authors: i64,
}

/// 聚合结果的**全窗基数**，与分页无关。
///
/// 口径对齐上游 `GetActiveUserResp`：`total_docs` ≈ `msg_count`（上游是对
/// 分组后的 `count` 求和，这里是对入窗文档求和，同一件事）、
/// `distinct_owners` ≈ `user_count`、`date_counts` ≈ `dateCount`
/// （键是 UTC 日历日 `YYYY-MM-DD`，值是**原始条数**，不套 [`TrendOptions::owner_cap`]
/// —— owner_cap 只用来压榜单分数，日活曲线压了就不可信）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrendStats {
    pub total_docs: i64,
    pub distinct_owners: i64,
    pub date_counts: BTreeMap<String, i64>,
}

/// 一页榜单 + 全窗基数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrendRanking {
    /// 本页的词条（已经按 `score DESC, term ASC` 排好再切片）。
    pub terms: Vec<TrendTerm>,
    /// 全窗基数：翻页翻到第几页都不变。
    pub stats: TrendStats,
    /// 排名全长（切片前）。客户端据此判断还要不要继续翻。
    pub ranked_total: usize,
    pub has_more: bool,
}

/// 聚合 + 排名，取前 `opts.limit` 名。窗口就是服务端那句
/// `[now - window_ms, now]`，不带客户端约束（要约束用 [`rank_trends_paged`]）。
pub fn rank_trends(docs: &[TrendDoc<'_>], now_ms: i64, opts: &TrendOptions) -> Vec<TrendTerm> {
    let window = server_window(now_ms, opts);
    let (ranked, _) = collect(docs, window, opts);
    ranked.into_iter().take(opts.limit.max(1)).collect()
}

/// 聚合 + 排名 + **先排名后切片**的分页。
///
/// * `window` = 客户端请求的时间窗（半开区间）。它与服务端窗口求**交集**，
///   只能收窄；交集为空则返回空榜单和全零基数，而不是报错——上游在
///   `len(result) == 0` 时同样返回 0 值结果集。
/// * `page` 是 1 起的页号（对齐上游 `Pagination.PageNumber`），传 0/负数按首页处理。
/// * `page_size` 是页长，`0` 按 1 处理；[`TrendOptions::limit`] 仍是**整榜长度上限**，
///   切片发生在它之内。
///
/// 关键性质：[`TrendRanking::stats`] 是**整个交集窗口**的基数，
/// [`TrendRanking::terms`] 只是排名列表的一段。把统计放在切片之后算是错的。
pub fn rank_trends_paged(
    docs: &[TrendDoc<'_>],
    now_ms: i64,
    window: Option<crate::query::TimeRange>,
    opts: &TrendOptions,
    page: usize,
    page_size: usize,
) -> TrendRanking {
    let server = server_window(now_ms, opts);
    let effective = match window {
        Some(w) => match w.intersect(server) {
            Some(hit) => hit,
            // 交集为空：没有可算的行，但这是正常输入，不是错误。
            None => {
                return TrendRanking {
                    terms: Vec::new(),
                    stats: TrendStats::default(),
                    ranked_total: 0,
                    has_more: false,
                }
            }
        },
        None => server,
    };
    let (ranked, stats) = collect(docs, effective, opts);
    // 服务端先定榜单全长上限，分页只在里面切段：`limit` 管"这榜最多多少个词"，
    // `page_size` 管"这一页给你几个"。两者都往内收，都不影响基数。
    let mut ranked = ranked;
    ranked.truncate(opts.limit.max(1));
    let ranked_total = ranked.len();
    let page = page.max(1);
    let size = page_size.max(1);
    let start = (page - 1).saturating_mul(size);
    let terms: Vec<TrendTerm> = ranked.into_iter().skip(start).take(size).collect();
    TrendRanking {
        has_more: start + terms.len() < ranked_total,
        terms,
        stats,
        ranked_total,
    }
}

/// 服务端窗口：`[now - window_ms, now]`，用半开区间写成 `[start, now + 1)`。
/// `+1` 是为了保住"恰好在 now 这一毫秒的文档也算数"这条既有语义。
fn server_window(now_ms: i64, opts: &TrendOptions) -> crate::query::TimeRange {
    crate::query::TimeRange::between(
        now_ms.saturating_sub(opts.window_ms),
        now_ms.saturating_add(1),
    )
    .unwrap_or(crate::query::TimeRange::UNBOUNDED)
}

/// UTC 日历日键（`YYYY-MM-DD`），对齐上游 `$dateToString` 的 `%Y-%m-%d`。
/// 用地板除取日序，所以 1970 之前的时间戳也落在正确的负日序上。
pub fn utc_date_key(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(DAY_MS));
    format!("{y:04}-{m:02}-{d:02}")
}

/// 真正做聚合的地方：先按窗口逐行量，再按词/作者分桶，最后排名。
fn collect(
    docs: &[TrendDoc<'_>],
    window: crate::query::TimeRange,
    opts: &TrendOptions,
) -> (Vec<TrendTerm>, TrendStats) {
    // term -> owner -> 该作者在该词下的文档数
    let mut contributions: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut stats = TrendStats::default();
    let mut owners: BTreeSet<&str> = BTreeSet::new();
    for doc in docs {
        // 逐行量窗口（不是"整批进来就都算"）：窗外行与非 public 行一样，
        // 既不进榜单也不进基数。
        if !window.contains(doc.created_at) || doc.audience != "public" {
            continue;
        }
        stats.total_docs += 1;
        owners.insert(doc.owner);
        *stats
            .date_counts
            .entry(utc_date_key(doc.created_at))
            .or_insert(0) += 1;
        let mut doc_terms: BTreeSet<String> = BTreeSet::new();
        doc_terms.extend(terms(doc.title));
        doc_terms.extend(terms(doc.body));
        for term in doc_terms {
            *contributions
                .entry(term)
                .or_default()
                .entry(doc.owner.to_string())
                .or_insert(0) += 1;
        }
    }
    stats.distinct_owners = owners.len() as i64;
    let mut ranked: Vec<TrendTerm> = contributions
        .into_iter()
        .filter_map(|(term, owners)| {
            let distinct_authors = owners.len() as i64;
            if (distinct_authors as usize) < opts.min_authors.max(1) {
                return None;
            }
            let doc_count: i64 = owners
                .into_values()
                // 每个作者最多贡献 owner_cap 篇，因此这里的取值天然有界
                .map(|n| n.min(opts.owner_cap) as i64)
                .sum();
            Some(TrendTerm {
                term,
                score: distinct_authors * 100 + doc_count,
                doc_count,
                distinct_authors,
            })
        })
        .collect();
    ranked.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.term.cmp(&b.term)));
    (ranked, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 显式种子：每条都写死 owner/时间，测试不依赖任何"当前时间以外的世界"。
    const T0: i64 = 1_700_000_000_000;

    fn doc<'a>(owner: &'a str, body: &'a str, created_at: i64) -> TrendDoc<'a> {
        TrendDoc {
            owner,
            audience: "public",
            title: "",
            body,
            created_at,
        }
    }

    #[test]
    fn window_excludes_stale_posts() {
        let opts = TrendOptions {
            window_ms: 1_000,
            ..Default::default()
        };
        let docs = [doc("a", "rustlang", T0 - 5_000), doc("b", "rustlang", T0)];
        let ranked = rank_trends(&docs, T0, &opts);
        assert_eq!(ranked.len(), 1, "只看窗口内那篇");
        assert_eq!(ranked[0].distinct_authors, 1);
    }

    #[test]
    fn single_author_cannot_flood_the_board() {
        // 同一个作者刷 50 篇，另一个作者只写了 1 篇别的词。
        // now 取最后一篇的时刻：窗口上界是"到 now 为止"，未来行不参与（见下）。
        let mut docs: Vec<TrendDoc<'_>> =
            (0..50).map(|i| doc("spammer", "hotword", T0 + i)).collect();
        docs.push(doc("real", "quietterm", T0));
        let opts = TrendOptions {
            window_ms: 60_000,
            owner_cap: 3,
            limit: 10,
            ..Default::default()
        };
        let ranked = rank_trends(&docs, T0 + 49, &opts);
        let spam = ranked.iter().find(|t| t.term == "hotword").unwrap();
        assert_eq!(spam.doc_count, 3, "刷到 50 篇也只按 3 计");
        assert_eq!(spam.distinct_authors, 1);
        let real = ranked.iter().find(|t| t.term == "quietterm").unwrap();
        assert_eq!(real.doc_count, 1);
        // 刷量者最多多拿 owner_cap - 1 = 2 分，而分数的主体是"作者数 × 100"，
        // 所以一个作者的自嗨永远压不动两个作者的真实热度（见下个用例）。
        assert!(spam.score < real.score + 100);
        assert_eq!(ranked[0].term, "hotword", "同分带内按贡献篇数取胜");
    }

    #[test]
    fn repeat_keyword_inside_one_doc_counts_once() {
        let docs = [doc("a", "rustlang rustlang rustlang", T0)];
        let ranked = rank_trends(&docs, T0, &TrendOptions::default());
        assert_eq!(ranked[0].doc_count, 1);
    }

    #[test]
    fn non_public_never_trends() {
        let docs = [
            TrendDoc {
                owner: "a",
                audience: "friends",
                title: "",
                body: "privateterm",
                created_at: T0,
            },
            TrendDoc {
                owner: "b",
                audience: "private",
                title: "",
                body: "privateterm",
                created_at: T0,
            },
        ];
        assert!(rank_trends(&docs, T0, &TrendOptions::default()).is_empty());
    }

    #[test]
    fn more_authors_beat_more_docs() {
        let mut docs: Vec<TrendDoc<'_>> = (0..3).map(|i| doc("solo", "oneword", T0 + i)).collect();
        docs.push(doc("u1", "twoword", T0));
        docs.push(doc("u2", "twoword", T0 + 1));
        docs.push(doc("u3", "twoword", T0 + 2));
        let ranked = rank_trends(&docs, T0 + 3, &TrendOptions::default());
        assert_eq!(ranked[0].term, "twoword", "三个作者的词压过一个人刷三篇");
    }

    #[test]
    fn limit_and_min_authors_are_honoured() {
        let docs = [doc("a", "alpha", T0), doc("b", "beta", T0 + 1)];
        let ranked = rank_trends(
            &docs,
            T0 + 1,
            &TrendOptions {
                limit: 1,
                ..Default::default()
            },
        );
        assert_eq!(ranked.len(), 1);
        // 两篇同作者的文档，每个词只有 1 个作者；min_authors=2 时全被过滤
        let docs = [doc("a", "alpha", T0), doc("a", "beta", T0 + 1)];
        let ranked = rank_trends(
            &docs,
            T0 + 1,
            &TrendOptions {
                min_authors: 2,
                ..Default::default()
            },
        );
        assert!(ranked.is_empty());
    }

    /// 日锚点：2024-10-04 / 10-05 的 UTC 零点，跨天分桶要用真日子。
    const D0: i64 = 20_000 * DAY_MS;

    #[test]
    fn utc_date_key_is_the_upstream_bucket_label() {
        assert_eq!(utc_date_key(0), "1970-01-01");
        assert_eq!(utc_date_key(DAY_MS - 1), "1970-01-01");
        assert_eq!(utc_date_key(DAY_MS), "1970-01-02");
        // 1970 之前走地板除，不是截断：-1ms 还属于 1969-12-31
        assert_eq!(utc_date_key(-1), "1969-12-31");
        assert_eq!(utc_date_key(D0), "2024-10-04");
        assert_eq!(utc_date_key(D0 + DAY_MS), "2024-10-05");
    }

    #[test]
    fn ranking_pages_but_stats_stay_global() {
        // 借 OpenIM `RangeUserSendCount` 的分界：`$slice` 只切 users，
        // msg_count / user_count / dates 三个总量始终按整窗算。
        // 六个词、两个人，翻第 1 页和第 3 页时基数必须一模一样。
        let docs = [
            doc("a", "alpha", D0),
            doc("b", "beta", D0),
            doc("a", "gamma", D0 + 1),
            doc("b", "delta", D0 + 1),
            doc("a", "epsilon", D0 + 2),
            doc("b", "zeta", D0 + 2),
        ];
        let opts = TrendOptions {
            window_ms: 30 * DAY_MS,
            limit: 20,
            ..Default::default()
        };
        let now = D0 + 3;
        let first = rank_trends_paged(&docs, now, None, &opts, 1, 2);
        let second = rank_trends_paged(&docs, now, None, &opts, 2, 2);
        let last = rank_trends_paged(&docs, now, None, &opts, 3, 2);
        assert_eq!(first.terms.len(), 2);
        assert_eq!(second.terms.len(), 2);
        assert_eq!(last.terms.len(), 2);
        assert_eq!(first.stats, second.stats, "翻页不改变统计基数");
        assert_eq!(first.stats, last.stats);
        assert_eq!(first.stats.total_docs, 6);
        assert_eq!(first.stats.distinct_owners, 2);
        assert_eq!(first.ranked_total, 6);
        assert!(first.has_more && second.has_more && !last.has_more);
        // 页与页不重不漏
        let mut all: Vec<String> = first
            .terms
            .iter()
            .chain(second.terms.iter())
            .chain(last.terms.iter())
            .map(|t| t.term.clone())
            .collect();
        let before = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), before, "同一词不该出现在两页里");
    }

    #[test]
    fn date_counts_bucket_by_day_and_ignore_owner_cap() {
        // 上游 dateCount 是"这条日子了多少条消息"，没有 per-user 上限。
        // 这里刷量者贡献 5 篇同一天，owner_cap=1 只该压分数，不该压曲线。
        let mut docs: Vec<TrendDoc<'_>> = (0..5).map(|i| doc("spam", "hotword", D0 + i)).collect();
        docs.push(doc("clean", "hotword", D0 + DAY_MS));
        let opts = TrendOptions {
            window_ms: 30 * DAY_MS,
            owner_cap: 1,
            ..Default::default()
        };
        let ranking = rank_trends_paged(&docs, D0 + 2 * DAY_MS, None, &opts, 1, 10);
        assert_eq!(
            ranking.stats.date_counts.get(utc_date_key(D0).as_str()),
            Some(&5),
            "第一天 5 篇，一条不少"
        );
        assert_eq!(
            ranking.stats.date_counts.get("2024-10-05"),
            Some(&1),
            "第二天 1 篇"
        );
        assert_eq!(ranking.stats.date_counts.values().sum::<i64>(), 6);
        assert_eq!(ranking.stats.date_counts.len(), 2);
        let hot = &ranking.terms[0];
        assert_eq!(
            hot.doc_count, 2,
            "分数侧仍受 owner_cap 约束：刷量者 1 篇 + 正常 1 篇"
        );
        assert_eq!(hot.distinct_authors, 2);
    }

    #[test]
    fn client_window_can_only_narrow_the_server_window() {
        let docs = [doc("a", "alpha", D0), doc("b", "beta", D0 + DAY_MS)];
        let opts = TrendOptions {
            window_ms: 30 * DAY_MS,
            ..Default::default()
        };
        let now = D0 + 2 * DAY_MS;
        // 客户端只要第一天
        let day = crate::query::TimeRange::from_date("2024-10-04").unwrap();
        let r = rank_trends_paged(&docs, now, Some(day), &opts, 1, 10);
        assert_eq!(r.ranked_total, 1);
        assert_eq!(r.stats.total_docs, 1);
        assert_eq!(r.stats.date_counts.len(), 1);
        // 客户端要"全历史"：被服务端窗口收住，这里等价于不传
        let wide = rank_trends_paged(
            &docs,
            now,
            Some(crate::query::TimeRange::UNBOUNDED),
            &opts,
            1,
            10,
        );
        assert_eq!(wide.stats.total_docs, 2);
        // 要一整段未来（30 天窗口之外）：交集为空 => 全零，不是报错，也不是全量
        let future =
            crate::query::TimeRange::between(now + 40 * DAY_MS, now + 41 * DAY_MS).unwrap();
        let none = rank_trends_paged(&docs, now, Some(future), &opts, 1, 10);
        assert_eq!(
            none,
            TrendRanking {
                terms: Vec::new(),
                stats: TrendStats::default(),
                ranked_total: 0,
                has_more: false,
            },
            "空交集是全零结果，不是错误、也不是全量"
        );
    }

    #[test]
    fn future_dated_docs_do_not_enter_the_board() {
        // 时钟超前的行：半开上界把它挡在外面。旧实现只有下界，这种行会常驻榜单。
        let docs = [doc("a", "ghostword", D0 + 5), doc("b", "realword", D0)];
        let opts = TrendOptions {
            window_ms: DAY_MS,
            ..Default::default()
        };
        let ranked = rank_trends(&docs, D0, &opts);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].term, "realword");
    }

    #[test]
    fn page_number_zero_is_clamped_not_wrapped() {
        // 上游把 pageNumber 直接写进 $slice 的偏移，传 0 会得到 -1 的语义。
        // 这里钳到首页：分页参数手滑不该变成"从尾巴往前数"。
        let docs = [doc("a", "alpha", D0), doc("b", "beta", D0)];
        let opts = TrendOptions::default();
        let zero = rank_trends_paged(&docs, D0, None, &opts, 0, 1);
        let one = rank_trends_paged(&docs, D0, None, &opts, 1, 1);
        assert_eq!(zero.terms, one.terms);
        let beyond = rank_trends_paged(&docs, D0, None, &opts, 99, 1);
        assert!(beyond.terms.is_empty());
        assert!(!beyond.has_more);
        assert_eq!(beyond.ranked_total, 2, "翻过头也只是空页，基数照旧");
    }
}
