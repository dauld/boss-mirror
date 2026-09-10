# prune-ci-images-by-age.lib.sh — drop the per-train CI images whose
# train has already arrived, BY AGE, on every hourly pass. Sourced, not
# executed, exactly like its sibling prune-registry-tags.lib.sh: one
# definition of a deletion loop, because two copies of a loop that
# deletes images is the drifting pair CLAUDE.md §9a bans.
#
#   prune_ci_images_by_age <docker-cmd> <expected-daemon-root> \
#                          <repo> <max-age-hours> <keep-newest> <log-prefix>
#
# WHY THIS EXISTS (backlog e5dc60e4). The forge's only sweep was
# FLOOR-TRIGGERED, so per-train `boss-ci:<sha>` images accumulated until
# they caused pressure mid-build. Measured on the host's own journal,
# Sep 03 -> Sep 09 2026: 145 hourly disk-floor-sweep runs, 45 below the
# floor, 26 ending `FLOOR UNMET`, and the one remediation that touches
# these images ran 41 times and freed 238GB — a mean of 5.8GB and a peak
# of 21.5GB per pass. All 41 were below the floor; the other 100 runs
# logged "nothing to do" while the pile grew back. On 2026-09-05 that
# pile was 81GB of a 228GB disk (disk-report, 68 images, 96%
# reclaimable). A disk floor refusal happens BEFORE any check runs, so
# it says nothing about the branch — but it strikes every car aboard,
# and two strikes hold a car out until a human looks: four clean cars
# lost five departures on 2026-08-22 and a full day went to holding on
# 2026-09-05.
#
# So the two passes are COMPLEMENTS, and the order between them is
# load-bearing: this one runs hourly whatever the disk looks like and
# keeps a LOOSER window, and the below-floor emergency pass deletes
# harder. Age-based pruning keeps the floor far away; the floor sweep
# still catches whatever the age rule did not anticipate.
#
# WHICH DAEMON, FIRST. The forge host runs TWO docker daemons
# (reap-dead-ci-jobs' header is the long version): david's rootless one,
# where the converge builds, and the SYSTEM one that Forgejo Actions
# jobs run in — which is where the CI images are and which the sweep
# never touched until 2026-09-05. `docker ps` SUCCEEDS against the
# rootless daemon with none of them, so a prune aimed at the wrong
# daemon REPORTS SUCCESS AND FREES NOTHING, which is worse than not
# running at all. There is therefore no detection: the caller names the
# daemon it means, and this function reads the daemon's own root
# directory and refuses — loudly, non-zero — when that is not what it
# got.
#
# WHAT IS A CANDIDATE. Only tags shaped like a git sha, which is what CI
# stamps on a per-train image (`boss-ci:${GITHUB_SHA}`,
# .forgejo/workflows/ci.yml). That structurally excludes the stable
# bases every build pulls — `rust1.96`, `latest` — without a list of
# exceptions anyone has to maintain. Anything still tagged for a train
# that has arrived is safe to drop: the registry holds what is worth
# keeping, and a re-pull is 3.47GB over the LAN.
#
# WHAT IT REPORTS. Counts and BYTES, before and after, plus one named
# line per item it could not clean. A record that says THAT something
# happened and not WHAT is the defect class that cost a day (CLAUDE.md
# §Diagnosis): this file's own predecessor logged "older than 24h" for
# an `until=4h` filter for days. docker's output on a failed removal is
# captured to a file and printed ONLY on failure — the quiet log without
# throwing the evidence away.
#
# Returns 0 when the pass ran (individual items may have been skipped,
# each named), 1 when it could not LOOK at the daemon it was asked to
# prune. A caller that treats that 1 as success re-creates the silent
# failure above.

# MiB, for humans, from exact bytes.
_ci_prune_mib() { echo $(( ${1:-0} / 1048576 )); }

prune_ci_images_by_age() {
    local docker_cmd="$1" expect_root="$2" repo="$3" max_age_h="$4" keep="$5" prefix="$6"
    local root listing line id tag created size age_h
    local kept_newest=0 kept_young=0 kept_named=0 unreadable=0 removed=0 failed=0
    local total_bytes=0 freed_bytes=0 candidates=0 listed=0

    # (1) WHICH DAEMON. `$docker_cmd` is deliberately word-split: it is
    # "sudo -n docker" for the system daemon, the same sudo the converge
    # runner uses for `docker run`. A refusal is a skip that SAYS SO.
    # shellcheck disable=SC2086
    if ! root="$($docker_cmd info --format '{{.DockerRootDir}}' 2>/dev/null)" || [ -z "$root" ]; then
        echo "$prefix: CI-IMAGE AGE PRUNE SKIPPED — \`$docker_cmd info\` could not be read, so there is no daemon to prune (sudo -n refused, or no daemon there). Nothing was freed; this is not a clean pass." >&2
        return 1
    fi
    case "$root" in
        *"$expect_root"*) : ;;
        *)
            echo "$prefix: CI-IMAGE AGE PRUNE SKIPPED — \`$docker_cmd\` is the daemon rooted at $root, not the one at $expect_root. Pruning the wrong daemon reports success and frees nothing, which is worse than not running." >&2
            return 1
            ;;
    esac

    # shellcheck disable=SC2086
    if ! listing="$($docker_cmd images "$repo" --format '{{.ID}} {{.Tag}}' 2>/dev/null)"; then
        echo "$prefix: CI-IMAGE AGE PRUNE SKIPPED — \`$docker_cmd images $repo\` failed, so the candidates are unknown." >&2
        return 1
    fi
    echo "$prefix: CI-image age prune — $repo in the daemon rooted at $root, keeping the newest $keep and anything under ${max_age_h}h"

    # (2) Candidates, with exact bytes and an exact creation time. Age is
    # read per-image from `{{.Created}}` (RFC3339) rather than parsed out
    # of docker's human "3 days ago" column, which a different locale
    # renders differently — the same reason reap-dead-ci-jobs reads
    # FinishedAt.
    local -a rows=()
    while IFS=' ' read -r id tag; do
        [ -n "${id:-}" ] || continue
        listed=$((listed + 1))
        case "$tag" in
            latest|'<none>') kept_named=$((kept_named + 1)); continue ;;
        esac
        # Only a per-train sha tag is a candidate. CI stamps the full
        # 40-char sha; the 7-char short form is what the deploy images
        # carry, and admitting both costs nothing.
        if ! printf '%s' "$tag" | grep -qE '^[0-9a-f]{7,40}$'; then
            kept_named=$((kept_named + 1))
            continue
        fi
        # Per-item fallible isolation (25b54ae8: one `?` on a per-item
        # call aborted the branch sweep's whole loop, every pass). An
        # image we cannot read is named and left alone — the grace
        # period's logic: we are never in a hurry.
        local meta=""
        # shellcheck disable=SC2086
        if ! meta="$($docker_cmd image inspect --format '{{.Created}} {{.Size}}' "$id" 2>/dev/null)" || [ -z "$meta" ]; then
            echo "$prefix: keeping $repo:$tag — its metadata could not be read (\`image inspect $id\` failed)"
            unreadable=$((unreadable + 1))
            continue
        fi
        created="${meta%% *}"
        size="${meta##* }"
        case "$size" in ''|*[!0-9]*) size=0 ;; esac
        local epoch=""
        if ! epoch="$(date -u -d "$created" +%s 2>/dev/null)" || [ -z "$epoch" ]; then
            echo "$prefix: keeping $repo:$tag — cannot read its creation time ($created)"
            unreadable=$((unreadable + 1))
            continue
        fi
        total_bytes=$((total_bytes + size))
        candidates=$((candidates + 1))
        rows+=("$epoch $tag $size")
    done <<<"$listing"

    if [ "$candidates" -eq 0 ]; then
        echo "$prefix: CI-image age prune: listed=$listed candidates=0 kept_named=$kept_named unreadable=$unreadable — nothing to prune"
        return 0
    fi

    # (3) Newest first, so the keep-N floor is the N the next job is most
    # likely to pull — and so the window below is applied to the rest.
    local cutoff rmi_out rank=0
    cutoff=$(( $(date -u +%s) - max_age_h * 3600 ))
    rmi_out="$(mktemp)"
    while read -r epoch tag size; do
        rank=$((rank + 1))
        age_h=$(( ( $(date -u +%s) - epoch ) / 3600 ))
        if [ "$rank" -le "$keep" ]; then
            kept_newest=$((kept_newest + 1))
            echo "$prefix: keeping $repo:$tag (${age_h}h, $(_ci_prune_mib "$size")MiB) — one of the $keep newest"
            continue
        fi
        if [ "$epoch" -ge "$cutoff" ]; then
            kept_young=$((kept_young + 1))
            continue
        fi
        # Remove by repo:tag, not by image id: an id carrying two tags
        # refuses a bare `rmi`, and untagging is what releases the layers.
        # shellcheck disable=SC2086
        if $docker_cmd rmi "$repo:$tag" >"$rmi_out" 2>&1; then
            removed=$((removed + 1))
            freed_bytes=$((freed_bytes + size))
            echo "$prefix: removed $repo:$tag (${age_h}h, $(_ci_prune_mib "$size")MiB)"
        else
            # CAPTURE TO A FILE, PRINT ON FAILURE. A `-q` or a tail here
            # would suppress the output and not the work, and the cost is
            # paid by whoever is next in front of the failure.
            failed=$((failed + 1))
            echo "$prefix: could NOT remove $repo:$tag (${age_h}h, $(_ci_prune_mib "$size")MiB) — docker said:" >&2
            sed 's/^/    /' "$rmi_out" >&2
        fi
    done < <(printf '%s\n' "${rows[@]}" | sort -rn)
    rm -f "$rmi_out"

    echo "$prefix: CI-image age prune: listed=$listed candidates=$candidates removed=$removed kept_newest=$kept_newest kept_young=$kept_young kept_named=$kept_named unreadable=$unreadable failed=$failed reclaimed=$(_ci_prune_mib "$freed_bytes")MiB of $(_ci_prune_mib "$total_bytes")MiB tagged ($(_ci_prune_mib $((total_bytes - freed_bytes)))MiB left in $repo)"
    return 0
}
