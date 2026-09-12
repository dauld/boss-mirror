#!/usr/bin/env bash
# a-build-never-shares-a-node-with-etcd.sh — the placement rule the
# 2026-09-11 outage taught: nothing that BUILDS may share a node with
# anything that fsyncs for the cluster.
#
# WHY. Incident a398c4d3: cp-2 — a control-plane node, so an etcd member
# — went NotReady for 21 minutes and took w-1 with it (KubePrism routed
# w-1's kubelet to cp-2's apiserver). The mechanism was the disk: image
# layer extraction on a pod admission, on a DRAM-less PM991a that also
# held etcd's WAL, stalled etcd, containerd and the kubelet together
# (/proc/pressure/io full ≈ 6.7 h on that node; CPU full 0). The SoR's
# own roll was that admission — and the-sor-runs-on-control-plane pins
# it there on purpose. The pipeline's write sinks (a gate's ~74 GB
# workspace, the seed refresh, every dev-pod builder) were only ever
# PREFERRED onto the build node; nothing forbade them a control plane.
# Two rules, then, and this lint states the second half:
#   1. cluster-state workloads pin to control-plane nodes
#      (the-sor-runs-on-control-plane.sh — unchanged);
#   2. BUILD workloads are forbidden control-plane nodes: a REQUIRED
#      nodeAffinity term `node-role.kubernetes.io/control-plane
#      DoesNotExist`, beside whatever preference they carry. A
#      preference is a hint the scheduler drops under pressure; the
#      outage is the day it would drop it.
#
# WHAT IT CHECKS. Every manifest listed in BUILD_WORKLOADS carries the
# required term verbatim. A manifest that cannot carry it yet is EXEMPT
# with the reason and the car that owes it — never silently.
#
# Runs on every gate; needs nothing but the tree.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

# The workloads that build: each writes tens of GB per run.
BUILD_WORKLOADS=(
    "infra/gate-runner/gate-runner.yaml"
    "infra/cluster/manifests/boss-dev.yaml"
)
# file<TAB>reason. Every entry is a car somebody owes.
EXEMPT=(
    "infra/cluster/manifests/boss-dev.yaml	a car editing this file rolls the dev pod on converge and ends the live operator session, so it lands only at a David-timed restart (boss-dev-manifest-cars-restart-the-session). The term to add is the same one gate-runner.yaml carries; until then the dev pod only PREFERS the build node."
)
TERM='{key: node-role.kubernetes.io/control-plane, operator: DoesNotExist}'

fail=0
for entry in "${EXEMPT[@]}"; do
    f="${entry%%	*}"
    [ -f "$f" ] || { echo "a-build-never-shares-a-node-with-etcd: STALE EXEMPTION — $f does not exist" >&2; fail=1; }
done
checked=0
for f in "${BUILD_WORKLOADS[@]}"; do
    [ -f "$f" ] || { echo "a-build-never-shares-a-node-with-etcd: $f is missing — the roster names a build workload the tree does not have" >&2; fail=1; continue; }
    exempt=0
    for entry in "${EXEMPT[@]}"; do [ "${entry%%	*}" = "$f" ] && exempt=1; done
    if [ "$exempt" -eq 1 ]; then continue; fi
    checked=$((checked + 1))
    if ! grep -qF "$TERM" "$f"; then
        echo "a-build-never-shares-a-node-with-etcd: $f builds but may schedule on a control-plane node." >&2
        echo "    Add, under the pod's affinity.nodeAffinity, a REQUIRED term:" >&2
        echo "      requiredDuringSchedulingIgnoredDuringExecution:" >&2
        echo "        nodeSelectorTerms:" >&2
        echo "          - matchExpressions:" >&2
        echo "              - $TERM" >&2
        echo "    A preference is dropped under pressure; the day it is dropped is the outage (a398c4d3)." >&2
        fail=1
    elif ! grep -qF 'requiredDuringSchedulingIgnoredDuringExecution' "$f"; then
        echo "a-build-never-shares-a-node-with-etcd: $f names the term but not under a REQUIRED affinity — a preferred term is a hint, not a rule." >&2
        fail=1
    fi
done
[ "$checked" -ge 1 ] || { echo "a-build-never-shares-a-node-with-etcd: every build workload is exempt — the lint has no subject" >&2; fail=1; }
if [ "$fail" -ne 0 ]; then exit 1; fi
echo "a-build-never-shares-a-node-with-etcd: ok — $checked build workload(s) forbid control-plane nodes by a required term (${#EXEMPT[@]} exempt, each with a reason and the car it owes)"
