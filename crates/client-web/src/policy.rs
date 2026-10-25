//! 浏览器侧的**纯策略**：不碰任何 `web-sys` 类型，因此可以在原生上跑单测。
//!
//! 浏览器与原生最大的差别不是 API，而是**失败模式**：
//!
//! * 配额用尽（`QuotaExceededError`）不是"磁盘满"，而是"这个源用太多了"，
//!   用户清一下就能好，所以提示文案完全不同；
//! * 私有/无痕模式下 IndexedDB **能打开但写不进**，或者干脆打不开，
//!   表现为 `InvalidStateError` / `SecurityError`——这时候不该白屏，
//!   该进"只读 + 不缓存"降级；
//! * 页面会被 `freeze`（后台标签页省电），定时器停摆、连接可能已断。
//!   恢复时必须**重新校验游标并补发**，不能假设"什么都没发生"。
//!
//! 这些判断写成纯函数，是为了让它们能被测试钉住——写在 `web-sys` 调用旁边
//! 就只能靠手工点页面来验。

use wxlike_client_core::ports::{Lifecycle, StoreError};

/// 浏览器异常的 `name` 字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomErrorName {
    /// 配额用尽。
    QuotaExceeded,
    /// 状态不对（私有模式下常见的"能打开不能写"）。
    InvalidState,
    /// 被安全策略拒绝（第三方 cookie/存储隔离）。
    Security,
    /// 事务被中止（版本变更、跨 tab 升级冲突）。
    Abort,
    /// 其它。
    Other,
}

impl DomErrorName {
    /// 从 `DOMException.name` 解析。
    #[must_use]
    pub fn parse(name: &str) -> Self {
        match name {
            "QuotaExceededError" => DomErrorName::QuotaExceeded,
            "InvalidStateError" => DomErrorName::InvalidState,
            "SecurityError" => DomErrorName::Security,
            "AbortError" => DomErrorName::Abort,
            _ => DomErrorName::Other,
        }
    }
}

/// 浏览器异常 → 端口错误。
///
/// **私有模式不该被当成"临时故障"**：它重试一万次也不会好，所以映射成
/// `Unavailable`（不可用）而不是 `Busy`（可重试）。界面据此显示"当前浏览
/// 模式不能离线保存"，而不是转圈。
#[must_use]
pub fn classify(name: &str, message: &str) -> StoreError {
    match DomErrorName::parse(name) {
        DomErrorName::QuotaExceeded => StoreError::QuotaExceeded,
        DomErrorName::InvalidState | DomErrorName::Security => {
            StoreError::Unavailable(format!("浏览器存储被禁用或不可写（{name}）：{message}"))
        }
        DomErrorName::Abort => StoreError::Busy,
        DomErrorName::Other => StoreError::Unavailable(format!("{name}：{message}")),
    }
}

/// 存储可用性判定结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageMode {
    /// 可读写。
    ReadWrite,
    /// 只能读（缓存命中就显示，写操作走内存 + 每次重拉）。
    ReadOnly,
    /// 完全不可用（私有模式、被策略禁用）。
    Unavailable,
}

impl StorageMode {
    /// 能不能把待发送消息落盘。
    ///
    /// 不能落盘时**必须阻止发送**并告知用户：否则用户以为发出去了，
    /// 刷新一下消息就消失了。
    #[must_use]
    pub fn can_queue(self) -> bool {
        matches!(self, StorageMode::ReadWrite)
    }

    /// 该不该继续把事件写进本地库。
    #[must_use]
    pub fn can_persist(self) -> bool {
        matches!(self, StorageMode::ReadWrite)
    }
}

/// 由一次打开失败的结果推断存储模式。
#[must_use]
pub fn storage_mode_after(error: Option<&StoreError>) -> StorageMode {
    match error {
        None => StorageMode::ReadWrite,
        Some(StoreError::QuotaExceeded) => StorageMode::ReadOnly,
        Some(StoreError::Unavailable(_)) => StorageMode::Unavailable,
        Some(_) => StorageMode::ReadOnly,
    }
}

/// 页面可见性 / 冻结状态机。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageState {
    /// 前台可见。
    Visible,
    /// 后台但可能仍在跑定时器。
    Hidden,
    /// 被冻结：定时器停摆，恢复时必须重新校验。
    Frozen,
}

impl PageState {
    /// 收到一个生命周期事件后的新状态。
    #[must_use]
    pub fn transition(self, event: Lifecycle) -> PageState {
        match event {
            Lifecycle::Foreground => PageState::Visible,
            Lifecycle::Background => PageState::Hidden,
            Lifecycle::Frozen => PageState::Frozen,
            // 从冻结恢复 = 回到前台；是否要重连由 `needs_resync` 决定
            Lifecycle::Resumed => PageState::Visible,
        }
    }

    /// 进入这个状态时要不要立刻做一次"游标校验 + 补发"。
    ///
    /// 只有从冻结恢复才需要：后台但没冻结时定时器还在跑，补发泵自己会处理。
    #[must_use]
    pub fn needs_resync(self, previous: PageState) -> bool {
        matches!(previous, PageState::Frozen) && matches!(self, PageState::Visible)
    }

    /// 这个状态下还该不该保持长连接。
    #[must_use]
    pub fn keeps_connection(self) -> bool {
        !matches!(self, PageState::Frozen)
    }
}

/// 缓存敏感度：决定要不要加密后再进 IndexedDB。
///
/// 判据是"泄露之后会不会直接害到用户"：消息正文、会话标题、token 都会。
/// 头像 URL、表情索引不会。
#[must_use]
pub fn is_sensitive_cache(key: &str) -> bool {
    const SENSITIVE_PREFIXES: [&str; 5] = ["msg:", "conv:", "secret:", "token", "profile:"];
    SENSITIVE_PREFIXES
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_is_quota_not_disk_full() {
        assert_eq!(
            classify("QuotaExceededError", "quota"),
            StoreError::QuotaExceeded
        );
        assert!(!classify("QuotaExceededError", "").is_retryable());
    }

    #[test]
    fn private_mode_is_unavailable_not_busy() {
        let error = classify("InvalidStateError", "IDB is disabled in private mode");
        assert!(matches!(error, StoreError::Unavailable(_)), "{error:?}");
        assert!(!error.is_retryable(), "私有模式重试不会好");
        assert_eq!(storage_mode_after(Some(&error)), StorageMode::Unavailable);
        assert!(!StorageMode::Unavailable.can_queue());
    }

    #[test]
    fn abort_is_retryable_because_another_tab_upgraded() {
        let error = classify("AbortError", "version change");
        assert!(error.is_retryable());
    }

    #[test]
    fn quota_leaves_the_app_read_only_not_broken() {
        assert_eq!(
            storage_mode_after(Some(&StoreError::QuotaExceeded)),
            StorageMode::ReadOnly
        );
        assert!(!StorageMode::ReadOnly.can_persist());
        assert_eq!(storage_mode_after(None), StorageMode::ReadWrite);
    }

    #[test]
    fn freeze_then_resume_requires_resync() {
        let visible = PageState::Visible;
        let hidden = visible.transition(Lifecycle::Background);
        assert_eq!(hidden, PageState::Hidden);
        assert!(!hidden.needs_resync(visible), "只是切后台，补发泵还在跑");
        let frozen = hidden.transition(Lifecycle::Frozen);
        assert_eq!(frozen, PageState::Frozen);
        assert!(!frozen.keeps_connection(), "冻结时不该假装还连着");
        let resumed = frozen.transition(Lifecycle::Resumed);
        assert!(resumed.needs_resync(frozen), "冻结恢复必须重新校验游标");
        assert!(!resumed.needs_resync(hidden), "普通回前台不需要重同步");
    }

    #[test]
    fn sensitive_cache_is_marked_by_prefix() {
        assert!(is_sensitive_cache("msg:conv-1"));
        assert!(is_sensitive_cache("token"));
        assert!(!is_sensitive_cache("avatar:u1"));
    }
}
