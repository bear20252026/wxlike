-- G14 migration 0001 的回滚草稿。
--
-- 兼容性说明（随 PR 一起提交，G00 复核）：
--   * 0001 只 `CREATE TABLE/INDEX/TRIGGER`，**没有** `ALTER` 任何既有表，
--     所以回滚只删自己建的对象；admin_accounts / admin_sessions / admin_oplog
--     不动，旧版本（D1/D2/D3 时代）的 adminx 可在同一份库上直接启动。
--   * 回滚会丢数据：g14_audit 一旦存在就是追责链，删表等于销毁证据。
--     生产环境回滚前必须先 `VACUUM INTO` 一份快照并归档（见 docs/g14-delivery.md）。
--   * 顺序：先删触发器再删表，否则 SQLite 会因触发器引用的表消失而报错。
--   * 迁移前必须停写（adminx 是单写者，WAL 下换 schema 会与在途事务冲突）。

DROP TRIGGER IF EXISTS g14_audit_no_update;
DROP TRIGGER IF EXISTS g14_audit_no_delete;
DROP TRIGGER IF EXISTS g14_journal_no_update;

DROP TABLE IF EXISTS g14_privacy_request;
DROP TABLE IF EXISTS g14_job;
DROP TABLE IF EXISTS g14_recon_diff;
DROP TABLE IF EXISTS g14_journal;
DROP TABLE IF EXISTS g14_refund;
DROP TABLE IF EXISTS g14_order;
DROP TABLE IF EXISTS g14_card_redeem;
DROP TABLE IF EXISTS g14_card;
DROP TABLE IF EXISTS g14_merchant;
DROP TABLE IF EXISTS g14_mini_release;
DROP TABLE IF EXISTS g14_service_config;
DROP TABLE IF EXISTS g14_review_item;
DROP TABLE IF EXISTS g14_case;
DROP TABLE IF EXISTS g14_stepup;
DROP TABLE IF EXISTS g14_audit;

PRAGMA user_version = 0;
