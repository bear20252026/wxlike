//! `wxlike-moderation` — P4 内容审核能力层（ADR-20 第二层：moderation-service）。
//!
//! 定位：**只回答"这条内容是什么"，不决定"这次请求放不放行"**。调用方是
//! 发送管线（先审后发/先发后审由 [`Mode`] 配置）与举报工单（举报复核）。
//! 风控在 `wxlike_risk`，两者互不调用——这是 ADR-20 的硬边界。
//!
//! # 为什么不是"包含敏感词就封"
//!
//! 真实绕过几乎不靠新词，靠**变形**：`敏 感 词`、`敏\u{200b}感词`、
//! `５２０`（全角）、`p0rn`（数字替换）。所以匹配前先做归一化：
//!
//! 1. 全角 → 半角（含全角数字/字母）；
//! 2. 删除零宽字符与格式控制符（U+200B–U+200F、U+2060、U+FEFF）；
//! 3. 小写化；
//! 4. 常见 leet 替换（0→o、1→i、3→e、4→a、5→s、7→t、@→a、$→s）；
//! 5. 只保留字母、数字、CJK——空格与标点被抹掉，`敏-感-词` 归一后连成词。
//!
//! 归一化是**双向**的：词表也走同一条管线，否则规则本身会被变形绕过。
//!
//! # 分级
//!
//! * [`Severity::Block`] — 硬违规（色情/暴恐/诈骗），任何模式下都拒绝；
//! * [`Severity::Review`] — 软违规（辱骂/广告），先审后发时挂起、先发后审时
//!   发布并打标；
//! * 无命中 → [`Outcome::Publish`]。
//!
//! 多模态由 [`MediaJudge`] 注入：本 crate 不做图像/音频模型推理，只定义
//! 调用契约与合并规则（ADR-15：Lexfence 式独立微服务，REST 调用）。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// 违规分级。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// 软违规：挂起/打标，人工复核。
    Review,
    /// 硬违规：直接拒绝。
    Block,
}

/// 审核模式（合同 §3.14「先审后发/先发后审配置」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// 先审后发：软违规挂起，人工放行后才投递。
    PreReview,
    /// 先发后审：软违规照常投递但打标，进人工队列。
    PostReview,
}

/// 处置结论（发送管线据此决定投递/挂起/拒绝）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// 正常投递。
    Publish,
    /// 挂起等人工审核（先审后发）。
    Hold,
    /// 投递但打标（先发后审）。
    PublishFlagged,
    /// 拒绝投递。
    Reject,
}

/// 一次判定的完整结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub outcome: Outcome,
    /// 命中的标签，稳定排序（同一输入永远同一输出，便于对账与测试）。
    pub labels: Vec<String>,
    /// 命中的最高分级；无命中为 `None`。
    pub severity: Option<Severity>,
}

impl Decision {
    fn clean() -> Self {
        Self {
            outcome: Outcome::Publish,
            labels: Vec::new(),
            severity: None,
        }
    }

    pub fn is_publishable(&self) -> bool {
        matches!(self.outcome, Outcome::Publish | Outcome::PublishFlagged)
    }
}

/// 多模态判定接口（图像/音频/视频由外部微服务实现）。
pub trait MediaJudge: Send + Sync + 'static {
    fn judge(&self, mime: &str, bytes: &[u8]) -> MediaVerdict;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaVerdict {
    Clean,
    Review,
    Block,
}

/// 词条：`(词, 标签, 分级)`。
#[derive(Clone, Debug)]
pub struct Rule {
    pub term: String,
    pub label: String,
    pub severity: Severity,
}

impl Rule {
    pub fn new(term: &str, label: &str, severity: Severity) -> Self {
        Self {
            term: normalize(term),
            label: label.to_string(),
            severity,
        }
    }
}

/// 字符 trie（词表规模是几十到几万条，逐条 `contains` 是 O(n·m)；
/// trie 是 O(m)，且不需要引入 Aho-Corasick 依赖）。
#[derive(Default)]
struct Node {
    children: HashMap<char, usize>,
    /// 该节点结束的词：`(标签, 分级)`。
    ends: Vec<(String, Severity)>,
}

pub struct Moderator {
    nodes: Vec<Node>,
    rules: Vec<Rule>,
}

impl Moderator {
    pub fn new(rules: Vec<Rule>) -> Self {
        let mut nodes = vec![Node::default()];
        for rule in &rules {
            let mut cur = 0usize;
            for ch in rule.term.chars() {
                let next = match nodes[cur].children.get(&ch) {
                    Some(&idx) => idx,
                    None => {
                        nodes.push(Node::default());
                        let idx = nodes.len() - 1;
                        nodes[cur].children.insert(ch, idx);
                        idx
                    }
                };
                cur = next;
            }
            nodes[cur].ends.push((rule.label.clone(), rule.severity));
        }
        Self { nodes, rules }
    }

    /// 内置演示词表。真实部署由运营导入（本 crate 不内嵌任何第三方词库）。
    pub fn with_builtin_rules() -> Self {
        use Severity::{Block, Review};
        let raw: &[(&str, &str, Severity)] = &[
            ("色情", "porn", Block),
            ("裸聊", "porn", Block),
            ("porn", "porn", Block),
            ("暴恐", "violence", Block),
            ("恐怖袭击", "violence", Block),
            ("诈骗", "fraud", Block),
            ("洗钱", "fraud", Block),
            ("博彩", "fraud", Block),
            ("加我微信", "ad", Review),
            ("加v", "ad", Review),
            ("刷单", "ad", Review),
            ("代开发票", "ad", Review),
            ("傻逼", "abuse", Review),
            ("fuck", "abuse", Review),
        ];
        Self::new(raw.iter().map(|(t, l, s)| Rule::new(t, l, *s)).collect())
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// 文本判定（归一化 + trie 扫描）。
    pub fn scan(&self, text: &str) -> Decision {
        let hay = normalize(text);
        let chars: Vec<char> = hay.chars().collect();
        let mut hits: Vec<(String, Severity)> = Vec::new();
        for start in 0..chars.len() {
            let mut cur = 0usize;
            for ch in chars.iter().skip(start) {
                let Some(&next) = self.nodes[cur].children.get(ch) else {
                    break;
                };
                cur = next;
                for (label, severity) in &self.nodes[cur].ends {
                    hits.push((label.clone(), *severity));
                }
            }
        }
        if hits.is_empty() {
            return Decision::clean();
        }
        hits.sort();
        hits.dedup();
        let severity = hits.iter().map(|(_, s)| *s).max();
        Decision {
            outcome: Outcome::Publish,
            labels: hits.into_iter().map(|(l, _)| l).collect(),
            severity,
        }
    }
}

/// 审核管线：模式 + 词表 + 可选多模态。
pub struct Pipeline {
    mode: Mode,
    moderator: Moderator,
    media: Option<Arc<dyn MediaJudge>>,
}

impl Pipeline {
    pub fn new(mode: Mode, moderator: Moderator) -> Self {
        Self {
            mode,
            moderator,
            media: None,
        }
    }

    pub fn with_media(mut self, judge: Arc<dyn MediaJudge>) -> Self {
        self.media = Some(judge);
        self
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// 文本判定：命中分级 + 模式 → 处置结论。
    pub fn judge_text(&self, text: &str) -> Decision {
        let mut decision = self.moderator.scan(text);
        decision.outcome = self.outcome_for(decision.severity);
        decision
    }

    /// 文本 + 媒体合并判定：取两者更严的结论。
    pub fn judge(&self, text: &str, media: Option<(&str, &[u8])>) -> Decision {
        let mut decision = self.judge_text(text);
        let Some((mime, bytes)) = media else {
            return decision;
        };
        let Some(judge) = self.media.as_ref() else {
            // 没有多模态能力时不假装干净：带媒体的内容进人工队列
            // （先审后发模式仍会挂起，先发后审模式打标）。
            decision.labels.push("media_unscanned".into());
            decision.labels.sort();
            decision.labels.dedup();
            if decision.severity.is_none() {
                decision.severity = Some(Severity::Review);
            }
            decision.outcome = self.outcome_for(decision.severity);
            return decision;
        };
        match judge.judge(mime, bytes) {
            MediaVerdict::Clean => decision,
            MediaVerdict::Review => {
                decision.labels.push("media_review".into());
                decision.labels.sort();
                decision.labels.dedup();
                if decision.severity.is_none() {
                    decision.severity = Some(Severity::Review);
                }
                decision.outcome = self.outcome_for(decision.severity);
                decision
            }
            MediaVerdict::Block => {
                decision.labels.push("media_block".into());
                decision.labels.sort();
                decision.labels.dedup();
                decision.severity = Some(Severity::Block);
                decision.outcome = Outcome::Reject;
                decision
            }
        }
    }

    fn outcome_for(&self, severity: Option<Severity>) -> Outcome {
        match (severity, self.mode) {
            (None, _) => Outcome::Publish,
            // 硬违规：任何模式都不投递
            (Some(Severity::Block), _) => Outcome::Reject,
            (Some(Severity::Review), Mode::PreReview) => Outcome::Hold,
            (Some(Severity::Review), Mode::PostReview) => Outcome::PublishFlagged,
        }
    }
}

/// 归一化：全角→半角、去零宽、小写、leet、只留字母数字与 CJK。
pub fn normalize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for raw in input.chars() {
        let ch = match raw {
            // 零宽与格式控制符直接丢弃
            '\u{200B}'..='\u{200F}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}' => continue,
            // 全角 ASCII 区（！..～）
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(raw as u32 - 0xFEE0).unwrap_or(raw),
            // 全角空格
            '\u{3000}' => ' ',
            other => other,
        };
        let lower = ch.to_ascii_lowercase();
        let mapped = match lower {
            '0' => 'o',
            '1' => 'i',
            '3' => 'e',
            '4' => 'a',
            '5' => 's',
            '7' => 't',
            '@' => 'a',
            '$' => 's',
            other => other,
        };
        if mapped.is_ascii_alphanumeric() || is_cjk(mapped) {
            out.push(mapped);
        }
    }
    out
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_evasion() {
        assert_eq!(normalize("敏 感 词"), "敏感词");
        assert_eq!(normalize("敏\u{200b}感-词"), "敏感词");
        assert_eq!(normalize("ＰＯＲＮ"), "porn");
        assert_eq!(normalize("p0rn"), "porn");
        assert_eq!(normalize("5p4m"), "spam");
    }
}
