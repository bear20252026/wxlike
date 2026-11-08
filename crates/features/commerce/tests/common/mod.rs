//! G06 集成测试夹具：真实 SQLite + 真实领域实现 + 沙箱供应商。

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rusqlite::params as sqlparams;
use wxlike_commerce::infrastructure::sandbox::{SandboxHandle, SandboxProvider};
use wxlike_commerce::testkit::{FixedEntropy, ManualClock, StaticMembership};
use wxlike_commerce::{Commerce, CommerceConfig, Money, OsEntropy, Ports};
use wxlike_ledger::Currency;

type FilePorts = (
    Arc<Commerce>,
    SandboxHandle,
    Arc<ManualClock>,
    Arc<StaticMembership>,
);

pub struct Harness {
    pub commerce: Arc<Commerce>,
    pub handle: SandboxHandle,
    pub clock: Arc<ManualClock>,
    pub membership: Arc<StaticMembership>,
}

pub fn harness() -> Harness {
    let provider = SandboxProvider::new();
    let handle = provider.handle();
    let clock = Arc::new(ManualClock::new(1_700_000_000_000));
    let membership = Arc::new(StaticMembership::new());
    let ports = Ports::new(Arc::new(provider), membership.clone())
        .with_clock_entropy(clock.clone(), Arc::new(FixedEntropy::default()));
    let commerce = Commerce::in_memory(CommerceConfig::sandbox(b"webhook-secret".to_vec()), ports)
        .expect("commerce");
    Harness {
        commerce: Arc::new(commerce),
        handle,
        clock,
        membership,
    }
}

pub fn cny(minor: i64) -> Money {
    Money::from_minor(minor, Currency::CNY)
}

pub fn fund(h: &Harness, user: &str, minor: i64) {
    h.commerce
        .sandbox_credit(user, cny(minor), &format!("fund:{user}:{minor}"))
        .expect("sandbox credit");
}

/// 任意 `Commerce`（含文件库夹具）的演示资金注入。
///
/// 这是一条**沙箱**通道：只用来给账户建立来源资金，不代表真实供应商入账。
pub fn credit(commerce: &Commerce, user: &str, minor: i64, key: &str) {
    commerce
        .sandbox_credit(user, cny(minor), key)
        .expect("sandbox credit");
}

#[allow(clippy::too_many_arguments)]
pub fn webhook_body(
    event_id: &str,
    intent_id: &str,
    kind: &str,
    provider_ref: &str,
    amount_minor: i64,
    timestamp_ms: i64,
) -> String {
    format!(
        "{{\"event_id\":\"{event_id}\",\"intent_id\":\"{intent_id}\",\"kind\":\"{kind}\",\"provider_ref\":\"{provider_ref}\",\"amount_minor\":{amount_minor},\"currency\":\"CNY\",\"timestamp_ms\":{timestamp_ms}}}"
    )
}

/// 带 `operation_id` 的回调信封（`POST /api/payment/callback` 的契约体）。
#[allow(clippy::too_many_arguments)]
pub fn callback_body(
    operation_id: &str,
    event_id: &str,
    intent_id: &str,
    kind: &str,
    provider_ref: &str,
    amount_minor: i64,
    timestamp_ms: i64,
) -> String {
    format!(
        "{{\"operation_id\":\"{operation_id}\",\"event_id\":\"{event_id}\",\"intent_id\":\"{intent_id}\",\"kind\":\"{kind}\",\"provider_ref\":\"{provider_ref}\",\"amount_minor\":{amount_minor},\"currency\":\"CNY\",\"timestamp_ms\":{timestamp_ms}}}"
    )
}

/// 文件库夹具：回调重放/崩溃恢复的用例必须能证明台账与分录真的落盘，
/// 所以这里刻意不走内存库。
///
/// 供应商实现仍是沙箱（唯一允许注入故障的外部依赖），账本、幂等、签名与
/// 状态机全部是本 crate 的真实实现；**这不构成真实供应商通道证据**，
/// 真凭据回调需要 G00 挂载 live provider 后再取证。
pub struct FileHarness {
    pub dir: std::path::PathBuf,
    pub commerce: Arc<Commerce>,
    pub handle: SandboxHandle,
    pub clock: Arc<ManualClock>,
    pub membership: Arc<StaticMembership>,
}

static FILE_SEQ: AtomicU64 = AtomicU64::new(0);

impl FileHarness {
    pub fn new(tag: &str) -> Self {
        let seq = FILE_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("wxlike-g06-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (commerce, handle, clock, membership) = Self::open_at(&dir.join("commerce.db"));
        Self {
            dir,
            commerce,
            handle,
            clock,
            membership,
        }
    }

    /// 在同一个文件上开一套全新端口——重开即"进程重启"。
    ///
    /// 熵源用 `OsEntropy`：重启后若沿用确定性计数器，会和上一进程写入的
    /// 主键相撞，那种"全绿"才是假的。
    fn open_at(path: &std::path::Path) -> FilePorts {
        let provider = SandboxProvider::new();
        let handle = provider.handle();
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        let membership = Arc::new(StaticMembership::new());
        let ports = Ports::new(Arc::new(provider), membership.clone())
            .with_clock_entropy(clock.clone(), Arc::new(OsEntropy));
        let commerce = Commerce::open(
            path.to_str(),
            CommerceConfig::sandbox(b"webhook-secret".to_vec()),
            ports,
        )
        .expect("open file commerce");
        (Arc::new(commerce), handle, clock, membership)
    }

    pub fn path(&self) -> std::path::PathBuf {
        self.dir.join("commerce.db")
    }

    /// 模拟崩溃后重启：在同一文件上重开，随后释放旧连接。
    ///
    /// 幂等台账若只活在内存里，这一步就会立刻暴露。
    pub fn restart(&mut self) {
        let path = self.path();
        let (commerce, handle, clock, membership) = Self::open_at(&path);
        let previous = std::mem::replace(&mut self.commerce, commerce);
        drop(previous);
        self.handle = handle;
        self.clock = clock;
        self.membership = membership;
    }

    /// 独立连接上的裸 SQL 计数（带一个绑定参数）：单一效果的外部证据。
    pub fn count(&self, sql: &str, param: &str) -> i64 {
        let conn = wxlike_ledger::open_connection(self.path().to_str()).expect("read conn");
        conn.query_row(sql, sqlparams![param], |r| r.get::<_, i64>(0))
            .expect("count")
    }

    /// 独立连接上的裸 SQL 计数（无参，表级体检）。
    pub fn count_all(&self, sql: &str) -> i64 {
        let conn = wxlike_ledger::open_connection(self.path().to_str()).expect("read conn");
        conn.query_row(sql, [], |r| r.get::<_, i64>(0))
            .expect("count")
    }
}

impl Drop for FileHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
