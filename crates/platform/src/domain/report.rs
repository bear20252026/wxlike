//! P4 举报工单的持久化与 HTTP 面（状态机在 [`wxlike_report`]）。
//!
//! 分层理由：状态机是纯逻辑（可脱离数据库验收），本文件只做"读出来 →
//! 应用动作 → 写回去"，不重新实现任何流转规则。所有写路径都经过
//! `Ticket::triage/act/close/reject`，非法流转在这里就是 409，而不是
//! 靠 SQL 的 CHECK 兜底。
//!
//! 权限：举报人只能看自己的工单；处置动作需要管理令牌（P6 adminx 之前用
//! `x-admin-token`，见 [`crate::PlatformOptions::admin_token`]）。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::report`]。

use crate::error::{PlatformError, Result};
use crate::Platform;
use rusqlite::{params, OptionalExtension as _};
use serde::Serialize;
use wxlike_report::{Action, Reason, State as TicketState, Ticket};

/// `reports` 一行的读取形状（列多，别名避免 clippy::type_complexity）。
type ReportRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
    String,
);

/// 处置请求体。
///
/// `Action` 是**内部标签**枚举（`#[serde(tag = "action")]`），它的原生形状
/// 本来就是平铺的：
///
/// ```json
/// {"action": "mute", "duration_ms": 60000, "note": "禁言一小时"}
/// ```
///
/// 但历史上 `ActBody { action: Action }` 直接 derive 出来的形状是**嵌套**的
/// （`{"action": {"action": "warn"}}`）——同一个键出现两次，很难看，也让
/// 客户端和文档各写各的。`crates/admin` 早就遇到过同一个问题，做法是
/// "两种都收、在边界归一化"（见 `admin::accounts::parse_action`）。这里照做：
///
/// - **平铺是规范形状**，文档与 curl 示例都用它；
/// - **嵌套仍然接受**，因为已经发出去的调用方不该被一次格式调整打断；
/// - 缺 `action` 一律 422，不猜默认值——"默默按 warn 处理"是最坏的兜底。
///
/// # 兼容分支的删除条件
///
/// 嵌套形状**不是永久契约**，它有一个明确的退出条件：
///
/// 1. `act` handler 在收到嵌套形状时打一条 `WARN` 日志（带工单 id 与操作人）；
/// 2. 连续 **30 天**没有这条日志，说明已无调用方使用；
/// 3. 届时删掉 `parse_action_value` 的对象分支、`legacy_shape` 字段与对应的
///    兼容测试——**删代码，不是加开关**。
///
/// 没有第 2 步的观测，第 3 步就永远做不了；这也是为什么 `legacy_shape` 要
/// 跟着数据从 `Deserialize` 里传出来，而不是在反序列化时直接打日志。
#[derive(Debug)]
pub struct ActBody {
    pub action: Action,
    pub note: String,
    /// 请求用的是旧的嵌套形状（`{"action":{"action":"warn"}}`）。
    ///
    /// `Deserialize` 只**记录**这个事实、不打日志——反序列化应当是纯函数，
    /// 副作用留给 handler（那里有 tracing span、能带上请求 id 与操作人）。
    /// 有了这个标记，运维才能回答"还有谁在用旧形状"，进而决定何时删掉兼容分支。
    pub legacy_shape: bool,
}

impl<'de> serde::Deserialize<'de> for ActBody {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let raw = serde_json::Value::deserialize(deserializer)?;
        let note = raw
            .get("note")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let (action, legacy_shape) = parse_action_value(&raw).map_err(D::Error::custom)?;
        Ok(Self {
            action,
            note,
            legacy_shape,
        })
    }
}

/// 从请求体里取出 `Action`，并告诉调用方"这是不是旧形状"。
///
/// 三条分支各有各的话，**不拼接 serde 的内部错误**：
///
/// 1. 没有 `action` → "缺少 action"（附可选值）；
/// 2. `action` 不是对象（规范形状）→ 直接按 `Action` 解析，报 serde 的具体原因
///    （比如 mute 缺 `duration_ms`）；
/// 3. `action` 是对象 → 只可能是旧的嵌套形状，或把 tag 写丢了。
///
/// 以前这里对**任何**失败都去试嵌套，于是 `{"action": 42}` 会得到
/// `flat=...; nested=...` 两句一模一样的错——噪音盖住了病因。
fn parse_action_value(raw: &serde_json::Value) -> std::result::Result<(Action, bool), String> {
    const HELP: &str = "可选：warn / mute{\"duration_ms\":N} / ban / remove_content / none";

    let Some(value) = raw.get("action") else {
        return Err(format!("缺少 action。{HELP}"));
    };

    // 规范形状：action 是字符串（mute 的时长写在同一层）
    if !value.is_object() {
        let action = serde_json::from_value::<Action>(raw.clone())
            .map_err(|err| format!("action 不合法（{err}）。{HELP}"))?;
        return Ok((action, false));
    }

    // action 是对象：旧的嵌套形状，或者把 tag 写丢了
    match serde_json::from_value::<Action>(value.clone()) {
        Ok(action) => Ok((action, true)),
        Err(_) => Err(format!(
            "action 写成对象时必须是旧的嵌套形状              {{\"action\":{{\"action\":\"warn\"}}}}；推荐改成平铺 {{\"action\":\"warn\"}}。{HELP}"
        )),
    }
}

#[derive(Serialize)]
pub(crate) struct TicketView<'a> {
    pub(crate) ticket: &'a Ticket,
    pub(crate) overdue: bool,
}

impl Platform {
    /// 创建工单（风控由调用方在路由层过闸：举报轰炸在 `recovery` 模块的
    /// `risk_gate` 里拦）。
    pub fn create_report(
        &self,
        reporter: &str,
        target_kind: &str,
        target_id: &str,
        reason: Reason,
        detail: &str,
    ) -> Result<Ticket> {
        let id = crate::auth::random_hex(8);
        let ticket = Ticket::open(
            &id,
            reporter,
            target_kind,
            target_id,
            reason,
            detail,
            wxlike_core::types::now_ms(),
        )
        .map_err(map_report_error)?;
        self.save_report(&ticket)?;
        self.audit(reporter, "report.create", &id, target_kind);
        Ok(ticket)
    }

    pub fn report(&self, id: &str) -> Result<Option<Ticket>> {
        let c = self.db().lock();
        let row: Option<ReportRow> = c
            .query_row(
                "SELECT reporter, target_kind, target_id, reason, detail, state, action_json,
                        handler, created_at, updated_at, history_json
                 FROM reports WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            reporter,
            target_kind,
            target_id,
            reason,
            detail,
            state,
            action_json,
            handler,
            created_at,
            updated_at,
            history_json,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(Ticket {
            id: id.to_string(),
            reporter,
            target_kind,
            target_id,
            reason: Reason::parse(&reason).ok_or(PlatformError::BadInput("bad reason"))?,
            detail,
            state: TicketState::parse(&state).ok_or(PlatformError::BadInput("bad state"))?,
            created_at: created_at.max(0) as u64,
            updated_at: updated_at.max(0) as u64,
            action: action_json.and_then(|s| serde_json::from_str(&s).ok()),
            handler,
            history: serde_json::from_str(&history_json).unwrap_or_default(),
        }))
    }

    /// 举报人自己的工单，最新在前。
    pub fn reports_by(&self, reporter: &str, limit: usize) -> Result<Vec<Ticket>> {
        let ids: Vec<String> = {
            let c = self.db().lock();
            let mut stmt = c.prepare(
                "SELECT id FROM reports WHERE reporter = ?1 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![reporter, limit.clamp(1, 200) as i64], |r| {
                r.get::<_, String>(0)
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = self.report(&id)? {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// 处置队列：按状态过滤（`open` / `triaged` / 空 = 全部未终结）。
    pub fn report_queue(&self, state: Option<&str>, limit: usize) -> Result<Vec<Ticket>> {
        let ids: Vec<String> = {
            let c = self.db().lock();
            match state {
                Some(s) => {
                    let mut stmt = c.prepare(
                        "SELECT id FROM reports WHERE state = ?1 ORDER BY created_at ASC LIMIT ?2",
                    )?;
                    let rows = stmt.query_map(params![s, limit.clamp(1, 200) as i64], |r| {
                        r.get::<_, String>(0)
                    })?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()?
                }
                None => {
                    let mut stmt = c.prepare(
                        "SELECT id FROM reports WHERE state IN ('open','triaged','actioned')
                         ORDER BY created_at ASC LIMIT ?1",
                    )?;
                    let rows = stmt.query_map(params![limit.clamp(1, 200) as i64], |r| {
                        r.get::<_, String>(0)
                    })?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()?
                }
            }
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = self.report(&id)? {
                out.push(t);
            }
        }
        Ok(out)
    }

    pub fn triage_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket> {
        let mut t = self.report(id)?.ok_or(PlatformError::NotFound)?;
        t.triage(actor, note, wxlike_core::types::now_ms())
            .map_err(map_report_error)?;
        self.save_report(&t)?;
        Ok(t)
    }

    pub fn act_on_report(
        &self,
        id: &str,
        actor: &str,
        action: Action,
        note: &str,
    ) -> Result<Ticket> {
        let mut t = self.report(id)?.ok_or(PlatformError::NotFound)?;
        t.act(actor, action, note, wxlike_core::types::now_ms())
            .map_err(map_report_error)?;
        self.save_report(&t)?;
        self.audit(actor, "report.action", id, &format!("{:?}", t.action));
        Ok(t)
    }

    pub fn close_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket> {
        let mut t = self.report(id)?.ok_or(PlatformError::NotFound)?;
        t.close(actor, note, wxlike_core::types::now_ms())
            .map_err(map_report_error)?;
        self.save_report(&t)?;
        Ok(t)
    }

    pub fn reject_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket> {
        let mut t = self.report(id)?.ok_or(PlatformError::NotFound)?;
        t.reject(actor, note, wxlike_core::types::now_ms())
            .map_err(map_report_error)?;
        self.save_report(&t)?;
        Ok(t)
    }

    fn save_report(&self, t: &Ticket) -> Result<()> {
        let action = match &t.action {
            Some(a) => Some(serde_json::to_string(a)?),
            None => None,
        };
        let history = serde_json::to_string(&t.history)?;
        let c = self.db().lock();
        c.execute(
            "INSERT INTO reports(id, reporter, target_kind, target_id, reason, detail, state,
                                 action_json, handler, created_at, updated_at, history_json)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
               state = excluded.state,
               action_json = excluded.action_json,
               handler = excluded.handler,
               updated_at = excluded.updated_at,
               history_json = excluded.history_json",
            params![
                t.id,
                t.reporter,
                t.target_kind,
                t.target_id,
                t.reason.as_str(),
                t.detail,
                t.state.as_str(),
                action,
                t.handler,
                // rusqlite 0.41 起 `u64` 不再实现 `ToSql`。SQLite 的 INTEGER 就是
                // 有符号 64 位，而工单时间戳是正数且远小于 `i64::MAX`，直接 `as i64`。
                t.created_at as i64,
                t.updated_at as i64,
                history
            ],
        )?;
        Ok(())
    }
}

fn map_report_error(e: wxlike_report::ReportError) -> PlatformError {
    match e {
        wxlike_report::ReportError::IllegalTransition { .. } => {
            PlatformError::Conflict("illegal ticket transition".into())
        }
        wxlike_report::ReportError::EmptyActor => PlatformError::BadInput("actor required"),
        wxlike_report::ReportError::NoteTooLong => PlatformError::BadInput("note too long"),
        wxlike_report::ReportError::BadTargetKind => PlatformError::BadInput(
            "target_kind must be user|message|group|channel|moment|article|video|status",
        ),
        wxlike_report::ReportError::Terminal => PlatformError::Conflict("ticket closed".into()),
    }
}
