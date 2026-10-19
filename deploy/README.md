# 部署

三种形态，按场景选一种即可。三者跑的是**同一个二进制**，区别只在"谁拉起它、
数据放哪、日志去哪"。

| 形态 | 适用 | 入口 |
|---|---|---|
| **macOS 原生（launchd）** | Mac mini 自建、个人/小团队 | `deploy/macos/install.sh` |
| **Linux 原生（systemd）** | 云主机、NAS、树莓派 | `deploy/linux/install.sh` |
| **Docker Compose** | 想连 Redis 一起起、要可复制环境 | `docker-compose.yml` |

## Mac mini 上原生部署

```bash
xcode-select --install          # 一次性：Command Line Tools
curl https://sh.rustup.rs -sSf | sh
git clone https://github.com/bear20252026/wxlike && cd wxlike
./deploy/macos/install.sh
```

脚本做四件事：编译 → 装到 `~/.local/bin` → 渲染 launchd 配置到
`~/Library/LaunchAgents` → 启动并跑健康检查。**全程不需要 sudo**。

* 数据：`~/Library/Application Support/wxlike/`（`wxlike.db` + `media/`）
* 日志：`~/Library/Logs/wxlike/server.{out,err}.log`
* 重启：`launchctl kickstart -k gui/$UID/com.wxlike.server`
* 卸载：`./deploy/macos/install.sh --uninstall`（保留数据）

Apple Silicon 与 Intel 都是同一份源码编译；`str0m` 在 Unix 上用 **vendored
OpenSSL**，所以不需要 `brew install openssl`，代价是首次编译多几分钟。

## 服务化外壳（为什么它能被 launchd/systemd 托管）

业务逻辑跨平台不动，外壳只做四件事，全部在 `server/src/main.rs` 与
`server/src/lib.rs` 的 `Stack::shutdown` 里：

| 要求 | 实现 |
|---|---|
| 前台运行，不 fork | 直接 `await`；不写 PID 文件——supervisor 自己知道 PID |
| **SIGTERM 优雅关闭** | `tokio::signal` 同时接 SIGTERM/SIGINT（`launchctl bootout`/`systemctl stop`/`docker stop` 发的都是 SIGTERM） |
| 停止接新连接、排空在飞请求 | `axum::serve(...).with_graceful_shutdown(...)` + 上限 `WXLIKE_SHUTDOWN_GRACE`（默认 10s） |
| 数据干净落盘 | 关闭前 `PRAGMA wal_checkpoint(TRUNCATE)`，避免 `-wal` 留半截 |
| 日志 | 只写 stdout/stderr，由 launchd 的 `StandardOutPath` / systemd 的 journal 收集，不做自己的轮转 |
| 路径惯例 | `dirs` crate：macOS `~/Library/Application Support/wxlike`、Linux `~/.local/share/wxlike`、Windows `%APPDATA%\wxlike` |
| 退出码 | 正常停止 0、异常 1——`KeepAlive.SuccessfulExit=false` 与 `Restart=always` 靠它区分 |

**没有引 `daemon-kit` 之类的 daemon 库**：launchd/systemd 要的就是前台进程，
fork + PID 文件是 SysV 时代的做法；真正需要写的只有信号与排空，tokio 已经给了
原语。少一个 658 行、两个月、两千下载量的依赖，多一条真 SIGTERM 的测试。

### 自检

部署前先看解析结果，不绑端口、不写库：

```bash
wxlike-server --check
```

### 验收

`server/tests/graceful_shutdown.rs`：起**真进程**、发**真 SIGTERM**，断言退出码 0
且日志出现 `graceful shutdown 完成`。macOS job 也跑这条——不然"能在 Mac 上
优雅停止"只是一句声称。

## 端口

| 端口 | 协议 | 用途 |
|---|---|---|
| 8099 | TCP | 平台 HTTP API（`/health` `/endpoints` `/metrics` `/api/*`） |
| 9443 | UDP | QUIC hub（消息推送/同步） |
| 40000+ | UDP | SFU 媒体（`WXLIKE_SFU_PORT` 起的端口段） |

公网部署请在 8099 前面放反向代理（Caddy/Nginx）配 TLS；UDP 端口要直接放行，
反代不管 UDP。

## 配置

全部走环境变量，没有配置文件——容器、systemd、launchd 三处都能直接给：

```
WXLIKE_DB        平台 SQLite 路径（默认 data/wxlike.db，":memory:" 允许）
WXLIKE_MEDIA_DIR 媒体文件目录
WXLIKE_HTTP      HTTP 绑定（默认 0.0.0.0:8099）
WXLIKE_QUIC      QUIC 绑定（默认 0.0.0.0:9443）
WXLIKE_HUB_ID    hub 标识（多节点时区分）
WXLIKE_MQ        memory | redis
WXLIKE_REDIS_URL redis://127.0.0.1:6379
WXLIKE_LOG       日志过滤，如 "wxlike_server=debug,info"

# 通话相关
WXLIKE_STUN_URL     stun:host:3478
WXLIKE_TURN_URL     turn:host:3478 + WXLIKE_TURN_USER / WXLIKE_TURN_PASS
WXLIKE_SFU_ADVERTISE  对外宣告的媒体 IP（NAT 后面必须显式指定）
WXLIKE_SFU_PORT       SFU 端口段起点
```

## 备份

只有两个东西要备：

1. `wxlike.db`（SQLite，单文件；热备请用 `sqlite3 wxlike.db ".backup out.db"`）
2. `media/`（用户上传的附件）

```bash
# 每日 3 点备份，保留 14 天
0 3 * * * sqlite3 "$HOME/Library/Application Support/wxlike/wxlike.db" \
  ".backup $HOME/backups/wxlike-$(date +\%F).db"
```

## 多节点

`WXLIKE_MQ=redis` 时多个实例可通过 Redis Streams 互相路由（`WXLIKE_HUB_ID` 区分）。
SFU 目前是**一个房间落在一个节点**，跨节点级联还没做——见
`crates/sfu/src/lib.rs` 顶部的容量说明。
