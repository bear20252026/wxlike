# H-43：restore drill 忽略媒体失败仍报 ok

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `e66c72d`。

审计原文：`AUDIT-60895e18.md:460-465`。`deploy/production/restore-drill.sh`
只有数据库 hash/count 校验；`tar -xzf ... || true` 把媒体解包失败吞掉；
manifest 声明了 `media` 也可以完全不存在；服务器 `kill`/`wait` 同样忽略结果。
最后无条件打印 `restore drill ok`。

## 修复

`deploy/production/restore-drill.sh`：

1. 从 manifest 读 `media.path/sha256/bytes`；声明了 media 时：
   - 路径必须是备份目录内的相对路径（拒绝 `..`/绝对路径）；
   - 文件必须存在，`sha256` 与 `bytes` 必须与 manifest 一致；
   - 解包失败立即 `exit 1`，不再有 `|| true`。
2. manifest 声明 `media: null` 时显式记 `media_verified=false`，不假装验过。
3. `drill.json` 新增 `media_verified` 与 `media_sha256`，让证据可审计。
4. 服务器启动后先等健康检查成功；失败时 `SIGKILL`、`wait` 回收并打印
   `server.log`。健康后再 `SIGTERM`，最多等 10 秒；仍未退出则 `SIGKILL` 并
   以失败结束，避免留下孤儿进程。`wait` 退出码只接受 0/137/143。
5. manifest 读取改用 Python `argv`，不再把路径拼进 Python 字符串。

## 证据

`python3 -m unittest ci.jobs.tests.test_restore_drill -v`：6 passed。

`ci/jobs/tests/test_restore_drill.py` 用 fake `sqlite3` 隔离数据库部分，覆盖：

- 正常 media：解包成功，`drill.json` 记录 `media_verified=true` 与归档 sha256；
- 归档被追加字节：sha256 mismatch，非零退出，不打印 ok；
- manifest 声明 media 但文件缺失：非零退出；
- 归档 hash/size 与 manifest 一致但不是 gzip：解包失败，非零退出；
- 无 media：成功，但 `media_verified=false`、`media_sha256=null`；
- 真实启动一个会响应 `/health` 的 fake server，验证 drill 能启动并回收它。

## 边界

- 校验到"归档级"：sha256 + 大小 + gzip/tar 解包完整性。manifest 没有逐对象
  hash，所以无法证明解包后的每个 blob 与源端逐字节一致；要补这一层需要
  backup.sh 在 manifest 里写对象清单。
- `kill -0` 的 10 秒等待在极慢 CI 上可能不够；当前失败是显式报错而不是静默
  继续，符合"不能假 ok"的口径。
