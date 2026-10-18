//! P4 账本守恒验收（合同第五条 P4："账本守恒测试——100 并发转账分毫不差"）。

use std::sync::Arc;

use wxlike_ledger::{AccountType, Ledger};

const USER_A: &str = "wallet:alice";
const USER_B: &str = "wallet:bob";
const MARKETING: &str = "marketing:subsidy";
const EQUITY: &str = "revenue:capital";

#[test]
fn double_entry_conserves_zero_across_transfers() {
    let l = Ledger::open(None).unwrap();
    l.create_account(USER_A, AccountType::Wallet).unwrap();
    l.create_account(USER_B, AccountType::Wallet).unwrap();
    l.create_account(MARKETING, AccountType::Marketing).unwrap();
    l.create_account(EQUITY, AccountType::Revenue).unwrap();

    // 权益注资：equity → marketing（复式记账下每一分钱都有来源）
    l.transfer("j-equity", EQUITY, MARKETING, 1_000_000, "注资")
        .unwrap();
    // 平台补贴入账：marketing → alice（1000）
    l.transfer("j-fund-1", MARKETING, USER_A, 1_000, "补贴")
        .unwrap();
    assert_eq!(l.balance(USER_A).unwrap(), 1_000);
    assert!(l.total_sum_is_zero().unwrap(), "全账本借贷相抵");

    // 5 笔转账后守恒不变
    for i in 0..5 {
        l.transfer(&format!("j-t{i}"), USER_A, USER_B, 100, "转账")
            .unwrap();
    }
    assert_eq!(l.balance(USER_A).unwrap(), 500);
    assert_eq!(l.balance(USER_B).unwrap(), 500);
    assert!(l.total_sum_is_zero().unwrap());
}

#[test]
fn idempotent_journal_and_insufficient_funds() {
    let l = Ledger::open(None).unwrap();
    l.create_account(USER_A, AccountType::Wallet).unwrap();
    l.create_account(USER_B, AccountType::Wallet).unwrap();
    l.create_account(MARKETING, AccountType::Marketing).unwrap();
    l.create_account(EQUITY, AccountType::Revenue).unwrap();
    l.transfer("j-e", EQUITY, MARKETING, 1_000, "注资").unwrap();
    l.transfer("j-fund", MARKETING, USER_A, 100, "f").unwrap();

    // at-least-once 重投：同 journal 重放是 no-op（分毫不差的关键）
    l.transfer("j-fund", MARKETING, USER_A, 100, "f").unwrap();
    assert_eq!(l.balance(USER_A).unwrap(), 100);

    // 余额不足：拒绝且不留分录
    assert!(matches!(
        l.transfer("j-over", USER_A, USER_B, 999, "豪"),
        Err(wxlike_ledger::LedgerError::InsufficientFunds(..))
    ));
    assert_eq!(l.balance(USER_A).unwrap(), 100);
    assert_eq!(l.balance(USER_B).unwrap(), 0);
    assert!(l.total_sum_is_zero().unwrap());
}

#[test]
fn account_type_namespaces_and_validation() {
    let l = Ledger::open(None).unwrap();
    l.create_account("wallet:u", AccountType::Wallet).unwrap();
    l.create_account("advertiser:x", AccountType::Advertiser)
        .unwrap();
    l.create_account(EQUITY, AccountType::Revenue).unwrap();
    l.transfer("j-e", EQUITY, "wallet:u", 100, "注资").unwrap();

    // 同名重开不同类型 → 拒绝
    assert!(matches!(
        l.create_account("wallet:u", AccountType::Revenue),
        Err(wxlike_ledger::LedgerError::DuplicateAccount(..))
    ));

    // 未知账户转账 → 拒绝
    assert!(matches!(
        l.transfer("j-x", "wallet:ghost", "wallet:u", 1, ""),
        Err(wxlike_ledger::LedgerError::UnknownAccount(..))
    ));

    // 非正金额 → 拒绝
    assert!(matches!(
        l.transfer("j-y", "wallet:u", "advertiser:x", 0, ""),
        Err(wxlike_ledger::LedgerError::NonPositiveAmount)
    ));

    // 命名空间对账面：type_total 按类聚合
    l.transfer("j-z", "wallet:u", "advertiser:x", 10, "投放")
        .unwrap();
    assert_eq!(l.type_total(AccountType::Advertiser).unwrap(), 10);
    // wallet:u 收注资 100、投放 10 → 净 90（余额=重放分录）
    assert_eq!(l.type_total(AccountType::Wallet).unwrap(), 90);
}

#[test]
fn hundred_concurrent_transfers_conserve_to_the_cent() {
    // 合同验收：100 并发转账分毫不差
    let l = Ledger::open(None).unwrap();
    l.create_account(USER_A, AccountType::Wallet).unwrap();
    l.create_account(USER_B, AccountType::Wallet).unwrap();
    l.create_account(MARKETING, AccountType::Marketing).unwrap();
    l.create_account(EQUITY, AccountType::Revenue).unwrap();
    l.transfer("j-e", EQUITY, MARKETING, 1_000_000, "注资")
        .unwrap();
    l.transfer("j-seed", MARKETING, USER_A, 100_000, "种子")
        .unwrap();

    let l = Arc::new(l);
    let mut handles = Vec::new();
    for i in 0..100 {
        let l = l.clone();
        handles.push(std::thread::spawn(move || {
            // 每笔 97 分（质数，故意不对齐），100 笔合计 9700
            l.transfer(&format!("j-c{i}"), USER_A, USER_B, 97, &format!("并发{i}"))
        }));
    }
    let mut failed = 0;
    for h in handles {
        if h.join().is_err() {
            failed += 1;
        }
    }
    assert_eq!(failed, 0, "100 并发全部成功");

    assert_eq!(l.balance(USER_A).unwrap(), 100_000 - 9_700);
    assert_eq!(l.balance(USER_B).unwrap(), 9_700);
    // 守恒：转账只移动不增减
    assert_eq!(
        l.balance(USER_A).unwrap() + l.balance(USER_B).unwrap(),
        100_000
    );
    assert!(l.total_sum_is_zero().unwrap(), "全账本借贷相抵");
    // 命名空间对账：wallet 净得 100_000，marketing 出 100_000，equity 出注资
    assert_eq!(l.type_total(AccountType::Wallet).unwrap(), 100_000);
    assert_eq!(l.type_total(AccountType::Marketing).unwrap(), 900_000);
    assert_eq!(l.type_total(AccountType::Revenue).unwrap(), -1_000_000);
}
