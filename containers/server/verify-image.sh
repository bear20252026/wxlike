#!/usr/bin/env bash
# Verify an image before it is allowed to be published.
#
#   ./containers/server/verify-image.sh wxlike-server:test
#
# Requires a running Docker daemon. Checks are deliberately observable rather
# than "trust the Dockerfile": the Dockerfile can be edited, the image cannot
# lie about its Config.User.
set -euo pipefail

IMAGE="${1:?usage: verify-image.sh <image>}"
fail() { printf 'ERROR %s\n' "$*" >&2; exit 1; }
say() { printf '[verify-image] %s\n' "$*"; }

command -v docker >/dev/null || fail "docker is not installed"
docker image inspect "$IMAGE" >/dev/null 2>&1 || fail "image $IMAGE not found locally"

user="$(docker image inspect -f '{{.Config.User}}' "$IMAGE")"
[[ -n "$user" ]] || fail "Config.User is empty (image would run as root)"
case "$user" in
  0|0:*|root|root:*) fail "Config.User is root: $user" ;;
esac
[[ "$user" == 10001:10001 ]] || fail "expected numeric user 10001:10001, got $user"
say "non-root user: $user"

for key in org.opencontainers.image.source org.opencontainers.image.revision \
           org.opencontainers.image.version org.opencontainers.image.licenses; do
  value="$(docker image inspect -f "{{index .Config.Labels \"$key\"}}" "$IMAGE")"
  [[ -n "$value" && "$value" != "<no value>" ]] || fail "missing OCI label $key"
done
say "OCI labels present"

health="$(docker image inspect -f '{{json .Config.Healthcheck}}' "$IMAGE")"
[[ "$health" != "null" ]] || fail "image has no HEALTHCHECK"
say "healthcheck present"

# Secret-looking ENV names are rejected. A real secret must come from the
# orchestrator, never from an image layer.
env_dump="$(docker image inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$IMAGE")"
if printf '%s\n' "$env_dump" | grep -Eiq '^[A-Z0-9_]*(SECRET|PASSWORD|PRIVATE_KEY|ACCESS_KEY|BEARER)[A-Z0-9_]*='; then
  printf '%s\n' "$env_dump" >&2
  fail "secret-looking ENV is baked into the image"
fi
say "no secret-looking ENV"

# The command must run with a read-only rootfs. --check binds nothing and writes
# nothing, so a failure here means the image needs a writable root.
docker run --rm --read-only --tmpfs /tmp:rw,noexec,nosuid,size=32m \
  --user 10001:10001 --entrypoint /usr/local/bin/wxlike-server "$IMAGE" --check >/dev/null \
  || fail "image cannot run with a read-only root filesystem"
say "read-only rootfs compatible"

# /data must be the only writable path.
tmp_vol="wxlike-verify-$$"
docker volume create "$tmp_vol" >/dev/null
trap 'docker volume rm "$tmp_vol" >/dev/null 2>&1 || true' EXIT
docker run --rm --read-only --tmpfs /tmp:rw,noexec,nosuid,size=32m \
  -v "$tmp_vol:/data" --user 10001:10001 --entrypoint /bin/sh "$IMAGE" \
  -c 'test "$(id -u)" = 10001 && touch /data/.probe && rm /data/.probe' \
  || fail "/data is not writable by uid 10001"
say "data volume writable by non-root user"

say "image $IMAGE passed"
