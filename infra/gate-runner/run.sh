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
    # The reporting step is selected by its spec KEY, never its
    # rendered title: the title is prose a registry edit may change
    # on purpose, and matching it kept one fact in two places with
    # nothing holding them together (48bed517 — the old selector
    # grepped for "Record"). `spec_slug` is the same key advancement
    # pairs steps by, exposed on every materialized step row.
    # Exactly one match or refuse LOUDLY: zero means the protocol and
    # this runner disagree, two means the report would land somewhere
    # arbitrary — either way the disagreement goes to the Job log and
    # the packet goes overdue, which is the alarm this rig already
    # defines; silence is the only wrong answer.
    step_id=$(curl -sf -H "x-boss-user: $ACTOR" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID" \
        | python3 -c '
import sys, json
j = json.load(sys.stdin)
hits = [s["id"] for s in j["steps"] if s.get("spec_slug") == "record-verdict"]
if len(hits) != 1:
    sys.stderr.write(
        "gate-runner: expected exactly one record-verdict step, found %d"
        " (slugs: %s) - the gate-run protocol and this runner disagree\n"
        % (len(hits), [s.get("spec_slug") for s in j["steps"]]))
    sys.exit(1)
print(hits[0])') || return 1
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

# One job, one branch, one PRIVATE disk. /gate-target is a per-run
# emptyDir now: born empty with the pod, dead with it. The wipe
# discipline that used to live here as three rm -rf lines — stale
# target (disk-filling, manufactured failures), stale clone
# ("destination path already exists"), stale receipt (nearly credited
# one branch with another's pass on 2026-08-25) — is structural: there
# is nothing from a previous run to wipe, and no other run can ever
# see this workspace. That structural isolation is what makes
# CONCURRENT gates safe (packet 28de3845); the 2026-08-24 crossed
# receipts needed a shared disk to happen on.
#
# The receipt now dies with the pod, deliberately. Its surviving
# copies are the packet (the record) and this pod's stdout — the
# `gate-runner: receipt` line below, which `kubectl logs` serves for
# ttlSecondsAfterFinished after the Job ends.
#
# /gate-seed is the one shared surface left: the warm target snapshot
# + the crate cache, on the PVC that used to BE the workspace. Reads
# and writes of it are flock-disciplined below.
SEED=/gate-seed
SEED_LOCK="$SEED/.seed.lock"
mkdir -p /gate-target

# SKEW GUARD, the other direction: under an OLD manifest (no /gate-seed
# mount) this pod's /gate-target is still the shared PVC, which
# persists between runs. For exactly that case the old wipe-per-run
# discipline comes back — without it the clone refuses a non-empty
# destination and cross-branch targets overfill the 120Gi volume (the
# three incidents the old rm lines were written for). /gate-target/cargo
# is deliberately NOT wiped: on the old shape it is the persistent
# crate cache, and wiping it would resurrect the crc32fast class.
if [ ! -d "$SEED" ]; then
    rm -rf /gate-target/target /gate-target/repo
    rm -f /gate-target/receipt.json
fi

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
# it is a speed one. CARGO_HOME was unset once, so it defaulted inside
# the container and died with the pod — meaning every gate re-downloaded
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
# So CARGO_HOME lives on the seed volume, which outlives every pod.
# Concurrent gates share it SAFELY without our lock: cargo has locked
# its package cache against concurrent processes since forever — this
# is the same arrangement as N developer builds sharing one ~/.cargo.
# The registry cache is content-addressed by version + checksum, so a
# stale entry cannot produce a wrong build — only a faster one.
#
# The [ -d ] probe is a SKEW guard: a session gating from a stale
# checkout renders the old manifest, which mounts no /gate-seed. That
# run must cost speed, not the gate — cold and loud beats dead.
if [ -d "$SEED" ]; then
    export CARGO_HOME="$SEED/cargo"
else
    echo "gate-runner: /gate-seed is not mounted (manifest older than this script?) — running cold"
    export CARGO_HOME=/gate-target/cargo
fi
mkdir -p "$CARGO_HOME"

# SEED THE TARGET from the warm snapshot. The math this replaces: a
# cold workspace build writes ~74G of target/ and costs 20+ minutes of
# compile (measured; boss-dev.yaml Q1). The seed copy moves the same
# bytes at disk speed — minutes, not tens of minutes — and cargo then
# rebuilds only the workspace crates, which is the ~14-minute warm
# gate this rig is known for. The copy runs under a SHARED flock:
# many seeding readers may overlap freely, but none may overlap the
# refresher rewriting the snapshot (exclusive lock, end of this
# script) — a half-rewritten seed under a reader is how you get
# corrupt rlibs beneath fresh-looking fingerprints, a red that is
# nobody's code. On any failure or a 15-minute lock timeout, fall
# back to a cold build: slow and correct.
mkdir -p /gate-target/target
seed_target() {
    if [ ! -d "$SEED/target" ]; then
        echo "gate-runner: no warm seed at $SEED/target — cold build (~20+ min extra)"
        return 0
    fi
    local t0=$SECONDS
    if ( flock -s -w 900 9 && cp -a "$SEED/target/." /gate-target/target/ ) 9>>"$SEED_LOCK"; then
        echo "gate-runner: target seeded from head $(cat "$SEED/.seed-head" 2>/dev/null || echo '<unrecorded>') in $((SECONDS - t0))s"
    else
        echo "gate-runner: seed copy failed or lock timed out after $((SECONDS - t0))s — cold build instead"
        rm -rf /gate-target/target
        mkdir -p /gate-target/target
    fi
}
if [ -d "$SEED" ]; then seed_target; fi
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
# THE VERDICT GOES IN THE LOG BEFORE IT GOES ANYWHERE ELSE.
#
# It used to live in exactly two places, and on 2026-08-25 both were
# lost at once (cf0021ae): the gate passed 30/30 on
# chore/the-build-leaves-the-control-plane, w-1 rebooted before the
# pod finished, and the receipt survived only on the PVC — it had to
# be recovered by mounting the disk in a throwaway pod. The pod log
# is the third copy, it costs one line, and `kubectl logs` reaches
# it without mounting anything.
echo "gate-runner: receipt $SUMMARY"

if ! report "$VERDICT" "$SUMMARY"; then
    # THE OLD FALLBACK CLAIMED AN ALARM THAT CANNOT ALWAYS FIRE.
    #
    # It said "packet will go overdue (the alarm still works)". That
    # holds only while the packet is OPEN. The case that actually
    # burned us is the other one: a gate-run packet reused across
    # relaunches was already TERMINAL, so the step write was refused
    # AND no overdue can ever be raised against a closed packet. Both
    # channels went quiet together and the run looked like it never
    # happened.
    #
    # So the two cases are told apart and only one of them is
    # reassuring. Neither changes the exit status: this is a failure
    # to RECORD the result, not a failure of the gate, and reporting
    # a green gate as red is the confusion cf0021ae exists about.
    state=$(curl -sf -H "x-boss-user: $ACTOR" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID" \
        | python3 -c 'import sys,json; print(json.load(sys.stdin).get("status","unknown"))' \
        2>/dev/null || echo unreachable)
    echo "WARN: verdict not recorded on packet $GATE_RUN_JOB_ID (packet status: $state)"
    case "$state" in
        open)
            echo "  The packet is still open, so it will go overdue and the alarm will fire."
            ;;
        unreachable)
            echo "  The jobs API could not be reached, so the packet state is unknown."
            echo "  If it was open it will go overdue; if it was not, this log is the only record."
            ;;
        *)
            echo "  THE PACKET IS ALREADY $state, SO NOTHING WILL GO OVERDUE AND NO ALARM"
            echo "  WILL FIRE."
            echo "  A terminal packet cannot accept a verdict on a STEP — file a fresh"
            echo "  gate-run packet rather than reusing one across relaunches (64cae7e9)."
            # BUT IT STILL ACCEPTS METADATA, so the verdict does not have
            # to die with this pod.
            #
            # The lines above have existed since 2026-08-27 and a green
            # run still evaporated on 2026-08-29 (1826ec9f), because a
            # pod log is not a record: the Job is reaped, `kubectl logs`
            # goes with it, and the packet says `lost` for a branch that
            # gated green. The step API refuses a frozen step and names
            # the job-metadata PATCH as the way to annotate instead —
            # verified 2026-08-30 that a CLOSED packet accepts it and
            # that other keys survive the merge.
            #
            # This records; it does not reopen. Reviving a terminal
            # packet would fight the freeze that makes receipts
            # trustworthy, which is why the packet asked for the verdict
            # to be RECOVERABLE rather than automatically re-applied.
            ORPHAN=$(python3 - "$SUMMARY" "$RECEIPT" "$(hostname)" <<'PY'
import json, sys
print(json.dumps({"orphaned_verdict": {
    "receipt": json.loads(sys.argv[1]),
    "receipt_path": sys.argv[2],
    "pod": sys.argv[3],
    "note": "the gate ran to a verdict, but its packet was already terminal "
            "so no step could take it. Recorded here rather than lost with "
            "the pod. The packet's own status is NOT evidence about this run.",
}}))
PY
            )
            if curl -sf -X PATCH -H "x-boss-user: $ACTOR" \
                 -H 'content-type: application/json' -d "$ORPHAN" \
                 "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID/metadata" >/dev/null 2>&1; then
                echo "  RECOVERED: verdict written to the packet as metadata.orphaned_verdict."
            else
                echo "  AND THE METADATA WRITE FAILED TOO — this log is the only record."
            fi
            ;;
    esac
fi

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

# REFRESH THE SEED — the housekeeping that keeps parallel gates warm.
# Runs AFTER the verdict is reported (a refresh must never delay a
# `--wait`), and only from a run whose target is worth inheriting:
#
#   - GREEN only. A red run's target is usually fine (test failures
#     still compile), but a compile-error red would seed broken
#     workspace artifacts, and telling the cases apart buys nothing:
#     green near-tip runs happen many times a day.
#   - AT/NEAR main's tip, measured not felt: every car gates as
#     main + one change, so `rev-list --count HEAD..origin/main` is 0
#     in the common case and small when a train merged mid-gate. Past
#     2 the branch is stale-based and its target would seed the
#     distance to main into every later gate.
#   - EXCLUSIVE, NON-BLOCKING lock. Readers hold the lock shared while
#     copying; a second refresher just skips (-n) — best-effort
#     housekeeping does not queue.
#   - STAGE THEN RENAME. The copy lands in target.partial and is
#     mv-ed into place; a pod that dies mid-refresh (w-1 has reset
#     mid-gate before) leaves a MISSING seed — next gate cold, slow,
#     correct — never a torn one under a fresh-looking marker. The
#     old seed is removed first because two targets (~74G each) do
#     not fit the 120Gi volume; the cold window is the price of
#     fitting, and it only opens on a mid-refresh death.
refresh_seed() {
    if [ ! -d "$SEED" ]; then return 0; fi
    if ! [ "$VERDICT" = "green" ]; then return 0; fi
    git fetch --depth 50 origin "+main:refs/remotes/origin/main" >/dev/null 2>&1 || {
        echo "gate-runner: seed not refreshed — could not re-fetch origin/main"; return 0; }
    local behind
    # rev-list prints a count or fails (shallow clone, no merge base
    # within depth) — 999 makes "cannot measure" read as "too far".
    behind=$(git rev-list --count HEAD..origin/main 2>/dev/null || echo 999)
    if [ "$behind" -gt 2 ]; then
        echo "gate-runner: seed not refreshed — HEAD is $behind commit(s) behind origin/main"
        return 0
    fi
    if [ "$(cat "$SEED/.seed-head" 2>/dev/null || true)" = "$HEAD_SHA" ]; then
        echo "gate-runner: seed already at $HEAD_SHA — not refreshed"
        return 0
    fi
    local t0=$SECONDS
    if ( flock -x -n 9 &&
         rm -f "$SEED/.seed-head" &&
         rm -rf "$SEED/target" "$SEED/target.partial" &&
         cp -a /gate-target/target "$SEED/target.partial" &&
         mv "$SEED/target.partial" "$SEED/target" &&
         echo "$HEAD_SHA" > "$SEED/.seed-head"
       ) 9>>"$SEED_LOCK"; then
        echo "gate-runner: seed refreshed to $HEAD_SHA in $((SECONDS - t0))s"
    else
        echo "gate-runner: seed refresh skipped (another writer holds the lock) or failed after $((SECONDS - t0))s — the previous seed stands"
    fi
}
refresh_seed || true

tail -5 /gate-target/gate.log || true
echo "gate-runner: $GATE_BRANCH@${HEAD_SHA:0:10} -> $VERDICT"
[ "$VERDICT" = green ]
