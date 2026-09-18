#!/usr/bin/env bash
# Build + push the boss-ci runner image, stamped with its own
# provenance: the sha256 of (Dockerfile + required-tools.txt) at build
# time. The locomotive CI job recomputes that hash from its checkout
# and refuses to run when they disagree — so "the runner cached an old
# image" (forge train #1 round 2: a registry retag does not refresh a
# runner's local tag) stops being a 25-minute mystery and becomes a
# named red with this script as the remediation.
#
# Run on the forge host (rootful docker, which the runner shares):
#   infra/forge/boss-ci/build.sh
set -euo pipefail
cd "$(dirname "$0")"

# The owner's namespace on the forge registry, from forge-defaults.sh
# off /etc/boss/sor.env (BOSS_CI_REGISTRY overrides).
. ../forge-defaults.sh
forge_need REGISTRY_BASE
REGISTRY="$REGISTRY_BASE"
TAG="${BOSS_CI_TAG:-rust1.96}"

# The image stamps itself from its own build context now — see the
# Dockerfile. This script no longer passes it, so it cannot pass it
# wrong; it recomputes the same hash only to report and verify.
stamp="$(cat Dockerfile required-tools.txt | sha256sum | cut -d' ' -f1)"

docker build -t "$REGISTRY/boss-ci:$TAG" .

baked="$(docker run --rm --entrypoint cat "$REGISTRY/boss-ci:$TAG" /etc/boss-ci-stamp)"
if [ "$baked" != "$stamp" ]; then
  echo "build.sh: image stamped $baked but this tree hashes to $stamp — refusing to push." >&2
  echo "  The two must agree or locomotive reds on every run with no useful message." >&2
  exit 1
fi

docker push "$REGISTRY/boss-ci:$TAG"

echo "boss-ci:$TAG pushed, stamp $stamp"
