#!/usr/bin/env bash
# Verify the FFmpeg/tini/sqlite3 provenance written into the live-ingest image.
#
#   ./ci/jobs/server/check_ffmpeg_notice.sh wxlike-live-ingest:test
set -euo pipefail

IMAGE="${1:?usage: check_ffmpeg_notice.sh <image>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
fail() { printf 'ERROR %s\n' "$*" >&2; exit 1; }

docker image inspect "$IMAGE" >/dev/null 2>&1 || fail "image $IMAGE not found"

version="$(docker run --rm --entrypoint /bin/sh "$IMAGE" -c 'cat /opt/wxlike/ffmpeg-version.txt')"
notice="$(docker run --rm --entrypoint /bin/sh "$IMAGE" -c 'cat /opt/wxlike/NOTICE')"
expected="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(next(p["expected_version"] for p in d["packages"] if p["name"]=="ffmpeg"))' "$ROOT/deploy/third-party/provenance.json")"

echo "[ffmpeg] $version"
[[ "$version" == ffmpeg\ version* ]] || fail "unexpected ffmpeg version line: $version"
if [[ -n "$expected" ]]; then
  grep -F "$expected" <<<"$version" >/dev/null || fail "ffmpeg version $version does not match expected $expected"
fi

grep -F "FFmpeg" <<<"$notice" >/dev/null || fail "/opt/wxlike/NOTICE does not mention FFmpeg"
grep -Ei "LGPL|GPL" <<<"$notice" >/dev/null || fail "/opt/wxlike/NOTICE does not state a licence"
grep -F "ffmpeg.org" <<<"$notice" >/dev/null || fail "/opt/wxlike/NOTICE does not name the source"
docker run --rm --entrypoint /bin/sh "$IMAGE" -c 'test -s /opt/wxlike/THIRD-PARTY-NOTICES.md' \
  || fail "full THIRD-PARTY-NOTICES.md is missing from the image"
docker run --rm --entrypoint /bin/sh "$IMAGE" -c 'command -v tini >/dev/null && command -v ffmpeg >/dev/null' \
  || fail "tini/ffmpeg missing from runtime image"
echo "[ffmpeg] notice and provenance checks passed"
