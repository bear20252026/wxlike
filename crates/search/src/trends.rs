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

use std::collections::{BTreeMap, BTreeSet};

use crate::query::terms;

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

/// 聚合 + 排名。排序键：`score DESC, term ASC`——同分按词条字典序，
/// 结果与 map 迭代顺序无关，因此跨进程/跨重启都稳定。
pub fn rank_trends(docs: &[TrendDoc<'_>], now_ms: i64, opts: &TrendOptions) -> Vec<TrendTerm> {
    let cutoff = now_ms.saturating_sub(opts.window_ms);
    // term -> owner -> 该作者在该词下的文档数
    let mut contributions: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for doc in docs {
        if doc.created_at < cutoff || doc.audience != "public" {
            continue;
        }
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
    ranked.truncate(opts.limit.max(1));
    ranked
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
        // 同一个作者刷 50 篇，另一个作者只写了 1 篇别的词
        let mut docs: Vec<TrendDoc<'_>> =
            (0..50).map(|i| doc("spammer", "hotword", T0 + i)).collect();
        docs.push(doc("real", "quietterm", T0));
        let opts = TrendOptions {
            window_ms: 60_000,
            owner_cap: 3,
            limit: 10,
            ..Default::default()
        };
        let ranked = rank_trends(&docs, T0, &opts);
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
        let docs = [doc("a", "alpha", T0), doc("b", "beta", T0)];
        let ranked = rank_trends(
            &docs,
            T0,
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
            T0,
            &TrendOptions {
                min_authors: 2,
                ..Default::default()
            },
        );
        assert!(ranked.is_empty());
    }
}
