//! 统一搜索编排层（issue #135 的"能被 G02/G05 复用"落点）。
//!
//! 一个 [`SearchService`]，多个 [`Provider`]：G02 把"消息"接进来，G05 把
//! 朋友圈/文章/视频号/状态接进来。编排层只认识三件事——候选、授权、投影，
//! 因此新增一个内容域**不需要**改分页/摘要/错误处理代码。
//!
//! 三条不变式（这是这个文件存在的全部理由）：
//!
//! 1. **先授权、后取正文。** [`Provider::candidates`] 只准返回 id + 排序键；
//!    正文只能在 [`Provider::authorize`] 返回 `true` 之后由
//!    [`Provider::project`] 取。索引里的死行/撤权行不会以任何形式出现。
//! 2. **分页不许漏。** 每个 provider 独立翻页，游标推进看**候选行**，
//!    不是看授权通过的行。老实现按 `limit + 1` 取一批候选、过滤掉不可见的
//!    就宣布 `has_more = false`，于是"最新两条好友可见 + 其后一条公开"这种
//!    数据形态会让后面的公开内容永远检索不到。这里按 k 路归并取满为止。
//! 3. **全局游标稳定。** 排序键 `(created_at DESC, doc_id DESC)` 与服务端
//!    时钟同源，翻页期间新内容不会把旧内容顶出结果集。
//!
//! 排序键相同的跨域碰撞（两个域同一毫秒且 id 完全相同）在本项目里概率为零
//! （id 由服务端 `random_hex(8)` 生成），故游标不带 kind 段；如将来接入
//! 外部 id 空间，需要把 kind 加进游标格式。

use std::collections::VecDeque;
use std::sync::Arc;

use crate::query::{parse_cursor, NormalizedQuery, QueryError};

/// 一条候选：只有 id 和排序键。**不得**携带正文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub doc_id: String,
    pub created_at: i64,
}

/// 一条对外结果。`snippet` 必须是 provider 截好的有界窗口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub doc_id: String,
    pub kind: String,
    pub title: String,
    pub snippet: String,
    pub created_at: i64,
}

/// provider 侧的错误：调用方（平台层）负责翻译成 API 错误，
/// 编排层不猜 HTTP 语义。
pub type ProviderResult<T> = anyhow::Result<T>;

/// 一个可检索的域。
pub trait Provider: Send + Sync {
    /// 稳定小写名，同时是 `scope` 参数取值。
    fn kind(&self) -> &str;

    /// 取 `(before_at, before_id)` **之前**的候选，最多 `limit` 条，
    /// 按排序键从新到旧。不判权限——那是 [`Self::authorize`] 的事。
    fn candidates(
        &self,
        q: &NormalizedQuery,
        before_at: i64,
        before_id: &str,
        limit: usize,
    ) -> ProviderResult<Vec<Candidate>>;

    /// 权威表裁决：这条对 `viewer` 可见吗？
    fn authorize(&self, viewer: &str, doc_id: &str) -> ProviderResult<bool>;

    /// 取正文 + 出摘要。`None` = 索引里剩个死行，直接丢弃。
    /// 实现必须自己再挡一次未授权访问（纵深防御），不能假定
    /// 调用方一定先问过 [`Self::authorize`]。
    fn project(&self, doc_id: &str, q: &NormalizedQuery) -> ProviderResult<Option<Item>>;
}

#[derive(Debug)]
pub enum SearchError {
    Query(QueryError),
    UnknownScope(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::Query(err) => f.write_str(err.as_str()),
            SearchError::UnknownScope(kind) => write!(f, "unknown search scope: {kind}"),
            SearchError::Internal(err) => write!(f, "search failed: {err:#}"),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<QueryError> for SearchError {
    fn from(err: QueryError) -> Self {
        SearchError::Query(err)
    }
}

impl From<anyhow::Error> for SearchError {
    fn from(err: anyhow::Error) -> Self {
        SearchError::Internal(err)
    }
}

/// 一页结果。`next_cursor` 为空串表示没有后续（与平台既有契约一致）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub items: Vec<Item>,
    pub next_cursor: String,
    pub has_more: bool,
}

/// 一个域在归并中的状态：自己的游标 + 已授权缓冲。
struct Lane {
    provider: Arc<dyn Provider>,
    at: i64,
    id: String,
    buf: VecDeque<Item>,
    /// 该域索引已翻到底（最后一批不足一页）。
    done: bool,
}

/// 多域检索编排器：注册一批 [`Provider`]，对外只暴露一个 [`Self::search`]。
pub struct SearchService {
    providers: Vec<Arc<dyn Provider>>,
}

/// 单页候选批量下限：至少取这么多，免得每翻一步都打一次库。
const MIN_BATCH: usize = 32;
/// 单页结果上限（与平台侧 `MAX_SEARCH_PAGE` 同量级，编排层再兜一道）。
const MAX_LIMIT: usize = 100;

impl SearchService {
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        Self { providers }
    }

    /// 已注册的 scope 名（给自省/文档用）。
    pub fn scopes(&self) -> Vec<String> {
        self.providers
            .iter()
            .map(|p| p.kind().to_string())
            .collect()
    }

    /// `scopes` 为空 = 全部已注册域；不认识的 scope 直接报错，
    /// **不静默退化成全量**（那会把"只能搜公开内容"的调用变成越权探测面）。
    pub fn search(
        &self,
        viewer: &str,
        input: &str,
        scopes: &[String],
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page, SearchError> {
        let q = NormalizedQuery::parse(input)?;
        let selected = self.select(scopes)?;
        if selected.is_empty() {
            return Ok(Page {
                items: Vec::new(),
                next_cursor: String::new(),
                has_more: false,
            });
        }
        let limit = limit.clamp(1, MAX_LIMIT);
        let batch = (limit + 1).max(MIN_BATCH);
        let (before_at, before_id) = parse_cursor(cursor, false)?;

        let mut lanes: Vec<Lane> = selected
            .into_iter()
            .map(|provider| Lane {
                provider,
                at: before_at,
                id: before_id.clone(),
                buf: VecDeque::new(),
                done: false,
            })
            .collect();

        let mut items: Vec<Item> = Vec::with_capacity(limit.min(16));
        let mut has_more = false;
        loop {
            if items.len() == limit {
                // 取满即停：还有缓冲或未翻完的域 => 有下一页
                has_more = lanes.iter().any(|lane| !lane.buf.is_empty() || !lane.done);
                break;
            }
            for lane in lanes.iter_mut() {
                while lane.buf.is_empty() && !lane.done {
                    lane.fill(&q, viewer, batch)?;
                }
            }
            let Some(winner) = head_winner(&lanes) else {
                break;
            };
            let Some(head) = lanes[winner].buf.pop_front() else {
                break;
            };
            items.push(head);
        }
        let next_cursor = items
            .last()
            .map(|last| format!("{}:{}", last.created_at, last.doc_id))
            .unwrap_or_default();
        Ok(Page {
            items,
            next_cursor,
            has_more,
        })
    }

    fn select(&self, scopes: &[String]) -> Result<Vec<Arc<dyn Provider>>, SearchError> {
        if scopes.is_empty() {
            return Ok(self.providers.clone());
        }
        let mut out: Vec<Arc<dyn Provider>> = Vec::with_capacity(scopes.len());
        for want in scopes {
            let trimmed = want.trim().to_lowercase();
            if trimmed.is_empty() {
                continue;
            }
            // 同名多注册（配置错误）时**全部保留**：只取第一个会让后面的
            // provider 被静默屏蔽，那正好是一个"看不到某类内容"的越权遮挡。
            let matched: Vec<Arc<dyn Provider>> = self
                .providers
                .iter()
                .filter(|p| p.kind() == trimmed.as_str())
                .cloned()
                .collect();
            if matched.is_empty() {
                return Err(SearchError::UnknownScope(trimmed.clone()));
            }
            for provider in matched {
                if !out.iter().any(|held| Arc::ptr_eq(held, &provider)) {
                    out.push(provider);
                }
            }
        }
        Ok(out)
    }
}

/// 归并冠军：队头排序键最大的 lane 下标。
fn head_winner(lanes: &[Lane]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (idx, lane) in lanes.iter().enumerate() {
        let Some(head) = lane.buf.front() else {
            continue;
        };
        let better = match best.and_then(|b| lanes[b].buf.front()) {
            None => true,
            Some(cur) => {
                (head.created_at, head.doc_id.as_str()) > (cur.created_at, cur.doc_id.as_str())
            }
        };
        if better {
            best = Some(idx);
        }
    }
    best
}

impl Lane {
    fn fill(&mut self, q: &NormalizedQuery, viewer: &str, batch: usize) -> Result<(), SearchError> {
        let page = self.provider.candidates(q, self.at, &self.id, batch)?;
        if let Some(last) = page.last() {
            // 游标跟着**候选**走，不是跟着可见行走：否则一整批不可见的候选
            // 会被反复重取（死循环），或被当成"到底了"（漏数据）。
            self.at = last.created_at;
            self.id = last.doc_id.clone();
        }
        if page.len() < batch {
            self.done = true;
        }
        for candidate in page {
            if !self.provider.authorize(viewer, &candidate.doc_id)? {
                continue;
            }
            if let Some(item) = self.provider.project(&candidate.doc_id, q)? {
                self.buf.push_back(item);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// 编排层的单元测试：这里的 provider 是脚本化的**排序夹具**，只用来钉死
    /// 归并/游标算法，不冒充任何业务实现。真实域 provider 的验收在
    /// `crates/platform/tests/g05_content.rs` 走 SQLite + HTTP。
    struct Fake {
        kind: &'static str,
        /// `(created_at, doc_id, visible)`
        rows: Vec<(i64, String, bool)>,
        fetches: AtomicUsize,
    }

    impl Fake {
        fn new(kind: &'static str, rows: &[(i64, &str, bool)]) -> Self {
            Self {
                kind,
                rows: rows
                    .iter()
                    .map(|(at, id, v)| (*at, (*id).to_string(), *v))
                    .collect(),
                fetches: AtomicUsize::new(0),
            }
        }
    }

    impl Provider for Fake {
        fn kind(&self) -> &str {
            self.kind
        }
        fn candidates(
            &self,
            q: &NormalizedQuery,
            before_at: i64,
            before_id: &str,
            limit: usize,
        ) -> ProviderResult<Vec<Candidate>> {
            self.fetches.fetch_add(1, AtomicOrdering::SeqCst);
            let needle = q.text().to_string();
            let mut matched: Vec<&(i64, String, bool)> = self
                .rows
                .iter()
                .filter(|(_, id, _)| id.contains(&needle))
                .filter(|(at, id, _)| {
                    *at < before_at || (*at == before_at && id.as_str() < before_id)
                })
                .collect();
            matched.sort_by(|a, b| (b.0, b.1.as_str()).cmp(&(a.0, a.1.as_str())));
            Ok(matched
                .into_iter()
                .take(limit)
                .map(|(at, id, _)| Candidate {
                    doc_id: id.clone(),
                    created_at: *at,
                })
                .collect())
        }
        fn authorize(&self, _viewer: &str, doc_id: &str) -> ProviderResult<bool> {
            Ok(self
                .rows
                .iter()
                .find(|(_, id, _)| id == doc_id)
                .is_some_and(|(_, _, visible)| *visible))
        }
        fn project(&self, doc_id: &str, q: &NormalizedQuery) -> ProviderResult<Option<Item>> {
            let Some((at, _, visible)) = self.rows.iter().find(|(_, id, _)| id == doc_id) else {
                return Ok(None);
            };
            if !visible {
                // provider 自己也挡一次：不能只靠调用方守规矩
                return Err(anyhow::anyhow!("projection without authorization"));
            }
            Ok(Some(Item {
                doc_id: doc_id.to_string(),
                kind: self.kind.to_string(),
                title: format!("{} title", self.kind),
                snippet: q.snippet(&format!("body of {doc_id}"), 40),
                created_at: *at,
            }))
        }
    }

    /// 时刻意排布：最新的两条朋友圈**不可见**，其后才是公开内容。
    fn service() -> SearchService {
        SearchService::new(vec![
            Arc::new(Fake::new(
                "moment",
                &[
                    (50, "m0001", false),
                    (40, "m0002", false),
                    (30, "m0030", true),
                    (20, "m0020", false),
                    (10, "m0010", true),
                ],
            )),
            Arc::new(Fake::new(
                "article",
                &[(45, "a0045", true), (15, "a0015", false)],
            )),
        ])
    }

    fn ids(page: &Page) -> Vec<String> {
        page.items.iter().map(|i| i.doc_id.clone()).collect()
    }

    #[test]
    fn invisible_candidates_do_not_truncate_the_page() {
        // 回归用例：老实现 kind=moment、limit=2 只取 3 条候选（50/40/30），
        // 过滤掉两条不可见后回 1 条并宣布 has_more=false —— m0010 永远搜不到。
        let svc = service();
        let page = svc
            .search("u", "m0", &["moment".to_string()], None, 2)
            .unwrap();
        assert_eq!(ids(&page), ["m0030", "m0010"], "不可见候选不占结果位");
        assert!(!page.has_more);
    }

    #[test]
    fn cross_scope_merge_is_global_order() {
        let page = service().search("u", "00", &[], None, 10).unwrap();
        assert_eq!(ids(&page), ["a0045", "m0030", "m0010"]);
        assert_eq!(page.next_cursor, "10:m0010");
    }

    #[test]
    fn paging_covers_every_visible_row_exactly_once() {
        let svc = service();
        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..12 {
            let page = svc.search("u", "00", &[], cursor.as_deref(), 1).unwrap();
            if page.items.is_empty() {
                break;
            }
            seen.push(page.items[0].doc_id.clone());
            if !page.has_more {
                break;
            }
            cursor = Some(page.next_cursor.clone());
        }
        assert_eq!(seen, ["a0045", "m0030", "m0010"]);
    }

    #[test]
    fn cursor_page_two_continues_where_page_one_stopped() {
        let svc = service();
        let first = svc.search("u", "00", &[], None, 2).unwrap();
        assert_eq!(ids(&first), ["a0045", "m0030"]);
        assert!(first.has_more);
        let second = svc
            .search("u", "00", &[], Some(&first.next_cursor), 2)
            .unwrap();
        assert_eq!(ids(&second), ["m0010"]);
        assert!(!second.has_more);
    }

    #[test]
    fn empty_authorization_returns_empty_not_an_error() {
        let svc = SearchService::new(vec![Arc::new(Fake::new("moment", &[(1, "m1", false)]))]);
        let page = svc.search("u", "m1", &[], None, 10).unwrap();
        assert!(page.items.is_empty());
        assert!(!page.has_more);
        assert_eq!(page.next_cursor, "");
    }

    #[test]
    fn unknown_scope_is_refused_instead_of_widening() {
        let err = service().search("u", "a0", &["wallet".to_string()], None, 10);
        assert!(
            matches!(&err, Err(SearchError::UnknownScope(kind)) if kind == "wallet"),
            "{err:?}"
        );
    }

    #[test]
    fn query_validation_is_the_same_gate_for_every_domain() {
        assert!(matches!(
            service().search("u", "   ", &[], None, 10),
            Err(SearchError::Query(QueryError::Empty))
        ));
    }

    #[test]
    fn repeated_paging_does_not_rescan_the_whole_index() {
        // 归并应当只按域打有限几次候选；游标推进后不再回头
        let arc = Arc::new(Fake::new(
            "moment",
            &[
                (9, "m9", false),
                (8, "m8", false),
                (7, "m7", true),
                (6, "m6", true),
            ],
        ));
        let svc = SearchService::new(vec![Arc::clone(&arc) as Arc<dyn Provider>]);
        let page = svc.search("u", "m", &[], None, 1).unwrap();
        assert_eq!(ids(&page), ["m7"]);
        assert_eq!(
            arc.fetches.load(AtomicOrdering::SeqCst),
            1,
            "一批候选就够填满 limit=1，不该重复扫"
        );
    }
}
