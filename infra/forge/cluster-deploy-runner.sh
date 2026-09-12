#!/usr/bin/env bash
#
# cluster-deploy-runner — converge the cluster onto forge main.
#
# The deployment half of the forge shipping protocol (directive
# 27ab7680): the conductor merges CI-green trains into forge main;
# this runner, on the forge host, notices main moved, builds the
# all-in-one image, pushes it to the forge registry, applies the
# tree's cluster manifests (infra/cluster/manifests/), and rolls the
# cluster deployment. Derived state reconverging on intent
# (deployment-as-network) — no ssh from the conductor, no shared
# credentials: each side touches only what it owns. Cluster config
# converges on forge main exactly like code does; hand-applied
# changes are drift (see infra/cluster/manifests/README.md).
#
# Install (forge host):
#   sudo cp infra/forge/cluster-deploy-runner.{service,timer} /etc/systemd/system/
#   sudo systemctl daemon-reload && sudo systemctl enable --now cluster-deploy-runner.timer
#
# Expects: a clone at $REPO with a `forgejo` remote; rootless docker
# (lingering enabled) logged into the registry; a kubeconfig at
# $KUBECONFIG_PATH; kubectl via the alpine/k8s container.
set -euo pipefail

# Run from a SNAPSHOT, never from the file this script is about to
# rewrite.
#
# This script lives inside $REPO, and a few lines below it runs
# `git checkout "$HEAD"` on that same repo. Bash does not read a script
# into memory; it reads incrementally and remembers a BYTE OFFSET. So
# when git replaces the file underneath a running invocation, bash
# carries on reading the new contents from the old offset — resuming
# mid-token, skipping a command, or executing a fragment of a line that
# happens to be syntactically valid. The failure is silent, unrepeatable
# and shaped by how much the diff moved the bytes, which makes it close
# to undiagnosable from the outcome alone.
#
# It has not bitten yet only because the file changed rarely and the
# offsets happened to survive. That is luck, not a property.
#
# `exec` into a copy makes the executing file structurally incapable of
# being rewritten: git can do whatever it likes to $REPO afterwards,
# because the bytes bash is reading are no longer reachable from there.
# The env var is the recursion guard and also carries the path so the
# snapshot can clean itself up on the way out — the trap belongs in the
# snapshot's own process, since `exec` replaces this one and would never
# run a trap set here.
if [ -z "${BOSS_RUNNER_SNAPSHOT:-}" ]; then
    snap="$(mktemp -t cluster-deploy-runner.XXXXXX)"
    cat "$0" > "$snap"
    BOSS_RUNNER_SNAPSHOT="$snap" exec bash "$snap" "$@"
fi

REPO="${BOSS_FORGE_REPO_DIR:-$HOME/boss}"

# THE RUN ENDS BY SAYING HOW IT ENDED — to the ops-request that started
# it, when one did (cluster-deploy-lib.sh answer_converge_requests;
# backlog d66f92b2). STAGE names where the run is, so a death anywhere
# below reads `converge_failed: <stage> (exit N)` on the packet that
# asked, instead of the clean `answered` that `systemctl start
# --no-block` returned before the unit had done anything. OUTCOME is set
# by the exits that are not failures (unchanged, held, converged). The
# trap also cleans both snapshots: exec replaces the process, so only
# the stage that actually exits runs it.
STAGE="start"
OUTCOME=""
# WHERE THE MINUTES WENT, on the packet. Every converge closed its
# maintenance packet `result=ok` and nothing else; how long the image
# build took — the second-longest stage of a car's life, 10–20 min
# measured 2026-09-12 — lived only in this journal, 200 lines at a time
# through an ops-request. run-summary.sh is the one definition of how a
# unit leaves facts for its own ExecStopPost (the unit declares
# BOSS_RUN_SUMMARY_FILE); each stage below stamps its seconds as soon as
# it knows them, and a run that dies keeps what it had recorded.
# From $REPO like the other libs: this script runs from a snapshot in
# /tmp (BOSS_RUNNER_SNAPSHOT), so `dirname "$0"` is not the tree.
. "$REPO/infra/run-summary.sh"
run_summary_reset
_stage_started=$(date +%s)
_stage_done() { # <field>  — seconds since the previous stage boundary
    local now; now=$(date +%s)
    run_summary_field "$1" "$(( now - _stage_started ))"
    _stage_started=$now
}
_finish() {
    local rc=$?
    rm -f "$BOSS_RUNNER_SNAPSHOT" ${BOSS_RUNNER_SNAPSHOT2:+"$BOSS_RUNNER_SNAPSHOT2"}
    if [ -n "$OUTCOME" ]; then
        answer_converge_requests "$REPO" "${OUTCOME%%=*}" "${OUTCOME#*=}" || true
    elif [ "$rc" -ne 0 ]; then
        answer_converge_requests "$REPO" converge_failed "$STAGE (exit $rc)" || true
    fi
    exit "$rc"
}
trap _finish EXIT
# Sourced from the PRE-checkout copy here, and again from HEAD after
# the stage-2 exec; both copies must carry the functions the trap
# uses, which is why they live in the lib and not in this file.
. "$REPO/infra/forge/cluster-deploy-lib.sh"
take_converge_requests "$REPO" || true
REGISTRY="${BOSS_FORGE_REGISTRY:-10.20.0.15:3000/david/boss}"
KUBECONFIG_PATH="${BOSS_FORGE_KUBECONFIG:-$HOME/kc.yaml}"
STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-$HOME/.boss-last-built}"
# The quarantine stamp for a head whose BOOT failed (rollout never went
# Ready and was rolled back). Distinct from STAMP_FILE — that one means
# "converged", this one means "proven unbootable; do not re-roll it".
FAILED_FILE="${BOSS_FORGE_LAST_FAILED:-$HOME/.boss-last-failed}"
export DOCKER_HOST="${DOCKER_HOST:-unix:///run/user/1000/docker.sock}"

cd "$REPO"
# ONE LOCK for every git user of this checkout (checkout-lock.sh):
# forge-converge fetches on the same tick and the merge-triggered
# `converge` starts this unit a second after any merge. On 2026-09-07
# the fetch below died on `cannot lock ref` and the retry ten minutes
# later on a stale index.lock — two converge cycles lost to the same
# checkout being shared without a lock (backlog d66f92b2).
. "$REPO/infra/forge/checkout-lock.sh"
STAGE="fetch"
checkout_git "$REPO" fetch -q forgejo main
HEAD=$(git rev-parse --short forgejo/main)
LAST=$(cat "$STAMP_FILE" 2>/dev/null || echo none)

if [ "$HEAD" = "$LAST" ]; then
    echo "cluster-deploy-runner: forge main unchanged ($HEAD)"
    run_summary_field unchanged "$HEAD"
    OUTCOME="converged=$HEAD (unchanged)"
    exit 0
fi
_stage_started=$(date +%s)

# A head that bricked its boot stays quarantined until main moves.
# Without this, the 2026-09-02 shape loops forever: rollout fails,
# set -e kills the run before the stamp, and the next tick rebuilds
# and re-rolls the SAME brick every ten minutes — which is exactly
# what the runner spent the 20:15-21:10 outage doing. Exit NONZERO:
# a held converge is a failed unit somebody can see, not a quiet pass.
if [ "$HEAD" = "$(cat "$FAILED_FILE" 2>/dev/null || echo none)" ]; then
    echo "cluster-deploy-runner: $HEAD bricked its boot on a previous converge — holding until main moves (rm $FAILED_FILE to retry it)" >&2
    STAGE="quarantined $HEAD"
    exit 1
fi

# An operator's hold (converge-hold.sh, the hold-converge ops verb)
# stops the roll before the build: main has moved, and a human said not
# yet. Loud on every tick, exit 0 — a hold is a decision, not a failure.
if reason=$(converge_held "${BOSS_CONVERGE_HOLD:-/var/tmp/boss-converge-hold}"); then
    echo "cluster-deploy-runner: converge HELD — $reason — main at $HEAD not built or rolled (release-converge lifts it)" >&2
    OUTCOME="converge_held=$reason"
    exit 0
fi
echo "cluster-deploy-runner: forge main moved $LAST -> $HEAD; building"
STAGE="checkout"
checkout_git "$REPO" checkout -q "$HEAD" 2>/dev/null || checkout_git "$REPO" checkout -qf "$HEAD"

# STAGE 2: converge on HEAD's OWN driver (David accepted (a) on
# d0b5efd4, through the v11 decision surface). The stage-0 snapshot is
# necessarily the PRE-checkout copy — so without this hop, a change to
# this script merged on train N only executes at train N+1's converge.
# Measured cost of that property, 2026-08-19: the chore-CronJob image
# pin sat out its own train's converge and seven CronJobs pulled a
# stale :latest for a full cycle.
#
# So after checkout, snapshot the checked-out copy and exec THAT. The
# re-executed prefix (env, cd, fetch, stamp check, checkout) is
# idempotent — same HEAD, stamp not yet written — and the second guard
# var is what stops a third hop. Both snapshots are cleaned by the
# stage that actually exits (exec replaces the process, so an exec'ing
# stage's trap never fires — the trap above cleans BOTH paths).
if [ -z "${BOSS_RUNNER_SNAPSHOT2:-}" ]; then
    snap2="$(mktemp -t cluster-deploy-runner-head.XXXXXX)"
    cat infra/forge/cluster-deploy-runner.sh > "$snap2"
    BOSS_RUNNER_SNAPSHOT2="$snap2" exec bash "$snap2" "$@"
fi
# The FULL commit rides into the image's environment (the runtime
# stage's ENV; Capabilities.commit reads it at process start) so the
# conductor's `converged` step can verify the running pod serves this
# exact merge — the short tag stays the image name, the full sha is
# the attestation (prefix-compared, so either length matches). It is
# NOT compiled in: that made every train recompile every crate.
STAGE="build $HEAD"
# A FAILED CONVERGE SAYS WHY.
#
# This ran `docker build -q`. On 2026-09-09 train 283 merged at 03:40
# and its converge failed four times — 03:46, 03:55, 04:04, 04:14 —
# and the record held the Dockerfile context dump, a one-line ERROR,
# and NOTHING the compiler said. Establishing even the exit code meant
# reading an untruncated journal line by hand; ruling out a compile
# error and a full disk took a workspace check on another box and a
# disk-report ops-request, and neither question should have needed
# asking. The build knew the answer and was told not to speak
# (backlog ddb0f7bd).
#
# `-q` suppresses OUTPUT, not work: it saves no build time, so the
# only thing it was buying was a quiet journal. Capturing to a file
# buys that too, and keeps the evidence. So the build always runs
# verbose into a log, the log is printed ONLY on failure, and the
# first failure is readable instead of the second.
#
# The stamp is the other half. The runner is stateless across timer
# firings, so without it a head that fails four times looks like four
# unrelated failures; with it the log says which attempt this is. It
# is deliberately NOT a quarantine — unlike FAILED_FILE, which holds
# an unbootable head out, a build that failed is retried, because the
# common causes here (a registry fetch losing a DNS race on this LAN,
# memory contention with a concurrent CI job) are transient and a
# retry that PASSES is itself the finding.
BUILD_FAILED_FILE="${BOSS_FORGE_LAST_BUILD_FAILED:-$HOME/.boss-last-build-failed}"
BUILD_ATTEMPT="first attempt"
if [ "$HEAD" = "$(cat "$BUILD_FAILED_FILE" 2>/dev/null || echo none)" ]; then
    BUILD_ATTEMPT="RETRY — this head already failed to build at least once"
fi
BUILD_LOG="$(mktemp -t boss-converge-build.XXXXXX)"
echo "cluster-deploy-runner: building $HEAD ($BUILD_ATTEMPT)"
_build_started=$(date +%s)
if docker build -f infra/oss-quickstart/Dockerfile \
    --build-arg BOSS_BUILD_COMMIT="$(git rev-parse HEAD)" \
    -t "$REGISTRY:$HEAD" . > "$BUILD_LOG" 2>&1; then
    rm -f "$BUILD_FAILED_FILE" "$BUILD_LOG"
    # Stamped here, in the block the build-log test lifts verbatim, with
    # the lib's own function rather than a helper the block cannot see.
    run_summary_field build_s "$(( $(date +%s) - _build_started ))"
    run_summary_field build_head "$HEAD"
    _stage_started=$(date +%s)
else
    build_rc=$?
    echo "$HEAD" > "$BUILD_FAILED_FILE"
    # THE FAILURE, NOT THE TAIL. Capturing the log was half the fix and
    # the half I got wrong first: BuildKit prints its epilogue — the
    # whole failing RUN's Dockerfile context, some 35 lines — AFTER the
    # step's own output, so `tail` showed the recipe and hid the
    # compiler. Measured 2026-09-09 05:12, on the first failure this
    # capture ever saw. That is precisely the defect CLAUDE.md's
    # Diagnosis section names, committed inside the fix for it.
    #
    # So scan for what a failure actually prints and show the window
    # around the FIRST one, falling back to the tail only when nothing
    # matches — and say which of the two is on screen, because a tail
    # presented as a diagnosis is how this started.
    lines="${BOSS_BUILD_LOG_TAIL:-80}"
    marker=$(grep -nEm1 'error\[E|^error:|error: could not compile|panicked at|cannot find|No space left|Killed|signal: 9|FATAL:' \
                 "$BUILD_LOG" 2>/dev/null | cut -d: -f1 || true)
    if [ -n "$marker" ]; then
        start=$(( marker > 20 ? marker - 20 : 1 ))
        echo "cluster-deploy-runner: BUILD FAILED for $HEAD (exit $build_rc, $BUILD_ATTEMPT) — the failure, with context, from line $marker of the build log" >&2
        sed -n "${start},$(( start + lines ))p" "$BUILD_LOG" >&2
    else
        echo "cluster-deploy-runner: BUILD FAILED for $HEAD (exit $build_rc, $BUILD_ATTEMPT) — nothing in the log named a failure, so this is the last $lines lines, not a diagnosis" >&2
        tail -n "$lines" "$BUILD_LOG" >&2
    fi
    echo "cluster-deploy-runner: (full build log kept at $BUILD_LOG)" >&2
    exit "$build_rc"
fi
STAGE="push $HEAD"
docker push "$REGISTRY:$HEAD"
_stage_done push_s

# The image proves it can boot before it goes anywhere near the cluster
# (cluster-deploy-lib.sh image_boots): its own launcher checks that
# every file it sources is beside it. A head that fails here is
# quarantined like a head that failed on the cluster — with no dark
# window at all, because nothing was applied.
. "$REPO/infra/forge/cluster-deploy-lib.sh"
STAGE="boot check $HEAD"
if ! image_boots docker "$REGISTRY:$HEAD"; then
    echo "$HEAD" > "$FAILED_FILE"
    echo "cluster-deploy-runner: $HEAD fails its own boot check — not rolling it; quarantined (rm $FAILED_FILE to retry it)" >&2
    exit 1
fi

# THE CONVERGE CLEANS UP AFTER ITSELF.
#
# Every run builds a 1.07 GB image and pushes it, and nothing ever
# removed the local copy. MEASURED 2026-08-29 on the minipc: 42 boss
# tags resident, ~45 GB, on a 228 G disk that had reached 93% full —
# the host that also serves the forge, the OCI registry and the CI
# runner, so image growth competes with the registry it feeds. The
# daily disk-headroom and stale-build-cache sweeps had each been
# opening a packet about it for three days (`0e62f404`, `b99e9627`).
#
# AFTER THE PUSH, DELIBERATELY. The push is what makes a local copy
# redundant, so cleanup that ran before it could delete the only copy
# of something. `set -e` means a failed push never reaches this.
#
# The deletion loop itself — keep the N newest, VERIFY a candidate is
# present in the registry before rmi, never touch `latest` — is the
# SHARED definition in prune-registry-tags.lib.sh, sourced from the
# checked-out HEAD (we cd'd to $REPO above). disk-floor-sweep.sh runs
# the identical loop below the disk floor; two copies of a loop that
# deletes images is the drifting pair §9a bans, so the rationale for
# why verification is load-bearing lives in the lib's header now.
# Checked while extracting this: all 42 resident tags were in the
# registry, so this is hygiene rather than recovery from a divergence.
. "$REPO/infra/forge/prune-registry-tags.lib.sh"
STAGE="prune images"
prune_registry_verified_tags "$REGISTRY" "${BOSS_RUNNER_KEEP_IMAGES:-5}" cluster-deploy-runner

# Build cache is regenerable by definition, so the only cost of being
# wrong here is a slower next build. Age-filtered rather than emptied:
# a week keeps the layers a rebuild actually reuses and drops the rest.
# `--keep-storage` is gone in Docker 29; `--filter until=` is the
# supported spelling.
docker builder prune -f --filter until=168h >/dev/null 2>&1 || true
echo "cluster-deploy-runner: build cache pruned (older than 168h)"

K="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"

# Cluster config converges with the code: apply the tree's manifests
# idempotently (secrets are referenced by name and stay out-of-tree).
# Apply comes BEFORE the image roll so the tag built above — not the
# placeholder tag committed in boss.yaml — is what the cluster ends
# on. A failed apply aborts here (set -e): no stamp is written, the
# next timer run retries.
KM="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro -v $REPO/infra/cluster/manifests:/manifests:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
# The applied copy carries the build that is ALREADY converged (the
# stamp), not the manifest's placeholder tag: the apply must never
# change what runs. Rolling to $HEAD is roll_deployment's job below.
STAGE="apply manifests"
APPLY_DIR="$(mktemp -d -t cluster-deploy-manifests.XXXXXX)"
manifests_with_image "$REPO/infra/cluster/manifests" "$APPLY_DIR" "$REGISTRY" "$LAST"
KM="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro -v $APPLY_DIR:/manifests:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
echo "cluster-deploy-runner: applying infra/cluster/manifests (boss image pinned to the converged $LAST)"
# SAY WHAT THE APPLY DOES NOT DO. `kubectl apply` with no `--prune` is
# ADDITIVE: it creates and updates the objects the files name and has no
# opinion about anything else. So a manifest DELETED from the tree takes
# its declaration away and leaves the object running, and a reader who
# assumes prune semantics reads this line as a full reconciliation. It
# is not one. The orphan is named by the verify step below instead, and
# the `kubectl delete` is a human step on purpose — see the header of
# infra/lint/a-deleted-manifest-leaves-no-object.sh for why `--prune` is
# refused here (it takes a derived set, and that set contains the
# StatefulSets and PVCs holding the audit log).
echo "cluster-deploy-runner: apply is additive — NO --prune. An object whose manifest was deleted keeps running; the verify step below names it."
$KM apply -f /manifests
rm -rf "$APPLY_DIR"

# StepPlugin bundles converge from the tree too (job d35aec77).
# Code converges in the image, config in the manifests above, schema
# in boss-init — but the `step-plugins` ConfigMap was built by a
# kubectl command someone ran by hand, so it converged with nothing.
# Adding a bundle to infra/step-plugins/ and landing a train delivered
# NOTHING: the row at /system/step-plugins pointed at a file that was
# never mounted, and the step rendered "No plugin registered" with no
# error anywhere. That is how seven seeded plugins came to be active
# with absent bundles, blocking eleven ready review-design steps.
#
# Regenerated from the directory every converge, so the mounted
# bundles are whatever the tree says. --dry-run=client | apply keeps
# it idempotent; README.md is excluded because it is documentation,
# not a bundle. Deliberately NOT committed as a manifest: it is a
# derived artifact whose sources are already in tree, and a committed
# copy would be the second definition that drifts (§9a).
KP="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro -v $REPO/infra/step-plugins:/plugins:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
# `-i`, and that single flag is the whole bug this replaces. The first
# version piped the generated ConfigMap into `$K apply -f -`, but $K is
# `docker run --rm` with no `-i`, so the container never attached
# stdin: apply read an empty document, said "error: no objects passed
# to apply", and the generator died with "write /dev/stdout: broken
# pipe". The runner has failed on every tick since — silently, because
# a failed systemd oneshot notifies nobody — leaving the cluster on the
# placeholder tag committed in boss.yaml while forge main moved on.
#
# The lesson is the one from the zsh/bash mixup earlier the same day:
# a command validated in a different environment than the one that
# runs it has not been validated. I checked the kubectl invocation
# against my own kubectl and never against the docker wrapper it
# actually runs through.
KAPPLY="sudo docker run --rm -i --network host -v $KUBECONFIG_PATH:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
echo "cluster-deploy-runner: converging the step-plugins ConfigMap"
STAGE="configmaps"
PLUGIN_ARGS=""
for f in "$REPO"/infra/step-plugins/*.js; do
    [ -e "$f" ] || continue
    PLUGIN_ARGS="$PLUGIN_ARGS --from-file=$(basename "$f")=/plugins/$(basename "$f")"
done
if [ -n "$PLUGIN_ARGS" ]; then
    # shellcheck disable=SC2086
    $KP create configmap step-plugins -n boss $PLUGIN_ARGS \
        --dry-run=client -o yaml | $KAPPLY apply -f -
else
    echo "cluster-deploy-runner: no bundles in infra/step-plugins — leaving the ConfigMap alone"
fi

# THE GATE RUNNER'S SCRIPT IS THE SECOND INSTANCE OF THE BUG ABOVE.
#
# infra/gate-runner/run.sh reaches the cluster only as the ConfigMap
# gate-runner-script, and that ConfigMap was built by a kubectl command
# written in a COMMENT in gate-runner.yaml and run by hand — so, exactly
# like step-plugins before it, it converged with nothing. Landing a
# train that changes run.sh delivered NOTHING, silently: the gate kept
# running the old script and no error appeared anywhere.
#
# Measured 2026-08-30: car e6f55a36 merged, deployed and arrived, and
# the string it adds appeared twice in the merged run.sh and zero times
# in the live ConfigMap. It sat at `Proven in prod` unprovable, because
# the behaviour it claims was not running (2b69220a).
#
# Same cure as step-plugins, and deliberately NOT a committed manifest:
# it is a derived artifact whose source is already in tree, and a
# committed copy would be the second definition that drifts (§9a).
KG="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro -v $REPO/infra/gate-runner:/gate:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
if [ -f "$REPO/infra/gate-runner/run.sh" ]; then
    echo "cluster-deploy-runner: converging the gate-runner-script ConfigMap"
    $KG create configmap gate-runner-script -n boss-dev \
        --from-file=run.sh=/gate/run.sh \
        --dry-run=client -o yaml | $KAPPLY apply -f -
else
    echo "cluster-deploy-runner: infra/gate-runner/run.sh missing — leaving the ConfigMap alone" >&2
fi

# ONE patch, ONE revision. This used to be `set image` (main container)
# followed by a separate init-container patch — kubectl's `set image`
# cannot reach initContainers — which minted TWO revisions per roll,
# the first carrying a MIXED template (new main, old init). A
# rollback that steps back one revision lands on exactly that
# intermediate; the 2026-09-02 RS table shows the same mixed-template
# class minted by hand during the firefight. Both images move in one
# json patch so every revision in history is a coherent template and
# every rollback target is real.
# Roll to $HEAD; on a boot failure roll back to the last CONVERGED build
# by image name (cluster-deploy-lib.sh roll_deployment) — never to
# "the previous revision", which on 2026-09-05 was the placeholder the
# apply had just created and could not boot.
STAGE="roll $HEAD"
if ! roll_deployment "$K" "$REGISTRY" "$HEAD" "$LAST" "$FAILED_FILE"; then
    exit 1
fi
$K set image -n boss cronjobs -l boss-chore=true "chore=$REGISTRY:$HEAD" || true

# THE CLUSTER-RESIDENT CONDUCTOR runs the same boss image and converges
# its tag here, exactly like the boss deployment above — the manifest
# (boss-conductor.yaml) commits a :latest placeholder and this is where
# the real :$HEAD replaces it. It is a Deployment in boss-dev, so the
# `boss -n boss` patch and the CronJob selector both miss it; it gets its
# own one-line patch. No `rollout status` follows: the conductor SHIPS
# DORMANT (replicas 0) and stays there until an operator's explicit
# cutover, so there is no rollout to wait on and a Ready-check would hang
# on a deployment with no pods. `|| true`: a cluster where this manifest
# has not applied yet (or a scaled-to-0 conductor) must not fail the
# converge. When an operator scales it to 1, it is already pinned to the
# HEAD this converge built.
$K set image -n boss-dev deploy/boss-conductor "conductor=$REGISTRY:$HEAD" || true

echo "$HEAD" > "$STAMP_FILE"
rm -f "$FAILED_FILE"
echo "cluster-deploy-runner: cluster on $REGISTRY:$HEAD"
_stage_done roll_s
# The roll is real from here; a failed verification below is a finding
# about it, not a failed converge — the request reads converged either
# way, and the maintenance packet carries the verification verdict.
OUTCOME="converged=$HEAD"
STAGE="verify manifests"

# THE CONVERGE VERIFIES WHAT IT APPLIED (60690755). Everything above
# is a write; nothing above reads back whether the cluster now holds
# what the tree says. infra/cluster/check-manifests-applied.sh is that
# read — every named object present, and for the kinds where
# present-but-wrong is the realistic failure, contents matching — and
# it was running nowhere with a credential that could see the 41
# objects (the dev-session credential reports 39 unreadable). This is
# the one place with both the tree and the admin kubeconfig, so it
# runs here, AFTER the stamp: the deploy above is real either way, and
# a drift is a finding about it, not a reason to roll it back.
#
# A failure — drift (exit 1) or cannot-verify (exit 2, which is
# 'unknown', not 'clean') — fails this unit, so ExecStopPost closes
# the converge packet "Maintenance failed" with the exit status
# (timers-leave-a-packet): the alarm channel that already exists,
# carrying the check's own report in the journal. It does not retry on
# its own; the next converge — the next main move — re-applies and
# re-checks, which is the honest cadence for a drift.
echo "cluster-deploy-runner: verifying infra/cluster/manifests against the cluster"
check_rc=0
KUBECONFIG="$KUBECONFIG_PATH" "$REPO/infra/cluster/check-manifests-applied.sh" || check_rc=$?
if [ "$check_rc" -ne 0 ]; then
    rc=$check_rc
    echo "cluster-deploy-runner: MANIFESTS CHECK FAILED (rc=$rc) — the tree and the cluster disagree, or the check could not verify; the converge packet stays open until a converge passes it" >&2
    exit 1
fi
echo "cluster-deploy-runner: manifests verified — the cluster holds what the tree declares"
_stage_done verify_s

# AND THE OTHER DIRECTION. check-manifests-applied.sh asks "is every
# DECLARED object present?" — a question no deleted manifest is in, and
# one an orphan answers correctly by being absent from it. The apply
# above does not prune, so the converge's own writes cannot close that
# gap; naming it can. This is the same argument that put the check above
# here: this is the one place with both the tree and a credential that
# can read all 50 objects (the dev-session credential the lint usually
# runs under reports 9 of 16 pairs unreadable), so it is the only place
# the sweep means anything.
#
# It FAILS the unit like the check above, for the reason CLAUDE.md gives
# under "a check nobody reads is a check that is not running": a finding
# demoted to a printed warning in a passing converge is a finding nobody
# reads. An undeclared object in a managed namespace is a real finding
# and the message says the three things it can be.
#
# FIRST ADMIN-REACH RUN. Its exemption set was populated from the dev
# pod's partial view, so this may be the first time anything has
# enumerated live ConfigMaps, Roles, RoleBindings, ServiceAccounts and
# PVCs in `boss`. If it names something nobody has looked at, that is
# the check working: declare it, delete it, or add it to EXEMPT with the
# reason — one car either way. The deploy is already stamped above, so a
# finding here never rolls anything back.
STAGE="check orphans"
echo "cluster-deploy-runner: checking for objects the tree no longer declares"
orphan_rc=0
KUBECONFIG="$KUBECONFIG_PATH" bash "$REPO/infra/lint/a-deleted-manifest-leaves-no-object.sh" || orphan_rc=$?
if [ "$orphan_rc" -ne 0 ]; then
    rc=$orphan_rc
    echo "cluster-deploy-runner: ORPHAN CHECK FAILED (rc=$rc) — something is running that no manifest declares; the apply cannot remove it (no --prune) so the delete is a named human step, printed above" >&2
    exit 1
fi
echo "cluster-deploy-runner: no orphans — nothing is running that the tree cannot account for"
