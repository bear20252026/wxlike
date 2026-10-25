# syntax=docker/dockerfile:1.7
# wxlike-server production image.
#
# Invariants enforced by containers/server/verify-image.sh in CI:
#   * runtime user is numeric and non-root (10001);
#   * the process starts with a read-only root filesystem (only /data is a
#     volume, /tmp is a tmpfs);
#   * no secret value is baked into an image layer or ENV;
#   * /health is the liveness probe, SIGTERM is the graceful-stop signal.
#
# Build from the repository root:
#   docker build -f containers/server/Dockerfile \
#     --build-arg GIT_REVISION=$(git rev-parse HEAD) -t wxlike-server:dev .

ARG RUST_IMAGE=rust:1.97.1-bookworm
FROM ${RUST_IMAGE} AS build

ARG GIT_REVISION=unknown
WORKDIR /src
ENV CARGO_NET_RETRY=5 \
    CARGO_TERM_COLOR=always

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY server ./server
COPY scripts ./scripts

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p wxlike-server --features redis && \
    cp target/release/wxlike-server /usr/local/bin/wxlike-server

FROM debian:bookworm-slim

ARG GIT_REVISION=unknown
ARG WXLIKE_VERSION=0.1.0

LABEL org.opencontainers.image.title="wxlike-server" \
      org.opencontainers.image.description="wxlike platform HTTP API and QUIC hub" \
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
    && install -d -o 10001 -g 10001 -m 0750 /data /data/media

COPY --from=build /usr/local/bin/wxlike-server /usr/local/bin/wxlike-server

ENV WXLIKE_DB=/data/wxlike.db \
    WXLIKE_MEDIA_DIR=/data/media \
    WXLIKE_HTTP=0.0.0.0:8099 \
    WXLIKE_QUIC=0.0.0.0:9443 \
    WXLIKE_SHUTDOWN_GRACE=25 \
    WXLIKE_LOG=info

USER 10001:10001
EXPOSE 8099 9443/udp
VOLUME ["/data"]

HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=6 \
    CMD ["curl", "-fsS", "http://127.0.0.1:8099/health"]

STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/wxlike-server"]
