# syntax=docker/dockerfile:1.7
# wxlike-admin (adminx) production image.
#
# Invariants enforced by containers/admin/verify-image.sh in CI:
#   * runtime user is numeric and non-root (10001);
#   * the process starts with a read-only root filesystem (only /data is a
#     volume, /tmp is a tmpfs);
#   * no secret value is baked into an image layer or ENV;
#   * /health is the liveness probe, SIGTERM is the graceful-stop signal.
#
# 与 containers/server/Dockerfile 同一套不变量，只有两处刻意不同：
#   * 构建/运行的是 wxlike-admin（独立进程，不随 server 起）；
#   * 不烘焙 WXLIKE_DB 与任何令牌：平台库路径由编排层给（adminx 只读打开），
#     令牌缺失时 adminx 拒绝启动，烘焙默认值等于把"必须显式配置"的纪律作废。
#
# Build from the repository root:
#   docker build -f containers/admin/Dockerfile \
#     --build-arg GIT_REVISION=$(git rev-parse HEAD) -t wxlike-admin:dev .

ARG RUST_IMAGE=rust:1.97.1-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97
FROM ${RUST_IMAGE} AS build

ARG GIT_REVISION=unknown
WORKDIR /src
ENV CARGO_NET_RETRY=5 \
    CARGO_TERM_COLOR=always

# workspace 成员（crates/* 与 server）必须齐全，否则 cargo 连清单都加载不了；
# schema 是 media 域 include_str! 的 DDL，缺了会在编译依赖链时报错。
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY server ./server
COPY scripts ./scripts
COPY schema ./schema

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p wxlike-admin && \
    cp target/release/wxlike-admin /usr/local/bin/wxlike-admin

FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3

ARG GIT_REVISION=unknown
ARG WXLIKE_VERSION=0.1.0

LABEL org.opencontainers.image.title="wxlike-admin" \
      org.opencontainers.image.description="wxlike operations console (adminx): read-only platform view, RBAC/MFA, audit" \
      org.opencontainers.image.source="https://github.com/bear20252026/wxlike" \
      org.opencontainers.image.revision="${GIT_REVISION}" \
      org.opencontainers.image.version="${WXLIKE_VERSION}" \
      org.opencontainers.image.licenses="AGPL-3.0-only"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl sqlite3 tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 wxlike \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin wxlike \
    && install -d -o 10001 -g 10001 -m 0750 /data

COPY --from=build /usr/local/bin/wxlike-admin /usr/local/bin/wxlike-admin

# 只烘焙非机密默认值：adminx 自己的账号库路径与日志级别。
# 监听地址刻意不烘焙：adminx 的默认是 127.0.0.1:8090（见 config.rs），
# 要对外由编排层显式注入 0.0.0.0:8090，镜像层不该悄悄改掉这条默认。
# WXLIKE_DB / WXLIKE_ADMIN_TOKEN / WXLIKE_ADMIN_PLATFORM_TOKEN 一律由编排层注入。
ENV WXLIKE_ADMIN_DB=/data/adminx.db \
    WXLIKE_ADMIN_LOG=info

USER 10001:10001
EXPOSE 8090
VOLUME ["/data"]

HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=6 \
    CMD ["curl", "-fsS", "http://127.0.0.1:8090/health"]

STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/wxlike-admin"]
