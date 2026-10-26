# wxlike-server image (WP-13) — the docker-compose development variant.
#
# The CI/release image is containers/server/Dockerfile (checked by
# containers/server/verify-image.sh and the `image` CI job); this root
# Dockerfile stays aligned with it:
#   * build stage pinned to the workspace toolchain image rust:1.97.1-bookworm;
#   * `cargo build --locked` — the image must never depend on lockfile drift;
#   * numeric non-root runtime user (10001), same as the verified image;
#   * HEALTHCHECK against the existing /health endpoint (the server also
#     serves /metrics; there is no /healthz — probe one of the real ones).
# Build includes the `redis` feature so compose can run the Streams backend.
FROM rust:1.97.1-bookworm AS build
WORKDIR /wxlike
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY server server
COPY scripts scripts
RUN cargo build --locked --release -p wxlike-server --features redis

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 wxlike \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin wxlike \
    && install -d -o 10001 -g 10001 -m 0750 /data /data/media
COPY --from=build /wxlike/target/release/wxlike-server /usr/local/bin/wxlike-server
ENV WXLIKE_DB=/data/wxlike.db \
    WXLIKE_MEDIA_DIR=/data/media \
    WXLIKE_HTTP=0.0.0.0:8099 \
    WXLIKE_QUIC=0.0.0.0:9443
VOLUME /data
USER 10001:10001
EXPOSE 8099 9443/udp
HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=6 \
    CMD ["curl", "-fsS", "http://127.0.0.1:8099/health"]
CMD ["wxlike-server"]
