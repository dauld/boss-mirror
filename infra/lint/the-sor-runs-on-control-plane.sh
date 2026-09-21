#!/usr/bin/env bash
# The SoR stack schedules on control-plane, not a flaky worker.
#
# 2026-09-06 (backlog 90932d98): the boss app Deployment carried no
# nodeSelector, so the scheduler put it on worker w-1. When w-1 shed
# pods, the SoR was evicted to another node and :7900 went dark for
# minutes on a Multi-Attach stall of its RWO PVC. Its data StatefulSets
# were already pinned to control-plane; the app was the gap. This pins
# the invariant so an unpinned boss workload cannot creep back in — a
# config-channel check the software gate would otherwise miss.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3
f="infra/cluster/manifests/boss.yaml"
[ -f "$f" ] || { echo "sor-control-plane: $f is missing"; exit 1; }

# `|| true` ON BOTH COUNTS, and the reason is the whole point of this
# lint. `grep -c` prints 0 and exits 1 when nothing matches, and under
# the `set -e` above that kills the script even though the count is
# captured — the assignment takes grep's status as its own. Zero pins
# is EXACTLY the condition this lint exists to catch (90932d98), so
# without these the script died on line 19 in its own worst case and
# the diagnostic below never printed: failing closed, but silently,
# which CLAUDE.md §Diagnosis calls a verdict nobody can act on
# (backlog 4c9733f7).
workloads=$(grep -cE '^kind: (Deployment|StatefulSet)' "$f" || true)
pinned=$(grep -c 'node-role.kubernetes.io/control-plane' "$f" || true)
if [ "$pinned" -lt "$workloads" ]; then
    echo "sor-control-plane: $f has $workloads workload(s) but only $pinned"
    echo "  control-plane nodeSelector(s). A boss workload without it can land on a"
    echo "  flaky worker (w-1) and take the SoR down when that node sheds pods (90932d98)."
    echo "  Pin every Deployment/StatefulSet:"
    echo "      nodeSelector:"
    echo "        node-role.kubernetes.io/control-plane: \"\""
    exit 1
fi
lint_scanned the-sor-runs-on-control-plane "$workloads" "workload(s) in $f"
echo "sor-control-plane: OK — all $workloads boss workload(s) pin to control-plane"
