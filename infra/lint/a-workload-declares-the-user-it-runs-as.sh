#!/usr/bin/env bash
#
# a-workload-declares-the-user-it-runs-as — a cluster workload states the
# uid it runs as, instead of inheriting it from an image nobody reads.
#
# WHY THIS IS A CONFIG-CHANNEL CHECK AND NOT A TEST
# -------------------------------------------------
# The gate never builds or applies these manifests, so a green pre-flight
# says nothing about them. That is the exact blindness that let the
# gate-runner's pod-security warning persist unnoticed through nine gate
# launches in one evening (backlog bb5a902e). The only thing that can
# hold a manifest invariant is a shape lint reading the manifest.
#
# WHAT WENT MISSING
# -----------------
# Surveyed 2026-09-11 (backlog 5234cda4): of the twenty manifests under
# infra/cluster/manifests/, eleven pod-bearing ones carried NO
# securityContext at all and boss.yaml carried only fsGroup. Most of them
# run the BOSS image, which is already uid 1500 — so for those the
# BEHAVIOUR was never wrong, only the DECLARATION was missing.
#
# An undeclared uid is not a cosmetic gap, because the cliff is
# cluster-wide rather than per-namespace: boss.yaml enforces the baseline
# profile on the boss namespace only, and the Talos machine-config
# DEFAULT outside this repo — measured 2026-09-12 (d42d4967): enforce
# baseline, warn restricted:latest — is what every unlabelled namespace
# takes. Tightening that default to restricted would hit every namespace
# at once — so these are not N independent small
# risks, they are one switch away from being one large one. A workload
# that states runAsNonRoot + runAsUser survives that switch; one that
# inherits its uid from an image is admitted or refused on a property no
# file in this tree records.
#
# WHAT IS ASSERTED, AND WHY THAT AND NOT MORE
# -------------------------------------------
# Per pod template, at pod level:   runAsNonRoot: true + runAsUser: <uid>
# Per container, at container level: allowPrivilegeEscalation: false
#
# Those are the three that cost nothing to declare when they are already
# true. readOnlyRootFilesystem is deliberately NOT asserted: it is not
# part of the restricted profile and it WOULD change behaviour — the
# chores shell out to boss-generate-configs, which writes files. An
# assertion that changes behaviour belongs to a car that measured the
# behaviour, which is what the exemption list below is for.
#
# THE EXEMPTION LIST IS THE POINT
# -------------------------------
# A file is exempt only with a reason, and the reason is always the same
# kind of thing: the uid its data or its credentials require has not been
# MEASURED yet. boss-backup.yaml is the worked example — its ship-key is
# defaultMode 0400 and (its own comment, line 264) "only ever succeeded
# because it runs as root", so the moment anything hands that pod
# runAsNonRoot the offsite leg fails, and it fails as an SSH problem,
# which is the wrong place to look. Guessing there breaks the service
# rather than one check. Each exempt entry gets its own car with its own
# establishment work.
#
# An exemption for a file that no longer exists is worse than no
# exemption: it silently widens to nothing and hides the next gap. So a
# stale entry FAILS here, the same way the consist check fails on a
# roster that has drifted from its directory (CLAUDE.md §9a).
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

DIR="infra/cluster/manifests"

# Exempt file<TAB>reason. Every entry is a car somebody else owes; none is
# a decision that this workload may stay undeclared forever.
EXEMPT=(
    "boss-dev.yaml	part (3) of 5234cda4, and it is worse than the hard set: the dev container needs SETGID/SETUID for the ssh door, so it can never reach drop-ALL — only baseline compliance. A car editing this file rolls the dev pod on converge and ends the live operator session, so it lands only at a David-timed restart (boss-dev-manifest-cars-restart-the-session)."
    "boss.yaml	holds THREE workloads with three different answers: the SoR postgres StatefulSet (the gate-runner car had to read /proc to establish postgres runs as 999), the nats StatefulSet, and the SoR app Deployment whose strategy is Recreate — so any roll of it is a full outage of :7900. Carries fsGroup: 1500 today, which is the volume half of the answer and not the user half. One car each, measured."
    "boss-backup.yaml	the documented trap: its ship-key is defaultMode 0400 and only ever succeeded because the pod runs as root, so runAsNonRoot breaks the offsite leg and reports it as an SSH failure. Also carries a postgres container and a google/cloud-sdk container, each with its own uid answer."
    "boss-tls.yaml	goacme/lego writes ACME account + certificate material, and alpine/k8s shells kubectl; neither uid has been measured."
    "boss-tls-front.yaml	caddy:2.8-alpine, whose data/config dir uid has not been measured."
    "boss-estate-observe.yaml	alpine/k8s, uid unmeasured — and a separate car is holding this file, so declaring it here would collide rather than land."
)

# Non-vacuity floor. Seven BOSS-image chore CronJobs are declared as of
# 2026-09-11; the floor sits one below so a legitimately retired chore
# does not red the tree, while a lint that has quietly lost its subject
# still does. NOT an exact count — that would make this a §9a duplicate
# of the directory listing, free to drift from it.
MIN_CHECKED=6

[ -d "$DIR" ] || { echo "workload-declares-user: $DIR is missing"; exit 1; }

fail=0

# A stale exemption hides the next gap — check that first, before any
# manifest is read.
for entry in "${EXEMPT[@]}"; do
    name="${entry%%	*}"
    if [ ! -f "$DIR/$name" ]; then
        echo "workload-declares-user: STALE EXEMPTION — $DIR/$name does not exist."
        echo "  An exemption for a deleted file widens this check to nothing and hides"
        echo "  the next undeclared workload. Delete the entry from EXEMPT in"
        echo "  ${BASH_SOURCE[0]}."
        fail=1
    fi
done

checked=0
for path in $(LC_ALL=C ls "$DIR"/*.yaml 2>/dev/null); do
    name="$(basename "$path")"

    skip=0
    for entry in "${EXEMPT[@]}"; do
        [ "${entry%%	*}" = "$name" ] && skip=1 && break
    done
    [ "$skip" = 1 ] && continue

    # Pod templates in this file. A file with none (Service, ConfigMap,
    # Role, Secret) has no uid to declare and is not a gap.
    pods=$(grep -cE '^kind: (Deployment|StatefulSet|CronJob|DaemonSet|Job|Pod)$' "$path")
    [ "$pods" -eq 0 ] && continue

    # One image: line per container — initContainers included, which is
    # correct: an initContainer is a container the restricted profile
    # judges the same way.
    containers=$(grep -cE '^[[:space:]]+image:' "$path")
    nonroot=$(grep -c 'runAsNonRoot: true' "$path")
    asuser=$(grep -cE 'runAsUser: [0-9]+' "$path")
    noescalate=$(grep -c 'allowPrivilegeEscalation: false' "$path")

    checked=$((checked + pods))

    if [ "$nonroot" -lt "$pods" ] || [ "$asuser" -lt "$pods" ]; then
        echo "workload-declares-user: $path declares $pods pod template(s) but only"
        echo "  $nonroot runAsNonRoot and $asuser runAsUser. The uid is inherited from the"
        echo "  image, so no file in this tree records it and a cluster-wide restricted"
        echo "  default decides the workload's fate on an unrecorded property."
        echo "  MEASURE the uid (do not infer it from the image name), then declare it:"
        echo "      securityContext:"
        echo "        runAsNonRoot: true"
        echo "        runAsUser: <measured uid>"
        echo "        runAsGroup: <measured gid>"
        echo "        seccompProfile: {type: RuntimeDefault}"
        echo "  If the uid CANNOT be established without risking the service, that is an"
        echo "  EXEMPT entry with its reason — not a guess."
        fail=1
    fi

    if [ "$noescalate" -lt "$containers" ]; then
        echo "workload-declares-user: $path has $containers container(s) but only"
        echo "  $noescalate allowPrivilegeEscalation: false. Declare it per container:"
        echo "      securityContext:"
        echo "        allowPrivilegeEscalation: false"
        echo "        capabilities: {drop: [\"ALL\"]}"
        fail=1
    fi
done

if [ "$checked" -lt "$MIN_CHECKED" ]; then
    echo "workload-declares-user: only $checked workload(s) checked, floor is $MIN_CHECKED."
    echo "  Either manifests moved out of $DIR or the exemption list has grown to cover"
    echo "  the tree. A check with no subject passes for the wrong reason."
    fail=1
fi

[ "$fail" = 0 ] || exit 1
echo "workload-declares-user: OK — $checked workload(s) declare the uid they run as (${#EXEMPT[@]} exempt, each with a reason)"
