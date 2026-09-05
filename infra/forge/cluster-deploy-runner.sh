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
trap 'rm -f "$BOSS_RUNNER_SNAPSHOT" ${BOSS_RUNNER_SNAPSHOT2:+"$BOSS_RUNNER_SNAPSHOT2"}' EXIT

REPO="${BOSS_FORGE_REPO_DIR:-$HOME/boss}"
REGISTRY="${BOSS_FORGE_REGISTRY:-10.20.0.15:3000/david/boss}"
KUBECONFIG_PATH="${BOSS_FORGE_KUBECONFIG:-$HOME/kc.yaml}"
STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-$HOME/.boss-last-built}"
# The quarantine stamp for a head whose BOOT failed (rollout never went
# Ready and was rolled back). Distinct from STAMP_FILE — that one means
# "converged", this one means "proven unbootable; do not re-roll it".
FAILED_FILE="${BOSS_FORGE_LAST_FAILED:-$HOME/.boss-last-failed}"
export DOCKER_HOST="${DOCKER_HOST:-unix:///run/user/1000/docker.sock}"

cd "$REPO"
git fetch -q forgejo main
HEAD=$(git rev-parse --short forgejo/main)
LAST=$(cat "$STAMP_FILE" 2>/dev/null || echo none)

if [ "$HEAD" = "$LAST" ]; then
    echo "cluster-deploy-runner: forge main unchanged ($HEAD)"
    exit 0
fi

# A head that bricked its boot stays quarantined until main moves.
# Without this, the 2026-09-02 shape loops forever: rollout fails,
# set -e kills the run before the stamp, and the next tick rebuilds
# and re-rolls the SAME brick every ten minutes — which is exactly
# what the runner spent the 20:15-21:10 outage doing. Exit NONZERO:
# a held converge is a failed unit somebody can see, not a quiet pass.
if [ "$HEAD" = "$(cat "$FAILED_FILE" 2>/dev/null || echo none)" ]; then
    echo "cluster-deploy-runner: $HEAD bricked its boot on a previous converge — holding until main moves (rm $FAILED_FILE to retry it)" >&2
    exit 1
fi

# An operator's hold (converge-hold.sh, the hold-converge ops verb)
# stops the roll before the build: main has moved, and a human said not
# yet. Loud on every tick, exit 0 — a hold is a decision, not a failure.
. "$REPO/infra/forge/cluster-deploy-lib.sh"
if reason=$(converge_held "${BOSS_CONVERGE_HOLD:-/var/tmp/boss-converge-hold}"); then
    echo "cluster-deploy-runner: converge HELD — $reason — main at $HEAD not built or rolled (release-converge lifts it)" >&2
    exit 0
fi
echo "cluster-deploy-runner: forge main moved $LAST -> $HEAD; building"
git checkout -q "$HEAD" 2>/dev/null || git checkout -qf "$HEAD"

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
# The FULL commit rides into the binaries (Capabilities.commit) so the
# conductor's `converged` step can verify the running pod serves this
# exact merge — the short tag stays the image name, the full sha is
# the attestation (prefix-compared, so either length matches).
docker build -q -f infra/oss-quickstart/Dockerfile \
    --build-arg BOSS_BUILD_COMMIT="$(git rev-parse HEAD)" \
    -t "$REGISTRY:$HEAD" .
docker push "$REGISTRY:$HEAD"

# The image proves it can boot before it goes anywhere near the cluster
# (cluster-deploy-lib.sh image_boots): its own launcher checks that
# every file it sources is beside it. A head that fails here is
# quarantined like a head that failed on the cluster — with no dark
# window at all, because nothing was applied.
. "$REPO/infra/forge/cluster-deploy-lib.sh"
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
APPLY_DIR="$(mktemp -d -t cluster-deploy-manifests.XXXXXX)"
manifests_with_image "$REPO/infra/cluster/manifests" "$APPLY_DIR" "$REGISTRY" "$LAST"
KM="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro -v $APPLY_DIR:/manifests:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
echo "cluster-deploy-runner: applying infra/cluster/manifests (boss image pinned to the converged $LAST)"
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
# 'unknown', not 'clean') — fails this unit, so ExecStartPost never
# closes the converge packet and it stays open on the fleet view
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
