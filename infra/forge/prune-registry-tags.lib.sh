# prune-registry-tags.lib.sh — the registry-verified old-tag removal,
# SHARED by cluster-deploy-runner.sh (after every converge) and
# disk-floor-sweep.sh (below the disk floor). Sourced, not executed:
# one definition of the deletion loop, because two copies of a loop
# that deletes images is exactly the drifting pair §9a bans.
#
#   prune_registry_verified_tags <registry> <keep> <log-prefix>
#
# Removes local boss image tags beyond the <keep> newest, and ONLY
# after `docker manifest inspect` proves the registry holds the tag.
# DELETING A LOCAL TAG IS ONLY SAFE IF THE REGISTRY HAS IT, because
# the registry is the rollback path — the cluster and `boss deploy`
# both pull from there, never from this daemon's store. Anything
# unverifiable is kept and named. `latest` is the quickstart tag, not
# a build artifact of the converge loop, so it is never a candidate.
# `--insecure` because the forge registry is plain HTTP on the LAN —
# which is also why the tags are 7 characters (`git rev-parse
# --short`) and not full shas.

prune_registry_verified_tags() {
    local registry="$1" keep="$2" prefix="$3"
    local kept=0 removed=0 unverified=0 tag
    # Newest first (docker's default ordering).
    for tag in $(docker images "$registry" --format '{{.Tag}}'); do
        case "$tag" in latest|'<none>') continue ;; esac
        if [ "$kept" -lt "$keep" ]; then
            kept=$((kept + 1))
            continue
        fi
        if docker manifest inspect --insecure "$registry:$tag" >/dev/null 2>&1; then
            docker rmi "$registry:$tag" >/dev/null 2>&1 && removed=$((removed + 1))
        else
            unverified=$((unverified + 1))
            echo "$prefix: keeping $tag — not verifiable in the registry"
        fi
    done
    echo "$prefix: images kept=$kept removed=$removed unverified=$unverified"
}
