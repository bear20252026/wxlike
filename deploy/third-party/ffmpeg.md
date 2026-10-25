# Third-party binaries and the notice chain

The runtime images install three OS packages that are not Rust crates:
`ffmpeg`, `tini` and `sqlite3`. They are not covered by `cargo deny`, so they
need their own provenance record.

| Package | Why | Licence | Provenance |
|---|---|---|---|
| FFmpeg | live source termination for `wxlike-live-ingest` | LGPL-2.1-or-later (Debian build, no `--enable-gpl`) | Debian bookworm apt, version recorded in the image |
| tini | PID 1 signal forwarding | MIT | Debian bookworm apt |
| sqlite3 | hot backup in the restore drill | Public Domain | Debian bookworm apt |

## What "reproducible" means here

We do not pretend a floating `debian:bookworm-slim` tag is reproducible. The
release workflow records, for every published image:

1. the image digest (`@sha256:…`), which is what production runs;
2. the exact `ffmpeg -version` line from inside that image;
3. an SBOM of the image;
4. the signature/attestation over the digest.

`containers/live-ingest/Dockerfile` accepts `FFMPEG_EXPECTED_VERSION`. Set it
once the version in `deploy/third-party/provenance.json` is reviewed; a base
image upgrade that silently changes FFmpeg then fails the build instead of
shipping a different encoder. Until that value is filled in, the version is
still recorded and surfaced by `ci/jobs/server/check_ffmpeg_notice.sh`, but the
build is not pinned to a specific package version.

The full text notices remain in [`THIRD-PARTY-NOTICES.md`](../../THIRD-PARTY-NOTICES.md).
The image also carries a copy at `/opt/wxlike/THIRD-PARTY-NOTICES.md` and a
short `/opt/wxlike/NOTICE` so an operator can audit a running container without
the repository.
