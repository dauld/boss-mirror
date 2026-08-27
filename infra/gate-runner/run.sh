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
# The disk is a PVC and outlives the Job. The clone below refuses a
# non-empty destination, so a second run on the same disk died with
# "destination path already exists" - the third reason this rig had
# never completed a run. "Wiped per run" has to include the clone.
rm -rf /gate-target/repo
# And the previous run's receipt. It is written only at the END of a
# run, so if this one dies partway the old verdict is still sitting
# there - with a different head - looking exactly like this run's
# result. That nearly credited one branch with another branch's pass
# on 2026-08-25 when w-1 reset mid-gate.
rm -f /gate-target/receipt.json

# Forge auth. The repo is not anonymously clonable: a bare clone dies
# with "could not read Username for http://...", which is the error
# dev-node-checkout.md called the last blocker. The token arrives as a
# FILE (secret forge-read, key token, mounted at /etc/forge) and is
# read by a credential helper rather than interpolated into the URL —
# argv is world-readable to anything sharing the pid namespace, and a
# token in the clone URL also lands in .git/config on the disk.
if [ -r /etc/forge/token ]; then
    git config --global credential.helper \
        '!f() { echo username=x-access-token; echo "password=$(cat /etc/forge/token)"; }; f'
else
    echo "gate-runner: /etc/forge/token missing - the clone will fail" >&2
fi

# Own clone: no dependency on the dev pod's PVC, so this Job schedules
# wherever its nodeSelector says — deliberately NOT the etcd node.
git clone --depth 50 "$FORGE_URL" /gate-target/repo
cd /gate-target/repo
# Explicit refspec. `git fetch origin <branch>` on a shallow clone
# updates FETCH_HEAD but creates no remote-tracking ref, so the
# checkout below died with "origin/<branch> is not a commit" - the
# second reason this rig had never completed a run.
git fetch origin "$GATE_BRANCH:refs/remotes/origin/$GATE_BRANCH"
git checkout -B "$GATE_BRANCH" "origin/$GATE_BRANCH"
HEAD_SHA=$(git rev-parse HEAD)

export CARGO_TARGET_DIR=/gate-target/target
# THE CRATE CACHE SURVIVES THE RUN, and it is a correctness fix before
# it is a speed one. CARGO_HOME was unset, so it defaulted inside the
# container and died with the pod — meaning every gate re-downloaded
# every dependency from static.crates.io, and every gate was therefore
# betting its verdict on several hundred consecutive successful fetches
# over a link that is measurably not that reliable.
#
# That bet lost on 2026-08-27 05:51Z: `clippy` was recorded as FAILED on
# fix/a-dropped-lookup-does-not-red-a-train when the real log said
#   error: failed to download from `https://static.crates.io/.../crc32fast`
#   Caused by: [7] Could not connect to server
# Every other check on that branch passed. A green branch was called red
# by the network, which is the same class of fault as the kaniko DNS
# break that cancelled a six-car train — and it is the likeliest
# explanation for the unexplainable clippy/fixture reds in backlog
# 9c7ed804, none of which reproduced by hand.
#
# `target/` is still wiped per run on purpose (two branches' targets do
# not fit on one 120Gi volume). The registry cache is a few GB and is
# content-addressed by version + checksum, so a stale entry cannot
# produce a wrong build — only a faster one.
export CARGO_HOME=/gate-target/cargo
mkdir -p "$CARGO_HOME"
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

# WHY THE FAILING OUTPUT IS REPLAYED TO STDOUT. gate.log is written to
# /gate-target, which only the gate container mounts — not the postgres
# sidecar, not any later Job. When this container exits, the last reader
# of that file is gone. So a failure that cost an hour to produce left a
# receipt naming WHICH check failed and no way whatsoever to learn WHY.
#
# That is not hypothetical: three branches were called red by this rig
# and then passed every one of those same checks run by hand, and no
# theory could be tested because the evidence died with the pod
# (backlog 9c7ed804). A verdict nobody can explain is barely better
# than no verdict — it teaches the reader to distrust the gate.
#
# stdout is the one surface that outlives the container: `kubectl logs`
# serves a terminated pod for as long as the Job exists. gate.sh already
# brackets every check with `::group::gate: <name>` / `::endgroup::`, so
# we can replay exactly the sections that failed and nothing else. That
# distinction is the whole point — a full gate.log is mostly successful
# build chatter and dumping it whole would bury the three lines that
# matter.
if [ "$VERDICT" != green ]; then
    echo "=== gate-runner: replaying failed checks from gate.log ==="
    python3 - "$RECEIPT" /gate-target/gate.log <<'PY' || tail -200 /gate-target/gate.log || true
import json, sys

receipt_path, log_path = sys.argv[1], sys.argv[2]
# Per-check cap. A check that fails by producing 200k lines must not
# push the earlier failures out of the reader's scrollback.
TAIL = 300

try:
    receipt = json.load(open(receipt_path))
    failed = [c["name"] for c in receipt["checks"] if c["result"] != "pass"]
except Exception as e:
    # No receipt means gate.sh died before writing one, which is itself
    # the interesting case. Fall through to the shell's tail.
    print("gate-runner: receipt unreadable (%s); falling back to raw tail" % e)
    raise SystemExit(1)

if not failed:
    print("gate-runner: verdict is not green but no check is marked failed —")
    print("  the run died outside a check (headroom guard, or a crash before the receipt).")
    raise SystemExit(1)

wanted = {"::group::gate: %s" % name: name for name in failed}
sections, current, buf = {}, None, []
with open(log_path, errors="replace") as fh:
    for line in fh:
        stripped = line.rstrip("\n")
        if current is None:
            name = wanted.get(stripped)
            if name is not None:
                current, buf = name, []
        elif stripped == "::endgroup::":
            sections[current] = buf[-TAIL:]
            current, buf = None, []
        else:
            buf.append(stripped)
# An unterminated group means the check was still running when the log
# ended — a timeout or a kill. Keep what it managed to say.
if current is not None:
    sections[current] = buf[-TAIL:]

for name in failed:
    body = sections.get(name)
    print("\n----- FAILED: %s -----" % name)
    if body is None:
        print("  (no ::group:: block for this check in gate.log — it failed before it ran,")
        print("   or gate.sh changed its grouping and this extractor needs updating)")
        continue
    if not body:
        print("  (the check produced no output at all)")
        continue
    print("  last %d of %d lines:" % (min(TAIL, len(body)), len(body)))
    for line in body:
        print("  " + line)
PY
    echo "=== end of failed-check replay ==="
fi

tail -5 /gate-target/gate.log || true
echo "gate-runner: $GATE_BRANCH@${HEAD_SHA:0:10} -> $VERDICT"
[ "$VERDICT" = green ]
