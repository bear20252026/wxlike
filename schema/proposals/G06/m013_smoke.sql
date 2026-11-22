TRUNCATE migration_ledger, migration_wallets;
INSERT INTO migration_wallets(user_id, balance_minor) VALUES ('u1', 100), ('u2', 50);
INSERT INTO migration_ledger(id, user_id, kind, amount_minor, counterparty, ref_id, memo, created_at)
VALUES
  (1, 'u1', 'fund_in', 150, '', 'r1', 'seed', 10),
  (2, 'u1', 'transfer_out', 50, 'u2', 'r2', 'pay', 20),
  (3, 'u2', 'transfer_in', 50, 'u1', 'r2', 'recv', 20);

CALL migrate_wallets_batch(10);

SELECT user_id, status, balance_minor, ledger_sum_minor FROM migration_wallets ORDER BY user_id;
SELECT user_id, balance FROM wallet_balances ORDER BY user_id;

SELECT j.id, j.kind, j.entry_count, count(e.id) AS entries, sum(e.amount_minor) AS total
FROM ledger_journals j
LEFT JOIN ledger_entries e ON e.journal_id = j.id
GROUP BY j.id, j.kind, j.entry_count
ORDER BY j.id;

CALL migrate_wallets_batch(10);
SELECT count(*) AS pending_after_second_run FROM migration_wallets WHERE status = 'pending';
SELECT count(*) AS journals_after_second_run FROM ledger_journals WHERE kind = 'migration.ledger';
