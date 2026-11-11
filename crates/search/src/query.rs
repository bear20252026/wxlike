//! 查询侧内核：G02（消息检索）与 G05（公开内容检索）**共用**的一条流水线。
//!
//! 为什么要抽出来：两个域原本各写一份"清洗用户输入 → 切词 → 摘要"的逻辑，
//! 于是一边改了转义另一边忘了。这里把它收成一份，并且刻意保持
//! **无第三方依赖、无 IO**——`SearchIndex`（tantivy）和
//! `wxlike_platform::content_index`（SQLite）都在它两侧插自己的适配器，
//! 谁也不能绕过这层的校验与转义。
//!
//! 三条硬规则：
//!
//! 1. **用户输入永远不是查询语言。** [`NormalizedQuery`] 只产出 token 与
//!    LIKE 模式串，布尔算子/字段名/引号在解析阶段就变成字面量。
//! 2. **通配符按字面量。** LIKE 侧 `%`、`_`、`\` 一律转义（[`escape_like`]）。
//! 3. **摘要是有界窗口。** [`NormalizedQuery::snippet`] 只回 `max` 个字符，
//!    不复制整篇正文（最小权限：检索响应不该是全文的搬运工）。
//! 4. **时间窗是半开区间，且解析失败就拒绝。** [`TimeRange`] 借 OpenIM 的
//!    一天窗口 `[dayStart, dayStart+1d)` 与 box-im 的服务端下界钳制；
//!    "看不懂这个日期"永远不折算成"那就不限时间"。

use std::fmt;

/// 查询串长度上限（两个域共用同一个上限，别再各写一份）。
pub const MAX_QUERY_CHARS: usize = 100;
/// token 数上限：超长输入截断，防查询风暴。
pub const MAX_TOKENS: usize = 16;
/// 摘要默认窗口（字符）。
pub const DEFAULT_SNIPPET_CHARS: usize = 120;
/// 中文/日文汉字区（BMP 基本区 + 扩展 A）。检索按二元组切，因为 CJK 没有词边界。
pub fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// 查询被拒绝的原因。**不**用 anyhow：调用方要据此映射 422，
/// 需要能匹配到具体分支，而不是读错误字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryError {
    Empty,
    TooLong,
    ControlChar,
    MalformedCursor,
    MalformedTimeRange,
    UnknownScope,
}

impl QueryError {
    /// 面向 API 的稳定文案（同时是给客户端看的唯一信息——
    /// 不透露"命中了但不可见"这种存在性线索）。
    pub fn as_str(self) -> &'static str {
        match self {
            QueryError::Empty => "query must not be empty",
            QueryError::TooLong => "query too long",
            QueryError::ControlChar => "query contains control characters",
            QueryError::MalformedCursor => "malformed cursor",
            QueryError::MalformedTimeRange => "malformed time range",
            QueryError::UnknownScope => "unknown search scope",
        }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for QueryError {}

/// 清洗后的查询。构造不出来就等于没通过闸。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedQuery {
    text: String,
    tokens: Vec<String>,
}

impl NormalizedQuery {
    /// 去空白 → 限长 → 禁控制字符 → 小写。空串与超长都是**拒绝**，
    /// 不是"截断后继续"（截断会把 `%` 之类的残留送进 LIKE）。
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(QueryError::Empty);
        }
        if trimmed.chars().count() > MAX_QUERY_CHARS {
            return Err(QueryError::TooLong);
        }
        if trimmed.chars().any(char::is_control) {
            return Err(QueryError::ControlChar);
        }
        let text = trimmed.to_lowercase();
        Ok(Self {
            tokens: tokenize(&text),
            text,
        })
    }

    /// 归一化后的原始串（小写、已去首尾空白）。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 切好的 token（AND 语义用）。
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 子串/LIKE 检索用的模式串。转义在 [`escape_like`]，这里只加两端通配。
    pub fn like_pattern(&self) -> String {
        format!("%{}%", escape_like(&self.text))
    }

    /// 有界摘要：优先从命中处开始，找不到命中词就取开头。
    ///
    /// 位置换算走"小写串的字符序"：`to_lowercase` 可能改变字节长度
    /// （如 `İ`），直接拿字节下标切原串会在非 UTF-8 边界 panic。
    pub fn snippet(&self, body: &str, max: usize) -> String {
        let lower = body.to_lowercase();
        let skip = lower
            .find(&self.text)
            .map(|byte| lower[..byte].chars().count())
            .unwrap_or(0);
        body.chars().skip(skip).take(max.max(1)).collect()
    }

    /// 热搜词提取：ASCII 词（≥3 且非停用词）+ CJK 二元组。
    /// 与 [`tokenize`] 不同：那是"检索单元"，这是"聚合单元"。
    pub fn terms(&self) -> Vec<String> {
        terms(&self.text)
    }
}

/// 索引侧/查询侧共用的切词：字母数字与 CJK 连成一段（ngram 分词器在
/// 索引侧再切一次，所以 "pineapple" 能命中 "pineapples"）。
///
/// 这层同时是 Tantivy 查询语法防火墙：`AND` / `field:` / 引号在这里
/// 只会变成普通 token，永远不会进解析器。
pub fn tokenize(input: &str) -> Vec<String> {
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

/// LIKE 通配符转义：`%`、`_`、`\`。用户输入里的这些只能当字面量。
/// 调用方的 LIKE 必须带 `ESCAPE '\'`，否则 `\\` 本身又不对了。
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

/// 热搜/统计用的词条切分：ASCII 词 + CJK 二元组，稳定排序去重。
pub fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(ch.to_ascii_lowercase());
        } else if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out.sort();
    out.dedup();
    out
}

fn flush_ascii(buf: &mut String, out: &mut Vec<String>) {
    if buf.chars().count() >= 3 && !STOPWORDS.contains(&buf.as_str()) {
        out.push(std::mem::take(buf));
    }
    buf.clear();
}

fn flush_cjk(buf: &mut Vec<char>, out: &mut Vec<String>) {
    if buf.len() >= 2 {
        for pair in buf.windows(2) {
            out.push(pair.iter().collect());
        }
    }
    buf.clear();
}

/// 英文高频词：进热搜等于把榜单让给"the"。
const STOPWORDS: [&str; 6] = ["the", "and", "for", "with", "this", "that"];

/// 统一分页游标：`"{created_at}:{doc_id}"`。
///
/// `None` = 从最新开始，用 `(i64::MAX, SENTINEL)` 表示"比任何行都新"，
/// 这样比较式 `created_at < ? OR (created_at = ? AND doc_id < ?)` 对
/// 所有页统一成立，不需要为首页写一条特殊 SQL。
pub const FIRST_CURSOR: (i64, &str) = (i64::MAX, "\u{10ffff}");
/// 游标里 doc id 的最大长度：内容 id 全部由服务端生成（ASCII 字母数字、
/// 定长上限），不接受其他形态，防止把外部拼出的游标当检索入口。
pub const MAX_CURSOR_ID_LEN: usize = 64;

/// 校验游标里的 id 形态（ASCII 字母数字、非空、≤64）。
pub fn cursor_id_is_valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_CURSOR_ID_LEN && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 解析 `"{at}:{id}"`。返回 `(before_at, before_id)`。
pub fn parse_cursor(cursor: Option<&str>, limit_ms: bool) -> Result<(i64, String), QueryError> {
    let Some(raw) = cursor.filter(|c| !c.is_empty()) else {
        return Ok((FIRST_CURSOR.0, FIRST_CURSOR.1.to_string()));
    };
    let Some((at, id)) = raw.split_once(':') else {
        return Err(QueryError::MalformedCursor);
    };
    let at: i64 = at.parse().map_err(|_| QueryError::MalformedCursor)?;
    if limit_ms && at < 0 {
        return Err(QueryError::MalformedCursor);
    }
    if !cursor_id_is_valid(id) {
        return Err(QueryError::MalformedCursor);
    }
    Ok((at, id.to_string()))
}

/// 一天的毫秒数（UTC，不考虑闰秒）。
pub const DAY_MS: i64 = 86_400_000;
/// 客户端能请求的最大时间跨度。超出即被 [`TimeRange::narrow_to`] 砍到该值，
/// 而不是报错——"检索最近十年"和"检索全部"在扇出成本上是同一件事。
pub const MAX_RANGE_SPAN_MS: i64 = 92 * DAY_MS;

/// 检索/聚合的时间窗，**半开区间 `[start_ms, end_ms)`**。
///
/// 这个形状是从 OpenIM Server 的 `MsgMgo.searchMessage` 借的：它把客户端传来
/// 的 `sendTime`（一个 `YYYY-MM-DD` 日期串）展开成
/// `send_time >= dayStart && send_time < dayStart+24h`，即**左闭右开的一天**；
/// 而日期串解析失败时它返回 `errs.ErrArgs`（400），**不会**把"解析不了"折成
/// "不加时间约束"。第二个性质是本 crate 之前缺的那一半：一旦畸形输入退化成
/// 无界，`?sendTime=` 手滑的调用就会拿到全库结果。
///
/// 下界钳制的口径来自 box-im `GroupMessageServiceImpl.loadHistoryMessage`：
/// 它取 `minDate = max(now - 保留天数, 成员入群时间)`，即**服务端把窗口下界
/// 往现在收**，客户端给的起点不能把它往前推。[`TimeRange::narrow_to`] 就是这个
/// `max` + 跨度上限的组合。
///
/// 缺省语义（两个上游一致）：OpenIM 的每个 facet 都是"字段非零才加进 filter"，
/// box-im 的 DTO 直接写明"maxSeqNo 0 或负值表示不限制"。所以 `None` /
/// [`TimeRange::UNBOUNDED`] = **不加约束**，绝不等于**放宽可见范围**——
/// 授权裁决和这个窗口是两条独立的闸，谁也不替谁开门。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeRange {
    start_ms: i64,
    end_ms: i64,
}

impl TimeRange {
    /// "不加约束"。用极值而不是 `Option`，是为了让调用方的比较式对首页/无约束
    /// 两种情况都写成同一条 SQL（同 [`FIRST_CURSOR`] 的理由）。
    pub const UNBOUNDED: Self = Self {
        start_ms: i64::MIN,
        end_ms: i64::MAX,
    };

    /// 显式区间。空区间与反区间都是**拒绝**（`Err`），不是"返回 0 条"的静默：
    /// 把 `end <= start` 折成空结果集，等于让调用方以为检索正常跑了。
    pub fn between(start_ms: i64, end_ms: i64) -> Result<Self, QueryError> {
        if end_ms <= start_ms {
            return Err(QueryError::MalformedTimeRange);
        }
        Ok(Self { start_ms, end_ms })
    }

    /// `YYYY-MM-DD`（UTC）→ 该天的 `[00:00:00.000, +1d)`。
    ///
    /// 严格解析：位数、分隔符、月日范围、闰年 2/29 都要过，任何一条不过就
    /// `Err(MalformedTimeRange)`。这里**不**接受日期时间混合串，也不接受
    /// 前后空白——同一句 SQL 里两种写法混用会让"同一天"出现两种边界。
    pub fn from_date(date: &str) -> Result<Self, QueryError> {
        let (y, m, d) = split_ymd(date)?;
        let start = days_from_civil(y, m, d)
            .checked_mul(DAY_MS)
            .ok_or(QueryError::MalformedTimeRange)?;
        let end = start
            .checked_add(DAY_MS)
            .ok_or(QueryError::MalformedTimeRange)?;
        Ok(Self {
            start_ms: start,
            end_ms: end,
        })
    }

    /// 某天的窗口，但**上界收到 `now_ms`**：`[dayStart, min(dayEnd, now))`。
    /// 用在"检索今天"这种不能让未来行入场的地方；若该天完全在未来则为空区间，
    /// 按 [`Self::between`] 的口径报错而不是默默返回 0 条。
    pub fn today_until_now(date: &str, now_ms: i64) -> Result<Self, QueryError> {
        let day = Self::from_date(date)?;
        Self::between(day.start_ms, day.end_ms.min(now_ms))
    }

    /// 包含判定——半开的右端不收。
    pub fn contains(&self, ms: i64) -> bool {
        self.start_ms <= ms && ms < self.end_ms
    }

    pub fn start_ms(&self) -> i64 {
        self.start_ms
    }

    pub fn end_ms(&self) -> i64 {
        self.end_ms
    }

    /// 服务端收口：下界不得早于 `floor_ms`（保留期/成员关系起点），
    /// 上界不得晚于 `cap_ms`（"未来不检索"），跨度不超过 `max_span_ms`。
    ///
    /// 三条都只往**内**收，永不外推，所以客户端无论传什么都拿不到比自己
    /// 权限/保留期更宽的结果集。跨度收口以 `end` 为锚点往前砍——理由是
    /// keyset 游标从新往旧翻，保住的是调用方最关心的那一段。
    pub fn narrow_to(self, floor_ms: i64, cap_ms: i64, max_span_ms: i64) -> Self {
        let start = self.start_ms.max(floor_ms);
        let end = self.end_ms.min(cap_ms.max(start + 1));
        let span = max_span_ms.max(1);
        let start = start.max(end.saturating_sub(span));
        Self {
            start_ms: start,
            end_ms: end,
        }
    }

    /// 与 [`crate::trends`] 的服务端窗口求交。交集为空时返回 `None`
    /// （= 这一窗确实没有可算的行），由调用方决定是报错还是回空榜。
    pub fn intersect(self, other: Self) -> Option<Self> {
        let start = self.start_ms.max(other.start_ms);
        let end = self.end_ms.min(other.end_ms);
        if end > start {
            Some(Self {
                start_ms: start,
                end_ms: end,
            })
        } else {
            None
        }
    }
}

/// 日历常量，[`split_ymd`] / [`days_from_civil`] / [`civil_from_days`] 共用一份。
///
/// 提出来不只是为了好看：这三个函数里的数字互相咬合（纪元 400 年、5 个月 153 天、
/// 400 年 146097 天、到 1970 的偏移 719468），散在算式里时**任何一处走偏都只有
/// 跨闰世纪的那条用例才会红**。C10 的 `magic_total` 不数 const 行，正好把
/// "同一份常数抄了三遍"这类债挡在门外。
const MONTHS_PER_YEAR: i64 = 12;
const DAYS_PER_YEAR: i64 = 365;
const DAYS_IN_5_MONTHS: i64 = 153; // 3..7 月：31+28+31+30+31
const YEARS_PER_ERA: i64 = 400;
const DAYS_IN_4_YEARS_MINUS_1: i64 = 1460;
const DAYS_IN_100_YEARS_MINUS_1: i64 = 36_524;
const DAYS_IN_400_YEARS_MINUS_1: i64 = 146_096;
const DAYS_IN_400_YEARS: i64 = 146_097;
const DAYS_FROM_0000_TO_1970: i64 = 719_468;
const DAYS_IN_FEBRUARY_LEAP: i64 = 29;
/// 平年每月天数；2 月由调用方按闰年替换（见 [`split_ymd`]）。
const MONTH_DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// `YYYY-MM-DD` 拆分 + 日历合法性（含闰年）。返回 `(year, month, day)`。
fn split_ymd(s: &str) -> Result<(i64, i64, i64), QueryError> {
    let bytes = s.as_bytes();
    if bytes.len() != 10
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || bytes[4] != b'-'
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || bytes[7] != b'-'
        || !bytes[8..].iter().all(u8::is_ascii_digit)
    {
        return Err(QueryError::MalformedTimeRange);
    }
    let y: i64 = s[..4].parse().map_err(|_| QueryError::MalformedTimeRange)?;
    let m: i64 = s[5..7]
        .parse()
        .map_err(|_| QueryError::MalformedTimeRange)?;
    let d: i64 = s[8..].parse().map_err(|_| QueryError::MalformedTimeRange)?;
    if !(1..=MONTHS_PER_YEAR).contains(&m) || d < 1 {
        return Err(QueryError::MalformedTimeRange);
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % YEARS_PER_ERA == 0;
    let mut days_in_month = MONTH_DAYS;
    days_in_month[1] = if leap {
        DAYS_IN_FEBRUARY_LEAP
    } else {
        MONTH_DAYS[1]
    };
    if d > days_in_month[(m - 1) as usize] {
        return Err(QueryError::MalformedTimeRange);
    }
    Ok((y, m, d))
}

/// 公历日期 → 自 1970-01-01 的天数（civil → fixed）。
///
/// 该处自研：算法是公知的 Hinnant `days_from_civil`（以 3 月 1 日为纪元内的
/// 年首，把 153 天的月和 146097 天的世纪折叠成整除式），本仓不依赖 chrono，
/// 所以把它写在这里。负数除法一律走"先按符号平移再加权"的地板除形式，
/// 因此 1969-12-31 得到 `-1` 而不是 Rust 截断除法给出的 `0`。
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - (YEARS_PER_ERA - 1) }) / YEARS_PER_ERA;
    let yoe = y - era * YEARS_PER_ERA; // [0, 399]
    let mp = (m + 9) % MONTHS_PER_YEAR;
    let doy = (DAYS_IN_5_MONTHS * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * DAYS_PER_YEAR + yoe / 4 - yoe / 100 + doy;
    era * DAYS_IN_400_YEARS + doe - DAYS_FROM_0000_TO_1970
}

/// [`days_from_civil`] 的逆：天数（地板除到日）→ `(年, 月, 日)`，UTC。
pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + DAYS_FROM_0000_TO_1970;
    let era = (if z >= 0 {
        z
    } else {
        z - (DAYS_IN_400_YEARS - 1)
    }) / DAYS_IN_400_YEARS;
    let doe = z - era * DAYS_IN_400_YEARS; // [0, 146096]
    let yoe = (doe - doe / DAYS_IN_4_YEARS_MINUS_1 + doe / DAYS_IN_100_YEARS_MINUS_1
        - doe / DAYS_IN_400_YEARS_MINUS_1)
        / DAYS_PER_YEAR; // [0, 399]
    let y = yoe + era * YEARS_PER_ERA;
    let doy = doe - (DAYS_PER_YEAR * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / DAYS_IN_5_MONTHS; // [0, 11]，3 月为首
    let d = doy - (DAYS_IN_5_MONTHS * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_range_is_half_open_and_strict() {
        let day = TimeRange::from_date("2026-11-07").unwrap();
        assert_eq!(day.end_ms - day.start_ms, DAY_MS);
        assert!(day.contains(day.start_ms), "左闭");
        assert!(!day.contains(day.end_ms), "右开：下一天的 00:00 不属于今天");
        // 解析失败必须拒绝，不能退化成"不加约束"（OpenIM 在这里返 ErrArgs）
        for bad in [
            "",
            "   ",
            "2026-11-7",
            "2026-11-07T00:00:00",
            " 2026-11-07",
            "2026-11-07 ",
            "2026-13-01",
            "2026-00-10",
            "2026-11-00",
            "2026-11-31",
            "2026-02-29",
            "2025-02-29",
            "abcd-ef-gh",
        ] {
            assert_eq!(
                TimeRange::from_date(bad),
                Err(QueryError::MalformedTimeRange),
                "{bad:?}"
            );
        }
        // 闰年真的要放行
        assert!(TimeRange::from_date("2024-02-29").is_ok());
        assert!(TimeRange::from_date("2000-02-29").is_ok());
    }

    #[test]
    fn civil_day_math_matches_known_epochs() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(
            days_from_civil(1969, 12, 31),
            -1,
            "地板除：截断除法会算成 0"
        );
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(days_from_civil(2026, 11, 7), 20_764);
        for (y, m, d) in [
            (1970, 1, 1),
            (1969, 12, 31),
            (2000, 2, 29),
            (1900, 1, 31),
            (2026, 11, 7),
            (2100, 3, 1),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        // 借来的日期串和算出来的日键必须互逆
        let day = TimeRange::from_date("2026-11-07").unwrap();
        assert_eq!(
            civil_from_days(day.start_ms.div_euclid(DAY_MS)),
            (2026, 11, 7)
        );
    }

    #[test]
    fn inverted_or_empty_window_is_refused_not_empty_result() {
        assert_eq!(
            TimeRange::between(10, 10),
            Err(QueryError::MalformedTimeRange)
        );
        assert_eq!(
            TimeRange::between(20, 10),
            Err(QueryError::MalformedTimeRange)
        );
        // 整天完全在未来：收口到 now 之后区间为空，同样拒绝
        assert!(TimeRange::today_until_now("2027-01-01", 1_700_000_000_000).is_err());
        // 2026-11-07 的 UTC 日界：[1_794_009_600_000, 1_794_096_000_000)
        let day_start = 1_794_009_600_000;
        let day_end = 1_794_096_000_000;
        assert_eq!(
            TimeRange::from_date("2026-11-07").unwrap(),
            TimeRange::between(day_start, day_end).unwrap()
        );
        // now 落在当天之内：右端收到 now
        let clipped = TimeRange::today_until_now("2026-11-07", day_start + 500).unwrap();
        assert_eq!(clipped.end_ms, day_start + 500);
        assert!(clipped.contains(day_start));
        assert!(clipped.contains(day_start + 499));
        assert!(
            !clipped.contains(day_start + 500),
            "半开：now 那一毫秒不给未来行留门"
        );
        // now 已经过了当天：整日照原样返回（不再往外扩）
        assert_eq!(
            TimeRange::today_until_now("2026-11-07", day_end + 1).unwrap(),
            TimeRange::between(day_start, day_end).unwrap()
        );
    }

    #[test]
    fn server_clamp_only_ever_narrows() {
        let floor = 1_000;
        let cap = 10_000;
        // 客户端想从宇宙尽头翻到宇宙尽头（= "全历史"）
        let wide = TimeRange::UNBOUNDED.narrow_to(floor, cap, MAX_RANGE_SPAN_MS);
        assert_eq!(wide.start_ms, floor, "下界被保留期收住，不外推");
        assert_eq!(wide.end_ms, cap, "上界被 now 收住，未来行不入场");
        // 跨度上限以 end 为锚往前砍
        let long = TimeRange::between(0, 1_000_000)
            .unwrap()
            .narrow_to(i64::MIN, i64::MAX, 500);
        assert_eq!(long, TimeRange::between(999_500, 1_000_000).unwrap());
        // 已经比窗口窄的请求原样通过（钳制只做交集）
        let tight = TimeRange::between(2_000, 3_000).unwrap();
        assert_eq!(tight.narrow_to(floor, cap, MAX_RANGE_SPAN_MS), tight);
        // cap 落在 floor 之后时区间至少留 1ms，不产生反区间
        assert_eq!(
            TimeRange::UNBOUNDED.narrow_to(5_000, 1, MAX_RANGE_SPAN_MS),
            TimeRange::between(5_000, 5_001).unwrap()
        );
    }

    #[test]
    fn intersect_reports_empty_as_none() {
        let a = TimeRange::between(0, 100).unwrap();
        let b = TimeRange::between(50, 150).unwrap();
        assert_eq!(a.intersect(b), TimeRange::between(50, 100).ok());
        assert_eq!(a.intersect(TimeRange::between(100, 200).unwrap()), None);
        assert_eq!(
            a.intersect(TimeRange::UNBOUNDED),
            Some(a),
            "无约束求交是恒等"
        );
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;

    #[test]
    fn parse_rejects_empty_long_and_control_input() {
        assert_eq!(NormalizedQuery::parse("  "), Err(QueryError::Empty));
        assert_eq!(
            NormalizedQuery::parse(&"x".repeat(MAX_QUERY_CHARS + 1)),
            Err(QueryError::TooLong)
        );
        assert_eq!(
            NormalizedQuery::parse("ok\u{7}bad"),
            Err(QueryError::ControlChar)
        );
        assert_eq!(
            NormalizedQuery::parse("  RustLang  ").unwrap().text(),
            "rustlang"
        );
    }

    #[test]
    fn like_pattern_is_escaped() {
        let q = NormalizedQuery::parse("100%_x\\y").unwrap();
        assert_eq!(q.like_pattern(), "%100\\%\\_x\\\\y%");
    }

    #[test]
    fn tokenizer_keeps_cjk_runs_and_blocks_query_syntax() {
        assert_eq!(tokenize("慢速模式 slow mode"), ["慢速模式", "slow", "mode"]);
        assert_eq!(tokenize("AND"), ["AND"]);
        assert_eq!(tokenize("sender:alice"), ["sender", "alice"]);
        let many = tokenize(&vec!["a"; MAX_TOKENS + 20].join(" "));
        assert_eq!(many.len(), MAX_TOKENS);
    }

    #[test]
    fn snippet_is_bounded_and_survives_case_folding_length_change() {
        let body = "前言".repeat(200) + "慢速模式" + &"后续".repeat(200);
        let q = NormalizedQuery::parse("慢速模式").unwrap();
        let s = q.snippet(&body, 12);
        assert_eq!(s.chars().count(), 12);
        assert!(s.starts_with("慢速模式"));
        // İ -> "i̇" ：小写化后字节数变了，按字节切会 panic
        let q = NormalizedQuery::parse("İ").unwrap();
        assert_eq!(q.snippet("İx", 4), "İx");
        let q = NormalizedQuery::parse("missing").unwrap();
        assert_eq!(q.snippet("abcdef", 3), "abc");
    }

    #[test]
    fn terms_are_ascii_words_and_cjk_bigrams() {
        let t = terms("慢速模式 rust language the");
        assert!(t.contains(&"慢速".to_string()));
        assert!(t.contains(&"速模".to_string()));
        assert!(t.contains(&"rust".to_string()));
        assert!(!t.contains(&"the".to_string()));
        assert!(!t.contains(&"ab".to_string()), "ASCII 词要 >=3");
        assert_eq!(t, {
            let mut v = t.clone();
            v.sort();
            v.dedup();
            v
        });
    }

    #[test]
    fn cursor_shape_is_strict() {
        assert_eq!(
            parse_cursor(None, true).unwrap(),
            (i64::MAX, "\u{10ffff}".into())
        );
        assert_eq!(
            parse_cursor(Some("9:abc123"), true).unwrap(),
            (9, "abc123".into())
        );
        for bad in [
            "9",
            "x:abc",
            "9:a b",
            "9:",
            "9:abc:9",
            "-1:abc",
            "9:../../x",
        ] {
            assert_eq!(
                parse_cursor(Some(bad), true),
                Err(QueryError::MalformedCursor),
                "{bad}"
            );
        }
        // 非毫秒域（如 seq 游标）允许负数，但 id 形态照样收紧
        assert_eq!(
            parse_cursor(Some("-5:ab"), false).unwrap(),
            (-5, "ab".into())
        );
    }
}
