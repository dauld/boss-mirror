#!/usr/bin/env bash
# gate-runner/run.sh — one gate, one Job, self-reporting.
#
# WHY THIS EXISTS. On 2026-08-22/23 gates ran as tmux trees inside the
# boss-dev pod and died four different deaths, none their own fault:
# a converge replaced the pod (twice), container limits made exec
# scopes reap the tmux server, a shared 100Gi target filled and turned
# "No space left on device" into fake code failures, and a cold
# container's first webServer boot mass-failed a mocked suite in 20s.
# Each death was reconstructed from journals after a human asked "how
# are we looking". This script is the other shape: a Kubernetes Job
# with its own clone, its own disk, a database sidecar, and a receipt
# it reports to the gate-run packet itself — so the SoR knows the
# verdict without anyone grepping a pod.
#
# Runs inside the boss-ci image (see gate-runner.yaml). Required env:
#   GATE_BRANCH        branch to gate (fetched from the forge)
#   GATE_RUN_JOB_ID    the gate-run packet this run reports to
# Optional:
#   GATE_MODE          "--auto" for scoped gates, empty for full
#   FORGE_URL          default http://10.20.0.15:3000/david/boss.git
#   JOBS_API           default http://10.20.0.34:7900
set -euo pipefail

FORGE_URL="${FORGE_URL:-http://10.20.0.15:3000/david/boss.git}"
JOBS_API="${JOBS_API:-http://10.20.0.34:7900}"
ACTOR='{"id":"automation:gate-runner","role":"platform-admin","access_tier":"operator"}'

report() { # verdict, note
    local step_id
    step_id=$(curl -sf -H "x-boss-user: $ACTOR" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID" \
        | python3 -c 'import sys,json;j=json.load(sys.stdin);print([s["id"] for s in j["steps"] if "Record" in (s.get("title") or "")][0])') || return 1
    python3 - "$1" "$2" <<'PY' > /tmp/verdict.json
import json, sys
print(json.dumps({"status": "completed",
                  "metadata": {"verdict": sys.argv[1], "receipt": sys.argv[2]}}))
PY
    curl -sf -o /dev/null -X PUT -H "x-boss-user: $ACTOR" \
        -H "Content-Type: application/json" -d @/tmp/verdict.json \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID/steps/$step_id"
}

# The run itself is guarded so ANY failure below still reports `lost`
# with the reason, rather than leaving the packet to go overdue.
fail_lost() { report lost "runner died before a receipt: $1" || true; exit 1; }
trap 'fail_lost "line $LINENO"' ERR

# One job, one branch, one clean disk. A cold workspace build needs
# ~74G; sharing a warm target between branches is what filled the
# disk mid-run and manufactured failures.
rm -rf /gate-target/target
mkdir -p /gate-target/target

# Own clone: no dependency on the dev pod's PVC, so this Job schedules
# wherever its nodeSelector says — deliberately NOT the etcd node.
git clone --depth 50 "$FORGE_URL" /gate-target/repo
cd /gate-target/repo
git fetch origin "$GATE_BRANCH"
git checkout -B "$GATE_BRANCH" "origin/$GATE_BRANCH"
HEAD_SHA=$(git rev-parse HEAD)

export CARGO_TARGET_DIR=/gate-target/target
# Build parallelism follows the CPU the container was actually GIVEN.
# It was pinned at 4, so raising the gate ceiling from 6 CPU to 20 in
# the build-node car bought nothing measurable: the gate was never
# bound at 20, it was bound at 4, and sixteen cores sat idle. A 20-CPU
# run on w-1 took 47 minutes against 40 on a 6-CPU control plane,
# which is what sent me looking.
#
# Read the CGROUP QUOTA, not nproc: inside a container nproc reports
# the node's core count (32 on w-1), not the slice the container may
# use, so it would oversubscribe by 60% here.
gate_cpus() {
    local q p
    if [ -r /sys/fs/cgroup/cpu.max ]; then
        read -r q p < /sys/fs/cgroup/cpu.max
        if [ "$q" != "max" ] && [ -n "$p" ] && [ "$p" -gt 0 ] 2>/dev/null; then
            echo $(( (q + p - 1) / p ))
            return
        fi
    fi
    nproc 2>/dev/null || echo 4
}
CPUS=$(gate_cpus)
[ "${CPUS:-0}" -ge 1 ] 2>/dev/null || CPUS=4

# RUST_TEST_THREADS stays at 2 ON PURPOSE. The suites share one
# postgres sidecar, and parallel DB-backed tests are what produced the
# /dev/shm exhaustion and the schema-load race this rig has already
# been bitten by. Raising both at once would also make the result
# unattributable. Widen it as a separate, measured change.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$CPUS}" RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"
echo "gate-runner: building ${CARGO_BUILD_JOBS}-wide (cgroup quota), tests 2-wide"

# Warm the web toolchain: the mocked suite's webServer boot on a cold
# container exceeded its timeout three times on 2026-08-23; every spec
# then fails on connect within seconds. (The durable fix is the
# config-side timeout car; this keeps the first boot off the clock.)
(cd apps/web && bun install --frozen-lockfile >/dev/null 2>&1 && bun run build >/dev/null 2>&1) || true

RECEIPT=/gate-target/receipt.json
if BOSS_GATE_RECEIPT="$RECEIPT" ./infra/gate.sh ${GATE_MODE:-} > /gate-target/gate.log 2>&1; then
    VERDICT=green
else
    VERDICT=failed
fi
trap - ERR

SUMMARY=$(python3 - "$RECEIPT" "$HEAD_SHA" <<'PY'
import json, sys
try:
    r = json.load(open(sys.argv[1]))
    fails = [c["name"] for c in r["checks"] if c["result"] != "pass"]
    print(json.dumps({"verdict": r["verdict"], "head": r["head"],
                      "mode": r.get("mode"), "fails": fails}))
except Exception as e:
    print(json.dumps({"verdict": "unreadable", "head": sys.argv[2],
                      "error": str(e)}))
PY
)
report "$VERDICT" "$SUMMARY" || echo "WARN: verdict not reported — packet will go overdue (the alarm still works)"
tail -5 /gate-target/gate.log || true
echo "gate-runner: $GATE_BRANCH@${HEAD_SHA:0:10} -> $VERDICT"
[ "$VERDICT" = green ]
