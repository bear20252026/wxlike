# 本地开发环境

本地用仓库根目录的 `docker-compose.yml`（Redis + server + MinIO），不要在这里再造一份：

```bash
cp deploy/dev/wxlike.env.example .env      # 仅第一次
docker compose up -d --build
curl --noproxy '*' http://127.0.0.1:8099/health
```

## 与生产镜像的区别

| 项 | 本地开发 | 预发/生产 |
|---|---|---|
| 镜像 | 根 `Dockerfile` / `containers/server/Dockerfile` 均可 | `containers/server/compose.prod.yml`，只接受已签名 digest |
| 根文件系统 | 可写 | 只读 + tmpfs `/tmp` |
| 用户 | 容器默认 | 10001:10001，`cap_drop: ALL` |
| 密钥 | `WXLIKE_DEV_INSECURE=1` 随机生成 | 必须显式注入，缺密钥拒绝启动 |
| 数据库 | 容器内 SQLite | 目前仍是 SQLite，PG 割接为 G00-04 未闭环缺口 |

## 重置

```bash
docker compose down -v          # 删除 server-data / redis-data / minio-data
```

`deploy/dev/wxlike.env.example` 只包含开发值。任何以 `CHANGE_ME` 开头的值在
`containers/server/verify-image.sh` 的配置检查里会被拒绝用于生产。
