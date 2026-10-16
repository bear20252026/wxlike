# wxlike-server image (WP-13).
# Build includes the `redis` feature so compose can run the Streams backend.
FROM rust:1-bookworm AS build
WORKDIR /wxlike
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY server server
COPY scripts scripts
RUN cargo build --release -p wxlike-server --features redis

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /wxlike/target/release/wxlike-server /usr/local/bin/wxlike-server
ENV WXLIKE_DB=/data/wxlike.db \
    WXLIKE_MEDIA_DIR=/data/media
VOLUME /data
EXPOSE 8099 9443/udp
CMD ["wxlike-server"]
