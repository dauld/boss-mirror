#!/usr/bin/env bash
# cluster-deploy-lib — the parts of the cluster converge that decide
# WHERE the cluster lands, as functions a stub kubectl can exercise.
# Sourced by cluster-deploy-runner.sh; tested by
# infra/lint/the-converge-rolls-back-to-a-named-build.sh on every gate.
#
# WHY. On 2026-09-05 a head bricked its boot and the runner's guard
# fired correctly — then rolled back to PRE_REV, the revision its own
# `kubectl apply` had just created from the manifest's literal image
# tag: a month-old build whose jobs API cannot start against today's
# registry. The cluster stayed dark for four hours on a "rollback" that
# rolled to the wrong place (packet: the converge rolls back to the
# manifest's placeholder). CLAUDE.md §Diagnosis: "roll back" is a
# TARGET, not a verb. The target is the last CONVERGED build — the sha
# the runner itself stamps after every successful roll — named by image,
# verified Ready.
#
# Two more things follow from the same night. The manifest's image
# field is a placeholder the apply step used to put on the cluster for
# the seconds between apply and patch — a dark window on an old build
# on every converge; `manifests_with_image` writes the converged sha
# into the applied copy instead, so the apply never changes what runs.
# And a head that cannot even find what its launcher sources should
# never reach the cluster: `image_boots` runs the image's own launcher
# check before anything is applied.

# manifests_with_image SRC_DIR DST_DIR REGISTRY TAG
#   Copy the manifests, rewriting the boss image tag to TAG so the
#   apply carries the build that is already converged. TAG "none"
#   (first ever converge) leaves the files untouched.
manifests_with_image() {
    local src="$1" dst="$2" registry="$3" tag="$4"
    mkdir -p "$dst"
    cp "$src"/*.yaml "$dst"/
    [ "$tag" = "none" ] && return 0
    local f
    for f in "$dst"/*.yaml; do
        sed -i -E "s#(image: ${registry}):[A-Za-z0-9._-]+#\1:${tag}#g" "$f"
    done
}

# image_boots DOCKER IMAGE
#   The image's launcher checks itself (services-launcher.sh --check:
#   every file it sources is beside it). Non-zero = do not roll it.
image_boots() {
    local docker="$1" image="$2"
    $docker run --rm --entrypoint /usr/local/bin/boss-launch "$image" --check
}

# roll_deployment K REGISTRY HEAD LAST_GOOD FAILED_FILE
#   Patch deploy/boss to REGISTRY:HEAD and wait for Ready. If it never
#   goes Ready: quarantine HEAD in FAILED_FILE and roll back to the
#   NAMED target — REGISTRY:LAST_GOOD when a converged build is known,
#   else the image that was running before the patch — then wait for
#   that to be Ready. Returns 0 only when HEAD is serving.
roll_deployment() {
    local k="$1" registry="$2" head="$3" last_good="$4" failed_file="$5"
    local pre_image
    pre_image=$($k get deploy boss -n boss -o jsonpath='{.spec.template.spec.containers[0].image}')
    _patch_boss_image "$k" "$registry:$head"
    if $k rollout status deploy/boss -n boss --timeout=420s; then
        return 0
    fi
    echo "$head" > "$failed_file"
    local target
    if [ -n "$last_good" ] && [ "$last_good" != "none" ]; then
        target="$registry:$last_good"
    else
        target="$pre_image"
    fi
    echo "cluster-deploy-runner: $head never went Ready — rolling back to $target (the last converged build, by name)" >&2
    if _patch_boss_image "$k" "$target" \
        && $k rollout status deploy/boss -n boss --timeout=300s; then
        echo "cluster-deploy-runner: rolled back — cluster serves $target; $head is quarantined (rm $failed_file to retry it)" >&2
    else
        echo "cluster-deploy-runner: ROLLBACK TO $target ALSO FAILED — the cluster needs hands NOW" >&2
    fi
    return 1
}

_patch_boss_image() {
    local k="$1" image="$2"
    $k patch deploy boss -n boss --type=json \
        -p "[{\"op\":\"replace\",\"path\":\"/spec/template/spec/containers/0/image\",\"value\":\"$image\"},{\"op\":\"replace\",\"path\":\"/spec/template/spec/initContainers/0/image\",\"value\":\"$image\"}]"
}

# converge_held HOLD_FILE — an operator's hold stands: print its reason
# and return 0; no hold, return 1. The runner asks before it builds.
converge_held() {
    local f="$1"
    [ -f "$f" ] || return 1
    cat "$f"
}
