# syntax=docker/dockerfile:1.7
# wxlike-live-ingest: the node that terminates a live source with FFmpeg and
# pushes MPEG-TS segments to wxlike-server.
#
# FFmpeg provenance is recorded in deploy/third-party/provenance.json and
# checked by ci/jobs/server/check_ffmpeg_notice.sh. The image keeps the exact
# version string under /opt/wxlike/ffmpeg-version.txt and the licence/source
# notice under /opt/wxlike/NOTICE so a running container can be audited.
#
# Build (repository root):
#   docker build -f containers/live-ingest/Dockerfile \
#     --build-arg GIT_REVISION=$(git rev-parse HEAD) -t wxlike-live-ingest:dev .

ARG RUST_IMAGE=rust:1.97.1-bookworm
FROM ${RUST_IMAGE} AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY server ./server
COPY scripts ./scripts
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p wxlike-video --bin wxlike-live-ingest && \
    cp target/release/wxlike-live-ingest /usr/local/bin/wxlike-live-ingest

FROM debian:bookworm-slim

ARG GIT_REVISION=unknown
ARG FFMPEG_EXPECTED_VERSION=""

LABEL org.opencontainers.image.title="wxlike-live-ingest" \
      org.opencontainers.image.description="Live source terminator (FFmpeg -> HLS segments)" \
      org.opencontainers.image.source="https://github.com/bear20252026/wxlike" \
      org.opencontainers.image.revision="${GIT_REVISION}" \
      org.opencontainers.image.licenses="AGPL-3.0-only AND LGPL-2.1-or-later"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 wxlike \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin wxlike \
    && install -d -o 10001 -g 10001 -m 0750 /work

COPY --from=build /usr/local/bin/wxlike-live-ingest /usr/local/bin/wxlike-live-ingest
COPY THIRD-PARTY-NOTICES.md /opt/wxlike/THIRD-PARTY-NOTICES.md

# Fail the build if the base image's FFmpeg is not the reviewed one. Empty
# means "record whatever Debian ships"; the release manifest records the exact
# version and image digest so the deployed artefact is still traceable.
RUN set -eux; \
    ffmpeg -version | head -n 1 > /opt/wxlike/ffmpeg-version.txt; \
    if [ -n "${FFMPEG_EXPECTED_VERSION}" ]; then \
      grep -F "${FFMPEG_EXPECTED_VERSION}" /opt/wxlike/ffmpeg-version.txt; \
    fi; \
    { \
      echo "FFmpeg: $(cat /opt/wxlike/ffmpeg-version.txt)"; \
      echo "Licence: LGPL-2.1-or-later (Debian builds without --enable-gpl)"; \
      echo "Source: https://ffmpeg.org/ and https://salsa.debian.org/multimedia-team/ffmpeg"; \
      echo "Full notices: /opt/wxlike/THIRD-PARTY-NOTICES.md"; \
    } > /opt/wxlike/NOTICE

USER 10001:10001
WORKDIR /work
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/wxlike-live-ingest"]
