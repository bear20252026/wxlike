//! G06-01 验收：不可变账本与资金账户。
//!
//! 钉住卡片里的四条：
//! * 禁止直接 UPDATE 余额——由数据库触发器拒绝，不是靠约定；
//! * `available`/`locked` 分开，余额由可审计分录与锁额导出；
//! * 虚拟演示资金与真实资金永不混记，跨币种拒绝隐式相加；
//! * 崩溃/重启后同幂等键不重复入账，日结余额与分录一致。
//!
//! 需要"数行数/改历史"这类越权动作的用例一律走文件库，并用**第二个裸
//! 连接**执行——这样才能证明兜底在数据库层，而不是在 Rust API 层。

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;
use wxlike_ledger::{
    AccountSpec, AccountType, Currency, FundClass, Ledger, Money, PostingLine, PostingRequest,
};

const ALICE: &str = "wallet:alice";
const BOB: &str = "wallet:bob";
const EQUITY: &str = "revenue:capital";

fn cny(minor: i64) -> Money {
    Money::from_minor(minor, Currency::CNY)
}

/// 注资一笔，让钱包有真实来源（复式记账不允许凭空造余额）。
fn fund(l: &Ledger, account: &str, minor: i64, key: &str) {
    l.transfer_money(key, EQUITY, account, cny(minor), "注资")
        .expect("fund");
}

/// 一个文件库账本；`conn()` 给出独立连接以模拟"绕过 API 的运维脚本"。
struct Db {
    dir: PathBuf,
}

impl Db {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("wxlike-ledger-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self { dir }
    }

    fn path(&self) -> String {
        self.dir
            .join("ledger.db")
            .to_str()
            .expect("utf-8 path")
            .to_string()
    }

    /// 开一个带标准三账户的账本（重复调用即"进程重启"）。
    fn open(&self) -> Ledger {
        let l = Ledger::open(Some(&self.path())).expect("open ledger");
        l.create_account(EQUITY, AccountType::Revenue)
            .expect("equity");
        l.create_account(ALICE, AccountType::Wallet).expect("alice");
        l.create_account(BOB, AccountType::Wallet).expect("bob");
        l
    }

    /// 独立连接：不带任何领域校验，专门用来试探数据库底线。
    fn conn(&self) -> Connection {
        wxlike_ledger::open_connection(Some(&self.path())).expect("conn")
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn()
            .query_row(sql, [], |r| r.get::<_, i64>(0))
            .expect("count")
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn direct_balance_tampering_is_refused_by_the_database() {
    let db = Db::new("tamper");
    {
        let l = db.open();
        fund(&l, ALICE, 1_000, "j-fund");
        assert_eq!(l.balance(ALICE).expect("balance"), 1_000);
        // 先留下一张日结快照，否则"改历史快照"那几条语句会因为没有命中行
        // 而静默通过，触发器根本不会被执行。
        l.close_range("2026-11-05", 0, i64::MAX).expect("close");
    }

    // 没有余额列可写；绕过 post_in 直接改历史分录/日结会被数据库拒绝。
    for statement in [
        "UPDATE ledger_entries SET amount_minor = 999999 WHERE id = 1",
        "UPDATE ledger_entries SET account = 'wallet:bob' WHERE id = 1",
        "DELETE FROM ledger_entries WHERE id = 1",
        "UPDATE ledger_journals SET business_key = 'tampered'",
        "DELETE FROM ledger_journals",
        "UPDATE ledger_closings SET credit_minor = 0 WHERE day = '2026-11-05'",
        "DELETE FROM ledger_closings",
        "UPDATE ledger_closing_balances SET balance_minor = 0 WHERE day = '2026-11-05'",
    ] {
        let conn = db.conn();
        let error = conn
            .execute_batch(statement)
            .expect_err("tampering statement must be refused");
        let text = error.to_string();
        assert!(
            text.contains("append-only") || text.contains("immutable snapshot"),
            "unexpected error for {statement}: {text}"
        );
    }

    // 拒绝即整体回滚：余额、分录数、日结快照、守恒都不变。
    assert_eq!(db.count("SELECT COUNT(*) FROM ledger_entries"), 2);
    assert_eq!(db.count("SELECT COUNT(*) FROM ledger_journals"), 1);
    assert_eq!(db.count("SELECT COUNT(*) FROM ledger_closings"), 1);
    let l = db.open();
    assert_eq!(l.balance(ALICE).expect("balance"), 1_000);
    l.verify_journals().expect("balanced");
    l.verify_closing("2026-11-05").expect("snapshot intact");
    assert!(l.total_sum_is_zero().expect("conserved"));
}

#[test]
fn balance_is_derived_from_entries_and_holds() {
    let l = Ledger::open(None).expect("ledger");
    l.create_account(EQUITY, AccountType::Revenue)
        .expect("equity");
    l.create_account(ALICE, AccountType::Wallet).expect("alice");
    l.create_account(BOB, AccountType::Wallet).expect("bob");
    fund(&l, ALICE, 1_000, "j-fund");

    // 锁额不是分录：total 不变，available 减少，locked 上升。
    let hold = l.hold("h-1", ALICE, cny(400)).expect("hold");
    assert_eq!(hold.amount, cny(400));
    assert_eq!(l.money_balance(ALICE).expect("total"), cny(1_000));
    assert_eq!(l.available(ALICE).expect("available"), cny(600));
    assert_eq!(l.locked(ALICE).expect("locked"), cny(400));
    assert_eq!(
        l.statement(ALICE, None, 50).expect("statement").items.len(),
        1,
        "冻结不产生分录"
    );

    // 同一锁额键重放不重复扣可用额度。
    let again = l.hold("h-1", ALICE, cny(400)).expect("idempotent hold");
    assert_eq!(again, hold);
    assert_eq!(l.available(ALICE).expect("available"), cny(600));

    // 超过可用额度的冻结直接拒绝（与"并发扣款不透支"同一条底线）。
    assert!(matches!(
        l.hold("h-2", ALICE, cny(601)),
        Err(wxlike_ledger::LedgerError::InsufficientFunds(..))
    ));

    l.release_hold("h-1").expect("release");
    assert_eq!(l.available(ALICE).expect("available"), cny(1_000));
    assert_eq!(l.locked(ALICE).expect("locked"), cny(0));
    assert_eq!(l.money_balance(ALICE).expect("total"), cny(1_000));
    assert!(l.total_sum_is_zero().expect("conserved"));
}

#[test]
fn virtual_and_real_funds_never_mix_in_one_journal() {
    let l = Ledger::open(None).expect("ledger");
    l.create_account_with(&AccountSpec::new(
        EQUITY,
        AccountType::Revenue,
        Currency::CNY,
    ))
    .expect("equity");
    l.create_account_with(
        &AccountSpec::new(ALICE, AccountType::Wallet, Currency::CNY)
            .with_fund_class(FundClass::Virtual),
    )
    .expect("virtual wallet");
    l.create_account_with(
        &AccountSpec::new(BOB, AccountType::Wallet, Currency::CNY).with_fund_class(FundClass::Real),
    )
    .expect("real wallet");

    let error = l
        .post(&PostingRequest::new(
            "j-mix",
            "topup",
            Currency::CNY,
            vec![
                PostingLine::debit(ALICE, 100),
                PostingLine::credit(BOB, 100),
            ],
        ))
        .expect_err("演示资金不能与真实资金同笔记账");
    assert!(matches!(error, wxlike_ledger::LedgerError::MixedFundClass));
    assert_eq!(l.balance(BOB).expect("bob"), 0);
    assert!(l.total_sum_is_zero().expect("conserved"));

    // 同名账户重开时改资金性质也属于异参：拒绝，不改目录。
    assert!(matches!(
        l.create_account_with(
            &AccountSpec::new(ALICE, AccountType::Wallet, Currency::CNY)
                .with_fund_class(FundClass::Real)
        ),
        Err(wxlike_ledger::LedgerError::DuplicateAccount(..))
    ));
    assert_eq!(
        l.account(ALICE)
            .expect("account")
            .expect("exists")
            .fund_class,
        FundClass::Virtual
    );
}

#[test]
fn cross_currency_totals_refuse_implicit_summation() {
    let usd = Currency::parse("USD").expect("usd");
    let l = Ledger::open(None).expect("ledger");
    l.create_account_with(&AccountSpec::new(
        EQUITY,
        AccountType::Revenue,
        Currency::CNY,
    ))
    .expect("equity cny");
    l.create_account_with(&AccountSpec::new(
        "revenue:capital:usd",
        AccountType::Revenue,
        usd,
    ))
    .expect("equity usd");
    l.create_account_with(&AccountSpec::new(ALICE, AccountType::Wallet, Currency::CNY))
        .expect("alice cny");
    l.create_account_with(&AccountSpec::new(
        "wallet:alice:usd",
        AccountType::Wallet,
        usd,
    ))
    .expect("alice usd");

    l.transfer_money("j-cny", EQUITY, ALICE, cny(100), "人民币")
        .expect("cny");
    l.transfer_money(
        "j-usd",
        "revenue:capital:usd",
        "wallet:alice:usd",
        Money::from_minor(200, usd),
        "美元",
    )
    .expect("usd");

    // 同一类别跨币种：不给一个"加起来"的数字，直接拒绝。
    assert!(matches!(
        l.type_total(AccountType::Wallet),
        Err(wxlike_ledger::LedgerError::MixedCurrency(..))
    ));
    assert!(matches!(
        l.type_total(AccountType::Revenue),
        Err(wxlike_ledger::LedgerError::MixedCurrency(..))
    ));
    // 分币种各自守恒仍然成立。
    assert!(l.total_sum_is_zero().expect("conserved"));
    assert_eq!(l.balance(ALICE).expect("cny"), 100);
    assert_eq!(l.balance("wallet:alice:usd").expect("usd"), 200);

    // 跨币种转账本身也走不通：意图币种与账户币种必须一致。
    assert!(matches!(
        l.transfer_money(
            "j-x",
            ALICE,
            "wallet:alice:usd",
            Money::from_minor(1, usd),
            "换汇"
        ),
        Err(wxlike_ledger::LedgerError::CurrencyMismatch { .. })
    ));
}

#[test]
fn crash_restart_and_replay_keep_a_single_journal() {
    let db = Db::new("restart");
    let req = PostingRequest::new(
        "j-restart",
        "transfer",
        Currency::CNY,
        vec![
            PostingLine::debit(ALICE, 300),
            PostingLine::credit(BOB, 300),
        ],
    )
    .with_memo("重启前后同一笔");

    let expected = {
        let l = db.open();
        fund(&l, ALICE, 1_000, "j-fund");
        let tx = l.post(&req).expect("post");
        assert_eq!(l.balance(ALICE).expect("alice"), 700);
        drop(l); // 进程在这里"崩溃"
        tx
    };

    // 新进程、新连接、同一文件：分录还在，重放同键不再产生第二笔。
    let l = db.open();
    assert_eq!(
        l.balance(ALICE).expect("alice"),
        700,
        "余额由磁盘上的分录导出"
    );
    let replay = l.post(&req).expect("replay after restart");
    assert_eq!(replay, expected);
    l.post(&req).expect("third attempt");
    assert_eq!(
        db.count("SELECT COUNT(*) FROM ledger_journals"),
        2,
        "注资 + 转账，重放不新增"
    );
    assert_eq!(db.count("SELECT COUNT(*) FROM ledger_entries"), 4);

    // 异参重放同键：拒绝，磁盘不变。
    let changed = PostingRequest::new(
        "j-restart",
        "transfer",
        Currency::CNY,
        vec![
            PostingLine::debit(ALICE, 301),
            PostingLine::credit(BOB, 301),
        ],
    );
    assert!(matches!(
        l.post(&changed),
        Err(wxlike_ledger::LedgerError::IdempotencyConflict)
    ));
    assert_eq!(l.balance(ALICE).expect("alice"), 700);
    assert_eq!(db.count("SELECT COUNT(*) FROM ledger_journals"), 2);
    l.verify_journals().expect("balanced");
    assert!(l.total_sum_is_zero().expect("conserved"));
}

#[test]
fn concurrent_debits_never_overdraw() {
    let l = Arc::new(Ledger::open(None).expect("ledger"));
    l.create_account(EQUITY, AccountType::Revenue)
        .expect("equity");
    l.create_account(ALICE, AccountType::Wallet).expect("alice");
    l.create_account(BOB, AccountType::Wallet).expect("bob");
    fund(&l, ALICE, 1_000, "j-fund");

    // 20 个并发各扣 100，只有 10 笔能成功；第 11 笔必须看见不足而不是透支。
    let winners = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..20)
            .map(|index| {
                let ledger = Arc::clone(&l);
                scope.spawn(move || {
                    ledger
                        .transfer(&format!("j-race-{index}"), ALICE, BOB, 100, "抢扣")
                        .is_ok()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .filter(|ok| *ok)
            .count()
    });
    assert_eq!(winners, 10, "恰好放行可用额度内的笔数");
    assert_eq!(l.balance(ALICE).expect("alice"), 0);
    assert_eq!(l.balance(BOB).expect("bob"), 1_000);
    assert!(l.total_sum_is_zero().expect("conserved"));
    l.verify_journals().expect("balanced");
}

#[test]
fn daily_closing_matches_entries_and_snapshot_is_immutable() {
    let db = Db::new("closing");
    let l = db.open();
    let start = 1_000;
    let end = 2_000;
    l.post_at(
        &PostingRequest::new(
            "j-day-1",
            "topup",
            Currency::CNY,
            vec![
                PostingLine::debit(EQUITY, 500),
                PostingLine::credit(ALICE, 500),
            ],
        ),
        1_500,
    )
    .expect("in-window");
    l.post_at(
        &PostingRequest::new(
            "j-day-2",
            "transfer",
            Currency::CNY,
            vec![
                PostingLine::debit(ALICE, 200),
                PostingLine::credit(BOB, 200),
            ],
        ),
        2_500,
    )
    .expect("next-window");

    let reports = l.close_range("2026-11-05", start, end).expect("close");
    assert_eq!(reports.len(), 1, "只有 CNY 一个币种");
    let report = &reports[0];
    assert_eq!(report.debits, 500);
    assert_eq!(report.credits, 500);
    assert_eq!(report.entry_count, 2, "窗口外的分录不能算进来");
    assert_eq!(report.balance_total, 0, "复式账本日终必为 0");
    assert_eq!(report.locked, 0);
    l.verify_closing("2026-11-05").expect("快照仍与分录一致");

    // 把两笔都覆盖进去再日结一次：此时快照余额必须逐一等于当前余额，
    // 这就是"日结余额与分录一致"的可执行定义。
    let covered = l
        .close_range("2026-11-06", 1_000, 3_000)
        .expect("close all");
    assert_eq!(covered[0].entry_count, 4);
    assert_eq!(covered[0].debits, 700);
    assert_eq!(covered[0].credits, 700);
    let balances = l
        .closing_balances("2026-11-06", Currency::CNY)
        .expect("balances");
    assert_eq!(balances.len(), 3);
    for row in &balances {
        assert_eq!(
            row.balance_minor,
            l.balance(&row.account).expect("live balance"),
            "{} 的日结余额与当前余额不一致",
            row.account
        );
    }
    // 前一次日结是"截至日终"的独立快照，不该被后一次日结改写。
    let earlier = l
        .closing_balances("2026-11-05", Currency::CNY)
        .expect("earlier balances");
    let alice = earlier
        .iter()
        .find(|row| row.account == ALICE)
        .expect("alice row");
    assert_eq!(alice.balance_minor, 500, "旧快照必须保持当时的日终值");
    assert!(
        l.closing_balances("2026-11-07", Currency::CNY)
            .expect("empty")
            .is_empty(),
        "没日结的日子不该有快照"
    );

    // 快照不可改写；重读日结报告仍是当初那一版。
    let conn = db.conn();
    let error = conn
        .execute_batch("UPDATE ledger_closings SET entry_count = 99 WHERE day = '2026-11-05'")
        .expect_err("快照必须不可改写");
    assert!(error.to_string().contains("immutable snapshot"));
    let again = l.closings("2026-11-05").expect("read back");
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].entry_count, 2);
    assert_eq!(again[0].checksum, report.checksum);
}
