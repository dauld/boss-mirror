#!/usr/bin/env bash
#
# mirror-base-images — copy the CI build's external base images into the
# forge registry so `build-image` resolves NO public DNS (backlog
# 7f408b99). The gcr.io / docker.io lookups the build depends on flaked
# repeatedly (#236, #250: `lookup gcr.io on 127.0.0.53:53: no such
# host`) and reddened trains; mirroring moves that DNS dependency off
# the every-train hot path and onto this occasional, retryable job.
# Trains then pull the base images from 10.20.0.15:3000 (the forge host
# itself), which needs no external resolver.
#
# BOUNDED BY CONSTRUCTION, like disk-floor-sweep.sh (verbs.json _about):
# the set of images is a FIXED in-tree list below, never a packet-
# supplied name. The `mirror-base-images` ops-verb runs THIS script with
# no arguments; a packet cannot ask it to pull or push anything else.
# The forge tags produced here are the exact refs ci.yml's `image:` and
# infra/forge/boss-ci/Dockerfile's `FROM`s point at — one source of
# truth, pinned by infra/lint/the-build-pulls-only-mirrored-bases.sh.
#
# AUTH. The one proven registry-push path on the forge host is david's
# rootless docker with its ambient registry login (see
# cluster-deploy-runner.sh). This script runs under the ops-runner as
# ROOT, so it points the docker CLI at david's socket AND david's
# docker config explicitly — otherwise root's own (unauthenticated)
# config is read and the push 401s. Both are overridable; failure is
# LOUD (a non-zero exit naming the image and step), never silent, so the
# ops-request records exactly what broke.
set -euo pipefail

REGISTRY_BASE="${BOSS_FORGE_REGISTRY_BASE:-10.20.0.15:3000/david}"
export DOCKER_HOST="${BOSS_MIRROR_DOCKER_HOST:-unix:///run/user/1000/docker.sock}"
export DOCKER_CONFIG="${BOSS_MIRROR_DOCKER_CONFIG:-/home/david/.docker}"

# external source ref  |  forge repo:tag (under $REGISTRY_BASE)
# Keep in lockstep with ci.yml (the kaniko executor the job runs in) and
# infra/forge/boss-ci/Dockerfile (the FROM bases kaniko then pulls).
IMAGES="
gcr.io/kaniko-project/executor:v1.23.2-debug|kaniko-executor:v1.23.2-debug
docker.io/oven/bun:1.3-slim|bun:1.3-slim
docker.io/library/rust:1.96.1-slim-bookworm|rust:1.96.1-slim-bookworm
"

mappings() { printf '%s\n' "$IMAGES" | sed '/^[[:space:]]*$/d'; }

if [ "${1:-}" = "--check" ]; then
    # No docker, no network: validate the list is well-formed and print
    # the mappings. Lets a gate lint and a dry inspection verify the
    # source-of-truth list without touching the registry.
    rc=0
    while IFS='|' read -r ext dst; do
        if [ -z "$ext" ] || [ -z "$dst" ] || [ "$ext" = "$dst" ]; then
            echo "mirror-base-images: malformed mapping: '$ext' -> '$dst'" >&2
            rc=1
        fi
        echo "  $ext  ->  $REGISTRY_BASE/$dst"
    done < <(mappings)
    echo "mirror-base-images: --check ok ($(mappings | wc -l | tr -d ' ') mapping(s))"
    exit $rc
fi

pull_with_retry() {
    # The pull is the only step exposed to public DNS — the very flake
    # this job exists to retire off the hot path. Retry it here so a
    # transient resolver miss during a mirror run does not need a human
    # to re-file the ops-request.
    local ref="$1" attempt=1 max=4
    while :; do
        if docker pull "$ref"; then return 0; fi
        if [ "$attempt" -ge "$max" ]; then
            echo "mirror-base-images: pull FAILED after ${max} attempts: $ref" >&2
            return 1
        fi
        echo "mirror-base-images: pull attempt ${attempt}/${max} failed for $ref — retrying" >&2
        attempt=$((attempt + 1)); sleep $((attempt * 5))
    done
}

count=0
while IFS='|' read -r ext dst; do
    forge="$REGISTRY_BASE/$dst"
    echo "mirror-base-images: $ext  ->  $forge"
    pull_with_retry "$ext" || exit 1
    docker tag "$ext" "$forge" || { echo "mirror-base-images: tag FAILED: $ext -> $forge" >&2; exit 1; }
    docker push "$forge"      || { echo "mirror-base-images: push FAILED (registry auth? DOCKER_CONFIG=$DOCKER_CONFIG): $forge" >&2; exit 1; }
    count=$((count + 1))
done < <(mappings)

echo "mirror-base-images: done — ${count} image(s) mirrored to ${REGISTRY_BASE}"
