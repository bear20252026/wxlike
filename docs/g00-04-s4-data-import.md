# G00-04 S4 — SQLite → PostgreSQL 数据导入工具链

状态（2026-11-27）：可重跑的导入工具链、M023 控制表与 M024 全量兼容补齐
已落地；**110 张 SQLite 表的 PG 形状普查已在本地 PostgreSQL 16.15 全绿**。
真实生产快照的行导入、生产停写窗口演练与生产 restore drill 仍未执行，
`G00-04-PG` 继续保持 open。

## 1. 交付边界

现有 `schema/proposals/G06/import_ledger_wallets.py` 只处理 M013 的钱包/账本
回填；它把 `wallets.balance` 与流水核对后生成 journal + 分录，不能当成通用
行复制器。S4 新增 `composition/data-import/`，负责其余平台的同名列复制：

- 只复制行，不推导账本、余额、消息序号等业务语义；
- `wallets` / `ledger` 默认跳过，继续交给 M013 导入器；
- `users` / `sessions` / `media` 是 M011/M012 的可写兼容视图，导入器按视图
  写入，不绕过 INSTEAD OF 触发器；
- 先在同一份 SQLite 快照上盘点、导出、回读校验，才允许进入停写窗口。

## 2. 流水线

| 阶段 | 文件 | 行为 |
| --- | --- | --- |
| 盘点 | `sqlite_source.py` | `mode=ro` 打开 SQLite；列出物理表；`PRAGMA table_xinfo` 排除生成列/虚拟表隐藏列；解析主键、外键；记录 `PRAGMA data_version` 与文件 SHA-256 |
| 计划 | `plan.py` | 按 SQLite 外键做 Kahn 拓扑排序，父表先于子表；有环时确定性断开并告警；`DEFAULT_SKIP` 跳过 `wallets`/`ledger`；`DEFAULT_TARGETS` 把 SQLite 的 `identity_session_token` 侧车映射到 PG 的 `identity_session_meta`；支持 mapping 的 `skip` 与 `targets` |
| 目标盘点 | `pg.py` | 通过 `psql` 读取 `pg_class`/`pg_attribute`/`pg_attrdef` 的 JSON catalog；识别表/视图/物化视图/分区表；复制任何行之前拒绝缺目标、物化视图、源独有列、目标独有 NOT NULL 无默认列、类型族不兼容 |
| 形状普查 | `snapshot.py` / `census.py` | 从 `crates/platform/tests/schema_snapshot.txt` 生成空 SQLite 形状库（110 表/704 列，无外键、无数据）；逐表跑 `apply_catalog`，输出 verified/failures JSON；失败表不进入行导入 |
| 导出 | `codec.py` / `exporter.py` | 流式 SELECT；NULL 写未加引号的 `\N`，空串写 `""`，bytea 写 `"\x..."`，所有非 NULL 字段加引号；逐行规范化哈希后排序再 SHA-256，得到顺序无关的行多重集摘要 |
| 装载 | `loader.py` | 先按依赖逆序清空未验证目标（子表先于父表，表和视图统一 `DELETE`），再 `COPY ... FROM STDIN WITH (FORMAT csv, HEADER false, NULL '\N')`；装载后对 identity/serial 列 `setval(max, true)`，空表 `setval(1, false)` |
| 校验 | `verifier.py` / `pg.py` | `COPY (SELECT ...) TO STDOUT` 流式回读；`csv.reader` 直接吃 raw pipe 并保留字段内 CR/LF；按目标类型解码 `\N`、空串、`\x`、`t/f`、数值；重建同一个 `RowDigest`，比对行数与摘要 |
| 状态/续跑 | `state.py` / `runner.py` | M023 `import_run` / `import_table` 记录源 SHA、data_version、逐表行数/摘要/状态；已验证表跳过；失败表置 `failed`，`--resume` 只重跑失败/未完成表；run 收口为 `verified`/`failed` |
| 停写窗口 | `window.py` | 记录源 SHA、SQLite data_version、逐表耗时、总耗时；前后 SHA 与 data_version 都不变才算 `quiescent`；产出 `window.json`；可设 `--max-window-seconds` 上限 |

CLI 入口是 `composition/data-import/import_all.py`：

```bash
python3 composition/data-import/import_all.py \
  --sqlite /path/to/app.db \
  --dsn "$DATABASE_URL" \
  --work-dir /path/to/import-work \
  --run-id cutover-20261126
```

`--plan-only` 只生成计划；`--resume` 复用原 `--work-dir` 和 `--run-id`；
`--verify-only` 只回读已有 CSV 并校验；`--allow-concurrent-writes` 关闭
`quiescent` 强制失败（仍记录测量值）。

形状普查独立于行导入，可对快照或真实 SQLite 运行：

```bash
PYTHONPATH=composition/data-import python3 composition/data-import/census.py \
  --snapshot crates/platform/tests/schema_snapshot.txt \
  --dsn "$DATABASE_URL" \
  --output /path/to/census.json
```

`--snapshot` 会先重建一个空形状库；`--sqlite` 可改为对真实快照只读盘点。
失败表按源表名列出，任何失败都会让进程退出 1。CI 在 M024 断言后跑
`ci/jobs/build/pg_data_import_census.sh`，把 108 个计划表（扣掉 M013 的
`wallets` / `ledger`）全部过一遍 target catalog。

## 3. M023 控制表

`schema/proposals/G00/0001_import_control.sql` 新增：

- `import_run`：源路径/SHA-256/data_version、开始/结束时间、`running/verified/failed/aborted`、`quiescent`、`window_ms`、notes；
- `import_table`：每表源/目标行数、双侧摘要、`pending/loading/loaded/verified/failed/skipped`、错误；
- 复合主键 `(run_id, source_table)`、`ON DELETE CASCADE`、状态 CHECK、状态/目标索引。

M023 已追加到 `schema/registry/migrations.json`，`next_id=M024`；
`crates/features/media/tests/migration_registry.rs` 的 `NEXT_ID` 同步为 M024。

M024（`schema/proposals/G00/0002_import_compat_backfill.sql`）是 110 表普查
发现的兼容补齐：

- `channel_role.role` 从 `BIGINT` 改为 SQLite 实际使用的 `TEXT` 角色名，补
  `granted_by` / `created_at` 与 `(user_id, channel_id)` 索引；
- 新增 `friend_tag`、`group_governance_audit`、`relations_audit` 三张关系域
  物理表，两张审计表的 `seq` 使用 identity；
- M014 保持不改，M024 只做前向修复；`next_id=M025`、`NEXT_ID=M025` 已同步。

## 4. 验证证据

本地 PostgreSQL 16.15 容器（`artex-pg`）实跑：

- 110 表形状普查：108 个计划表全部通过，0 失败；`wallets`/`ledger` 按
  M013 跳过；唯一 warning 是 `sessions.rowid` 由兼容视图默认值填充；
- M023 DDL 连跑两次幂等；`pg_migration_asserts_import.sql` 输出
  `M023 import control: OK`；
- 合成 SQLite 四表（父子外键、identity、空表）导入后 run 与四表全
  `verified`，NULL/空串分离，identity 下一个 id=3；
- 删除 child 行并把该表置 `failed`，`--resume` 恢复 child，父表不重跑；
- 额外的 bytea/CRLF/`numeric` 值表验证：`bytea` 保持 `00ff`，字段内
  `\r\n` 不被改写，`1.23`/`0.10` 摘要一致；
- 目标先塞入旧父子行，新 run 逆序清空后完整导入，证明非空目标也可重跑。

单元测试：

```bash
PYTHONPATH=composition/data-import \
python3 -m unittest discover -s composition/data-import/tests \
  -t composition/data-import
```

当前 55 个测试通过。CI 的 `pg-migration` 作业新增
`ci/jobs/build/pg_data_import_smoke.sh`，在 M023 断言之后跑：

- 合成五表 `ci_import_parent / ci_import_child / ci_import_serial /
  ci_import_empty / ci_import_values`；
- 先写入旧行，再验证清空、父子行数、空表、bytea/CRLF/`numeric` 保留；
- run 与每表 `verified`；
- 删除 child 行并置 `failed`，`--resume` 后恢复 child，identity 下一个 id=3。

`composition/data-import/**` 已加入 `ci/jobs/policy/paths.json` 的
`full_matrix_triggers`，导入器改动会触发完整矩阵，而不是只跑 Python 语法检查。

## 5. 未完成与边界

- **真实生产快照的行导入未执行**：形状普查已证明 110 张表的列/类型/目标
  对象在空库上兼容，但真实快照的行数、NULL 分布、目标触发器反馈、钱包
  对账仍必须在割接演练库上逐表跑；旧 `pg-table-map.json` 仍标记 stale。
- **M013 范围不变**：`wallets` / `ledger` 仍由 M013 导入器处理，新工具只负责
  其余同名列；余额守恒仍以 M013 的 strict 回填为准。
- **停写窗口只有源侧测量**：`window.json` 记录 SHA、data_version、逐表/总耗时，
  但没有独立的外部停写信号；窗口时长是否满足 G00-04 验收仍需 owner 评审。
- **生产 PG 版本/拓扑未裁决**：CI 与本地验证是 PostgreSQL 16；生产大版本、
  连接池与部署拓扑仍按 `schema/registry/pg-cutover.md` §S6 处理。
- **生产 restore drill 未执行**：CI 只证明 `pg_dump → psql restore` 在合成库
  上成立；生产快照演练仍需在割接窗口前完成。

## 6. 回滚

M023 是 expand-only：

```sql
DROP TABLE import_table;
DROP TABLE import_run;
```

导入本身是破坏性行复制：目标表在装载前会被清空。回滚数据必须依赖割接前的
PG 备份/快照，不能用本工具的 `--resume` 反向恢复。
