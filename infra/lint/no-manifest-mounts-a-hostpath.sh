#!/usr/bin/env bash
#
# no-manifest-mounts-a-hostpath — no pod this tree declares mounts the
# node's filesystem, because the cluster refuses the mount and the gate
# cannot see the refusal.
#
# WHAT HAPPENED (backlog d42d4967, 2026-09-12). gate-seed-local.yaml
# landed on #340 with a prepare CronJob whose hostPath mount
# (DirectoryOrCreate) was the one thing that would create the local
# PV's directory on w-1. Its manifest comment reasoned that boss-dev
# "carries no pod-security enforce label", so the mount was admitted.
# The label IS absent — and that is exactly why the namespace takes the
# Talos machine-config default, `enforce: baseline`, which forbids
# hostPath volumes. Run by hand, the Job created NO pod in ten minutes
# and no condition; its twin with `emptyDir: {}` in the same slot
# completed in twenty seconds. Nothing in the cluster could then make
# the directory, so the first gate after the train would have hung at
# MountVolume.SetUp — every car blocked, by a manifest the gate had
# passed green, because the gate applies nothing and the only reader of
# an admission verdict is the API server.
#
# WHAT IS ASSERTED. No file under infra/cluster/manifests/ or
# infra/gate-runner/ contains a `hostPath:` volume. Every namespace this
# tree declares is baseline or stricter (boss.yaml labels `boss`
# baseline; boss-dev inherits the same default), so a hostPath anywhere
# here is a pod that will never be admitted. The CronJob it caught is
# gone; the directory is the node's own declaration (Talos
# `machine.files` + `machine.kubelet.extraMounts` on w-1 — see gate-seed-local.yaml).
#
# There is no exemption list. A workload that truly needs the node's
# filesystem needs a namespace labelled `privileged` first, and that is
# a decision with its own car; the day it is taken, this lint learns the
# namespace, not the file.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

ROOTS=(infra/cluster/manifests infra/gate-runner)

hits="$(grep -rn --include='*.yaml' -E '^\s*(- )?hostPath:' "${ROOTS[@]}" 2>/dev/null || true)"
if [ -n "$hits" ]; then
    echo "no-manifest-mounts-a-hostpath: a pod in this tree mounts the node's filesystem, and every namespace here enforces baseline, which refuses hostPath — the pod will never be admitted, and the gate cannot tell you (d42d4967):" >&2
    echo "$hits" | sed 's/^/    /' >&2
    echo "    A directory on a node is the NODE's declaration (Talos machine.files + kubelet.extraMounts); a file the pod needs rides a PVC, a ConfigMap or a Secret." >&2
    exit 1
fi
n="$(find "${ROOTS[@]}" -name '*.yaml' | wc -l | tr -d ' ')"
echo "no-manifest-mounts-a-hostpath: ok — $n manifest(s), no hostPath volume"
