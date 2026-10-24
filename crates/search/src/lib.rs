//! `wxlike-search` — P3 search-service 索引层（ADR-22，borrow-first：
//! 嵌入式全文检索用 **Tantivy**，自 2026-10-17 起替换 P1 的子串过渡实现）。
//!
//! 索引只存 `(conv_id, seq, sender, text, sent_at)`——**信箱的投影，不是真相**。
//! 可见性裁决永远在权威存储：命中结果回 Store 验证撤回状态与授权会话集
//! （`authorize` 由调用方执行）。索引可随时弃可重建（append listener 重放）。
//!
//! 分词：ngram(1..=10) 单字段——CJK 无词边界，ngram 让"慢速"命中"慢速模式"，
//! 同时让英文 "pineapple" 命中 "pineapples"（≤10 字符内前缀/子串皆可）。
//! 查询侧把用户输入切成字母数字/CJK token 后 AND 起来，Tantivy 查询语法
//! （AND/OR/field:）不会泄漏给终端用户。

use std::path::Path;

use anyhow::Context as _;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{IndexRecordOption, Schema, Value as _, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, Term};

/// 命中：调用方拿它回权威存储取正文并做撤回/授权裁决。
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub conv_id: String,
    pub seq: i64,
    pub sender: String,
    pub score: f32,
}

pub struct SearchIndex {
    reader: IndexReader,
    /// `None` = 只读副本（查询端）。同一目录的全局只允许一个 writer
    /// （tantivy 目录锁），所以写端在后台索引线程手里。
    writer: Option<IndexWriter>,
    f_conv: tantivy::schema::Field,
    f_seq: tantivy::schema::Field,
    f_sender: tantivy::schema::Field,
    f_text: tantivy::schema::Field,
}

/// ngram 分词器名（1..=10）。
const NGRAM: &str = "ngram10";
/// 查询 token 上限：超长输入截断，防查询风暴。
const MAX_TOKENS: usize = 16;

/// 打开目录索引；不存在时按本 crate 的 schema 新建（索引可弃可重建）。
fn open_dir_index(dir: &Path) -> anyhow::Result<Index> {
    std::fs::create_dir_all(dir)?;
    if dir.join("meta.json").exists() {
        Index::open_in_dir(dir).context("open tantivy index")
    } else {
        let (schema, ..) = build_schema();
        Index::create_in_dir(dir, schema).context("create tantivy index")
    }
}

fn build_schema() -> (
    Schema,
    tantivy::schema::Field,
    tantivy::schema::Field,
    tantivy::schema::Field,
    tantivy::schema::Field,
) {
    let mut builder = Schema::builder();
    let f_conv = builder.add_text_field("conv_id", STRING | STORED);
    let f_seq = builder.add_i64_field("seq", STORED);
    let f_sender = builder.add_text_field("sender", STRING | STORED);
    // ngram 索引 + 原文存储（摘要由调用方截取）
    let f_text = builder.add_text_field(
        "text",
        TEXT.set_indexing_options(
            tantivy::schema::TextFieldIndexing::default().set_tokenizer(NGRAM),
        )
        .set_stored(),
    );
    (builder.build(), f_conv, f_seq, f_sender, f_text)
}

impl SearchIndex {
    /// 内存索引（测试/演示）：读写一体。
    pub fn in_memory() -> anyhow::Result<Self> {
        let (schema, ..) = build_schema();
        Self::assemble(Index::create_from_tempdir(schema)?, true)
    }

    /// 查询端：只读副本（无 writer，绕开目录写锁）。检索前 Manual reload，
    /// 后台索引线程一旦提交即可见。
    pub fn open_query(dir: &Path) -> anyhow::Result<Self> {
        let index = open_dir_index(dir)?;
        Self::assemble(index, false)
    }

    /// 写端：后台索引线程独占（同一目录只能有一个实例持有 writer）。
    pub fn open_writer(dir: &Path) -> anyhow::Result<Self> {
        let index = open_dir_index(dir)?;
        Self::assemble(index, true)
    }

    fn assemble(index: Index, with_writer: bool) -> anyhow::Result<Self> {
        // ngram tokenizer: char-level 1..=10；重开索引后必须重新注册
        index.tokenizers().register(
            NGRAM,
            tantivy::tokenizer::NgramTokenizer::new(1, 10, false)?,
        );
        let schema = index.schema();
        let f_conv = schema.get_field("conv_id").context("conv_id field")?;
        let f_seq = schema.get_field("seq").context("seq field")?;
        let f_sender = schema.get_field("sender").context("sender field")?;
        let f_text = schema.get_field("text").context("text field")?;
        let writer = if with_writer {
            Some(index.writer(15_000_000)?)
        } else {
            None
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            reader,
            writer,
            f_conv,
            f_seq,
            f_sender,
            f_text,
        })
    }

    /// 一条消息进索引并提交（测试/演示用；服务端走 [`IndexerHandle`] 攒批）。
    pub fn index(
        &mut self,
        conv_id: &str,
        seq: i64,
        sender: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.stage(conv_id, seq, sender, text)?;
        self.commit()
    }

    /// 只写入 writer 缓冲，不提交（批量索引线程用）。
    pub fn stage(
        &mut self,
        conv_id: &str,
        seq: i64,
        sender: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let writer = self.writer.as_mut().context("read-only search index")?;
        writer.add_document(doc!(
            self.f_conv => conv_id,
            self.f_seq => seq,
            self.f_sender => sender,
            self.f_text => text,
        ))?;
        Ok(())
    }

    /// 提交本批并刷新 reader（之后检索可见）。
    pub fn commit(&mut self) -> anyhow::Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            writer.commit()?;
        }
        self.reader.reload()?;
        Ok(())
    }

    /// AND 语义检索，限定授权会话集。空授权集 → 空结果（防越权读索引）。
    pub fn search(
        &self,
        query: &str,
        conv_filter: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<SearchHit>> {
        if conv_filter.is_empty() {
            return Ok(Vec::new());
        }
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let mut musts: Vec<(Occur, Box<dyn Query>)> = tokens
            .iter()
            .map(|t| {
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.f_text, t),
                        IndexRecordOption::WithFreqs,
                    )) as Box<dyn Query>,
                )
            })
            .collect();
        // conv 过滤：Should 组 + 外层 Must（命中集合必须落在授权会话内）
        let conv_terms: Vec<(Occur, Box<dyn Query>)> = conv_filter
            .iter()
            .map(|c| {
                (
                    Occur::Should,
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.f_conv, c),
                        IndexRecordOption::Basic,
                    )) as Box<dyn Query>,
                )
            })
            .collect();
        musts.push((Occur::Must, Box::new(BooleanQuery::new(conv_terms))));

        // 查询前 reload：后台索引线程的最新提交此刻可见（无新提交时近零开销）
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        // tantivy 0.27：TopDocs 结构体本身不再实现 Collector——
        // 构建器链上 order_by_score() 返回匿名 impl Collector（fruit 形状不变）
        let top = searcher.search(
            &BooleanQuery::new(musts),
            &TopDocs::with_limit(limit.clamp(1, 100)).order_by_score(),
        )?;

        let mut hits = Vec::new();
        for (score, addr) in top {
            let hit_doc: tantivy::TantivyDocument = searcher.doc(addr)?;
            let conv = hit_doc
                .get_first(self.f_conv)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let seq = hit_doc
                .get_first(self.f_seq)
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let sender = hit_doc
                .get_first(self.f_sender)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            hits.push(SearchHit {
                conv_id: conv,
                seq,
                sender,
                score,
            });
        }
        Ok(hits)
    }
}

/// 查询侧切词：字母数字 + CJK 连续段为一个 token（ngram 分词器在索引侧再切）。
/// 这层同时是 Tantivy 查询语法防火墙：`AND`/`field:`/引号不进解析器。
fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in input.chars() {
        if ch.is_alphanumeric() {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.truncate(MAX_TOKENS);
    out
}

// ---------------------------------------------------------------------------
// 后台批量索引线程：把 commit/fsync 移出消息网关的关键路径（ADR-22：
// 索引经事件订阅更新，绝不阻塞投递）。
// ---------------------------------------------------------------------------

/// 一条待索引消息。
pub struct IndexJob {
    pub conv_id: String,
    pub seq: i64,
    pub sender: String,
    pub text: String,
}

/// 后台索引线程的提交端：`submit` 永不阻塞（通道有界满时丢弃并记日志——
/// 索引可重建，丢批不是丢消息）。
pub struct IndexerHandle {
    tx: std::sync::mpsc::Sender<IndexJob>,
}

impl IndexerHandle {
    /// 拉起后台线程：攒批（最多 128 条或通道暂空）→ 单次 commit。
    pub fn spawn(mut index: SearchIndex) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<IndexJob>();
        std::thread::spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut batch = vec![first];
                while batch.len() < 128 {
                    let Ok(more) = rx.try_recv() else { break };
                    batch.push(more);
                }
                for job in &batch {
                    if let Err(err) = index.stage(&job.conv_id, job.seq, &job.sender, &job.text) {
                        eprintln!("[search] stage failed: {err}");
                    }
                }
                if let Err(err) = index.commit() {
                    eprintln!("[search] commit failed ({} docs): {err}", batch.len());
                }
            }
        });
        Self { tx }
    }

    /// 提交一条；通道满（极端背压）时静默丢弃——索引可重建，消息本体在信箱。
    pub fn submit(&self, conv_id: &str, seq: i64, sender: &str, text: String) {
        let _ = self.tx.send(IndexJob {
            conv_id: conv_id.to_string(),
            seq,
            sender: sender.to_string(),
            text,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
    fn fill(idx: &mut SearchIndex) {
        idx.index("d-a~b", 1, "alice", "pineapple express").unwrap();
        idx.index("d-a~b", 2, "bob", "no pineapples here").unwrap();
        idx.index("d-a~c", 3, "alice", "hello carol").unwrap();
        idx.index("d-a~b", 4, "alice", "慢速模式 slow mode")
            .unwrap();
    }

    #[test]
    fn word_query_hits_stemmed_and_prefixed_forms() {
        let mut idx = SearchIndex::in_memory().unwrap();
        fill(&mut idx);
        let hits = idx.search("pineapple", &["d-a~b".to_string()], 10).unwrap();
        assert_eq!(hits.len(), 2, "pineapple + pineapples via ngram: {hits:?}");
    }

    #[test]
    fn cjk_substring_matches_without_word_boundaries() {
        let mut idx = SearchIndex::in_memory().unwrap();
        fill(&mut idx);
        let hits = idx.search("慢速", &["d-a~b".to_string()], 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 4);
    }

    #[test]
    fn conv_filter_is_a_hard_boundary() {
        let mut idx = SearchIndex::in_memory().unwrap();
        fill(&mut idx);
        // carol 的会话看不到 pineapple（哪怕索引里有）
        let hits = idx.search("pineapple", &["d-a~c".to_string()], 10).unwrap();
        assert!(hits.is_empty());
        // 空授权集直接短路
        assert!(idx.search("pineapple", &[], 10).unwrap().is_empty());
        assert!(idx
            .search("", &["d-a~b".to_string()], 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn multi_token_query_is_and_semantics() {
        let mut idx = SearchIndex::in_memory().unwrap();
        fill(&mut idx);
        idx.index("d-a~b", 5, "a", "100% match_").unwrap();
        idx.index("d-a~b", 6, "a", "1x0y0z% match_").unwrap();
        // tokenize("100% match_") = ["100","match"] AND → 只中第一条
        let hits = idx
            .search("100% match_", &["d-a~b".to_string()], 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 5);
    }

    #[test]
    fn index_survives_reopen_from_disk() {
        let dir =
            std::env::temp_dir().join(format!("wxlike-search-{}-{}", std::process::id(), now()));
        {
            let mut idx = SearchIndex::open_writer(&dir).unwrap();
            idx.index("d-a~b", 1, "alice", "persist this").unwrap();
        }
        let idx = SearchIndex::open_query(&dir).unwrap();
        let hits = idx.search("persist", &["d-a~b".to_string()], 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_syntax_is_never_interpreted() {
        let mut idx = SearchIndex::in_memory().unwrap();
        idx.index("d-a~b", 1, "a", "hello AND world").unwrap();
        idx.index("d-a~b", 2, "a", "plain text").unwrap();
        // 输入里的 AND 是字面 token，不是布尔算子
        let hits = idx.search("AND", &["d-a~b".to_string()], 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 1);
        // field: 语法同样失效
        let hits = idx
            .search("sender:alice", &["d-a~b".to_string()], 10)
            .unwrap();
        assert!(hits.is_empty(), "no such token in any body");
    }
}
