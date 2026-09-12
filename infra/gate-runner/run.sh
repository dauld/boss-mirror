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

# --- report-back (begin) ---
# THE REPORT RETRIES, because the system of record rolls.
#
# This used to be one call. A gate ends 10-40 minutes after it starts,
# the SoR Recreate-rolls for 30-90s on every train converge, and every
# merge now converges within a minute of landing - so any gate whose
# last minute overlapped a roll made its single report into a dark API,
# printed a WARN, and exited 0. Four greens were lost that way on the
# night of 2026-09-07 (backlog 23188cc5), each re-proven by hand at ~10
# minutes of cluster time. The verdict existed the whole time, in this
# pod's log; nothing was retrying the one call that mattered.
#
# So: `report` is a loop over `report_once`, sleeping GATE_REPORT_BACKOFF
# between attempts (~5 minutes in all, several times a roll), and it
# tells the two failure shapes apart. NOBODY ANSWERED (connection
# refused, timeout, empty reply, 5xx) is a roll and is retried. A REFUSAL
# (any other 4xx, or a step-selection disagreement) is about the write
# itself - a frozen step, a packet this runner does not understand - and
# retrying it for five minutes would only delay the terminal-packet
# branch below. Every attempt is logged, so a reader of the Job log sees
# the roll the runner rode out.
#
# The block is bracketed so boss-testing's gate_runner_report_retry test
# can lift it verbatim and run it against a stub SoR; the pod receives
# exactly one file, so the code cannot live anywhere else.
GATE_REPORT_BACKOFF="${GATE_REPORT_BACKOFF:-5 10 20 30 45 60 60 60}"

# THE REPORT-BACK RECORDS ITS OWN STORY, on the receipt the packet keeps.
#
# Everything the loop below knows about the roll it rode out - how many
# attempts, how many seconds, whether the system of record went dark at
# all - it prints to the pod log, and the pod log is reaped with the Job.
# The packet, which is the record, said only "green". During train #282's
# converge two gates reported cleanly across a dark SoR and there is no
# record anywhere that it happened.
#
# §Diagnosis: "an alarm that reports through its subject dies with it".
# The runner cannot report an outage THROUGH the API that is out - but
# it can carry its own account inside the payload it finally lands, so
# the outage is visible afterwards instead of only while it is happening.
# These three are set by `report` and read by `report_once` at the moment
# of the write, because the story is only true as of the attempt making
# it.
REPORT_ATTEMPT=1
REPORT_WAITED=0
REPORT_UNREACHABLE=false

# Did curl fail because nobody answered? (6 resolve, 7 refused, 28 timed
# out, 35 TLS, 52 empty reply, 55 send, 56 recv.) Those are what a roll
# looks like from a client.
report_transient_curl() { case "$1" in 6|7|28|35|52|55|56) return 0 ;; *) return 1 ;; esac; }

# One attempt. Exit 0 recorded; 75 nobody answered (retry); 1 refused.
report_once() { # verdict, note
    local rc out code body step_id
    # The reporting step is selected by its spec KEY, never its
    # rendered title: the title is prose a registry edit may change
    # on purpose, and matching it kept one fact in two places with
    # nothing holding them together (48bed517 - the old selector
    # grepped for "Record"). `spec_slug` is the same key advancement
    # pairs steps by, exposed on every materialized step row.
    # Exactly one match or refuse LOUDLY: zero means the protocol and
    # this runner disagree, two means the report would land somewhere
    # arbitrary - either way the disagreement goes to the Job log and
    # the packet goes overdue, which is the alarm this rig already
    # defines; silence is the only wrong answer.
    rc=0
    out=$(curl -s --max-time 20 -w '\n%{http_code}' -H "x-boss-user: $ACTOR" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID") || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "gate-runner: report: GET packet failed (curl exit $rc)"
        report_transient_curl "$rc" && return 75
        return 1
    fi
    code=${out##*$'\n'}; body=${out%$'\n'*}
    case "$code" in
        2??) ;;
        5??|000) echo "gate-runner: report: GET packet answered HTTP $code"; return 75 ;;
        *) echo "gate-runner: report: GET packet answered HTTP $code"; return 1 ;;
    esac
    step_id=$(printf '%s' "$body" | python3 -c '
import sys, json
j = json.load(sys.stdin)
j = j.get("data", j)
hits = [s["id"] for s in j["steps"] if s.get("spec_slug") == "record-verdict"]
if len(hits) != 1:
    sys.stderr.write(
        "gate-runner: expected exactly one record-verdict step, found %d"
        " (slugs: %s) - the gate-run protocol and this runner disagree\n"
        % (len(hits), [s.get("spec_slug") for s in j["steps"]]))
    sys.exit(1)
print(hits[0])') || return 1
    # A PER-INVOCATION file, not a fixed /tmp path: the block is lifted
    # verbatim by boss-testing's gate_runner_report_retry and run
    # concurrently there, where a shared name is a race that empties one
    # test's payload under another.
    local payload
    payload=$(mktemp) || return 1
    python3 - "$1" "$2" "$REPORT_ATTEMPT" "$REPORT_WAITED" "$REPORT_UNREACHABLE" <<'PY' > "$payload"
import json, sys
verdict, receipt, attempt, waited, unreachable = sys.argv[1:6]
# The receipt is a JSON STRING on the step (the encoding every reader
# already parses). Annotate it as an object and re-serialize, so the
# report-back's own story rides WITH the gate's findings rather than
# beside them - one document, one parse, and a reader that only wants
# the verdict is unaffected. A receipt that will not parse is passed
# through untouched: an unreadable receipt is evidence too, and wrapping
# it would destroy the only copy.
try:
    body = json.loads(receipt)
    if not isinstance(body, dict):
        raise ValueError("receipt is not an object")
    body["report"] = {"attempts": int(attempt), "waited_s": int(waited),
                      "sor_unreachable": unreachable == "true"}
    receipt = json.dumps(body, separators=(",", ":"))
except Exception:
    pass
print(json.dumps({"status": "completed",
                  "metadata": {"verdict": verdict, "receipt": receipt}}))
PY
    rc=0
    out=$(curl -s --max-time 20 -o /dev/null -w '%{http_code}' -X PUT \
        -H "x-boss-user: $ACTOR" -H "Content-Type: application/json" \
        -d @"$payload" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID/steps/$step_id") || rc=$?
    rm -f "$payload"
    if [ "$rc" -ne 0 ]; then
        echo "gate-runner: report: PUT verdict failed (curl exit $rc)"
        report_transient_curl "$rc" && return 75
        return 1
    fi
    case "$out" in
        2??) return 0 ;;
        5??|000) echo "gate-runner: report: PUT verdict answered HTTP $out"; return 75 ;;
        *) echo "gate-runner: report: PUT verdict answered HTTP $out"; return 1 ;;
    esac
}

report() { # verdict, note
    local attempt=0 rc delay t0=$SECONDS
    REPORT_UNREACHABLE=false
    for delay in $GATE_REPORT_BACKOFF end; do
        attempt=$((attempt + 1))
        REPORT_ATTEMPT=$attempt
        REPORT_WAITED=$((SECONDS - t0))
        rc=0
        report_once "$1" "$2" || rc=$?
        if [ "$rc" -eq 0 ]; then
            echo "gate-runner: verdict $1 recorded on packet $GATE_RUN_JOB_ID (attempt $attempt, $((SECONDS - t0))s)"
            return 0
        fi
        if [ "$rc" -ne 75 ]; then
            echo "gate-runner: report attempt $attempt refused outright - not a roll, not retrying"
            return 1
        fi
        # 75 is "nobody answered", which is the roll. Latch it: the
        # attempt that eventually LANDS is the one carrying the story,
        # and what it has to say is that the SoR was dark for a while -
        # not that it was reachable at the moment of the write.
        REPORT_UNREACHABLE=true
        if [ "$delay" = end ]; then
            echo "gate-runner: report attempt $attempt could not reach the system of record - out of retries after $((SECONDS - t0))s"
            return 1
        fi
        echo "gate-runner: report attempt $attempt could not reach the system of record - retrying in ${delay}s (a deploy rolls it for ~30-90s)"
        sleep "$delay"
    done
}
# --- report-back (end) ---

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
    # --reflink=auto: on one filesystem (the seed is a local PV on the
    # build node's xfs since 5b3dabb5) this shares extents and writes
    # only metadata; anywhere else it falls back to a plain copy. The
    # timing line below is the measurement either way.
    if ( flock -s -w 900 9 && cp -a --reflink=auto "$SEED/target/." /gate-target/target/ ) 9>>"$SEED_LOCK"; then
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

# --- failure detail (begin) ---
# WHAT FAILED, IN THE RECORD THAT OUTLIVES THE POD.
#
# The 2026-09-09 work made the receipt name the failing CHECK: 62 entries
# with a result and a duration each, which is what turned one diagnosis
# into a six-minute read. It stopped one level short. Gate-run 9dc722d2
# recorded verdict=failed with `test` correctly marked the single failure
# and nothing whatsoever about WHICH test - and the check name is not
# what a reader acts on. The failing test name was one grep away in this
# pod's log, and the pod log is REAPED: the receipt is the durable record
# (backlog 4a4d1227).
#
# So the sections this script already pulls out of gate.log for the
# stdout replay are also parsed for the part that has to survive - the
# failing test names, and the panic line with its file:line - and merged
# into the receipt as `fails` before it is reported.
#
# ONE PARSER. The replay and `fails` answer the same question ("what did
# the failed checks say"), so gate.log is read once and the replay text
# is written to a file the block below merely prints (CLAUDE.md §9a: a
# fact that would otherwise live twice). That also moves the old
# `|| tail -200` shell fallback INSIDE the parser, where it can say why
# it fired instead of firing silently.
#
# `fails` IS ALWAYS PRESENT, `[]` when nothing failed. The field was
# absent from every receipt gate.sh writes - so a reader saw `null`,
# while the pre-2026-09-09 four-field digests sitting beside it on older
# packets carried `[]`. "Nothing failed" and "nobody wrote the field"
# must not look the same.
#
# EVERY CAP BELOW IS DELIBERATE AND STATES ITSELF. A suite failing 200
# tests must not write a receipt nobody can read; a cap that silently
# drops the remainder is the 778 KB-log-tailed-to-16 KB defect wearing a
# different hat, so each one carries the count of what it left out.
python3 - "$RECEIPT" /gate-target/gate.log /gate-target/failed-checks.txt <<'PY' || echo "gate-runner: failure-detail extractor crashed - the receipt keeps whatever gate.sh wrote"
import json, os, re, sys
from collections import deque

receipt_path, log_path, replay_path = sys.argv[1], sys.argv[2], sys.argv[3]

REPLAY_TAIL = 300      # lines per failed check replayed to the pod log
PARSE_TAIL = 2000      # lines per failed check this parser reads
RAW_TAIL = 200         # lines of gate.log when nothing can be parsed
PER_CHECK = 5          # named failures per check on the receipt
TOTAL_ENTRIES = 40     # entries on the whole receipt
ENTRY_CHARS = 400      # characters per entry
QUOTE_LINES = 3        # raw lines quoted for a check this cannot parse

try:
    with open(log_path, errors="replace") as fh:
        LOG = [line.rstrip("\n") for line in fh]
except OSError:
    LOG = []


def raw_tail():
    """gate.log's own last words, for the cases nothing can be parsed."""
    if not LOG:
        return ["(gate.log is missing or empty - there is nothing to fall back to)"]
    return ["last %d of %d line(s) of gate.log:" % (min(RAW_TAIL, len(LOG)), len(LOG))] \
        + LOG[-RAW_TAIL:]


def sections(names):
    """The ::group:: block each named check wrote.

    gate.sh brackets every check with `::group::gate: <name>` /
    `::endgroup::`, so the failed sections can be lifted exactly and
    nothing else - a full gate.log is mostly successful build chatter.
    Returns name -> (last PARSE_TAIL lines, TRUE line count), because a
    reduction that cannot say what it dropped is the defect this whole
    block is about. An UNTERMINATED group is kept: that is a timeout, an
    OOM kill, a node reset mid-gate - one of the cases most worth
    explaining.
    """
    wanted = {"::group::gate: %s" % n: n for n in names}
    out, cur, buf, seen = {}, None, None, 0
    for line in LOG:
        if cur is None:
            name = wanted.get(line)
            if name is not None:
                cur, buf, seen = name, deque(maxlen=PARSE_TAIL), 0
        elif line == "::endgroup::":
            out[cur] = (list(buf), seen)
            cur, buf = None, None
        else:
            buf.append(line)
            seen += 1
    if cur is not None:
        out[cur] = (list(buf), seen)
    return out


RE_FAILED = re.compile(r"^test (\S+) \.\.\. FAILED")
RE_STDOUT = re.compile(r"^---- (\S+) stdout ----")
RE_LISTED = re.compile(r"^ {4}(\S+)$")
RE_PANIC_OLD = re.compile(r"^thread '([^']*)' panicked at '(.*)', (\S+)$")
RE_PANIC_NEW = re.compile(r"^thread '([^']*)' panicked at (\S+):$")
RE_ERROR = re.compile(r"^\s*(error(\[E\d{4}\])?|Error|ERROR)\b[: ]")
RE_ARROW = re.compile(r"^\s*--> (\S+)")


def failing_tests(body):
    """Every test cargo said failed, in the order it said so.

    Three statements of the same fact, all real: the per-test
    `test X ... FAILED` line, the `---- X stdout ----` header above each
    panic, and the `failures:` list cargo prints at the end. The list is
    the only one certain to be within PARSE_TAIL of a long suite, so all
    three are read and deduped.
    """
    names, seen, in_list = [], set(), False

    def add(n):
        if n not in seen:
            seen.add(n)
            names.append(n)

    for line in body:
        if line.strip() == "failures:":
            in_list = True
            continue
        m = RE_FAILED.match(line) or RE_STDOUT.match(line)
        if m:
            add(m.group(1))
            in_list = False
            continue
        if in_list:
            m = RE_LISTED.match(line)
            if m:
                add(m.group(1))
            elif line.strip():
                in_list = False
    return names


def panics(body):
    """(test or None, location, message) for each panic cargo printed.

    Attributed to the `---- <test> stdout ----` block it appeared in,
    falling back to the panicking thread's name - which for a plain
    `#[test]` IS the test name. Both cargo panic formats are read: the
    current two-line one (location, then the message) and the older
    single-line `panicked at 'msg', location`.
    """
    out, block = [], None
    for i, line in enumerate(body):
        m = RE_STDOUT.match(line)
        if m:
            block = m.group(1)
            continue
        m = RE_PANIC_OLD.match(line)
        if m:
            out.append((block or m.group(1) or None, m.group(3), m.group(2)))
            continue
        m = RE_PANIC_NEW.match(line)
        if m:
            msg = body[i + 1].strip() if i + 1 < len(body) else ""
            out.append((block or m.group(1) or None, m.group(2), msg))
    return out


def error_lines(body):
    """Error lines with their `-->` location, for checks that are not
    cargo-test-shaped: a compile error, clippy, svelte-check. This is as
    far as the evidence goes - no test name is invented from them."""
    out = []
    for i, line in enumerate(body):
        if RE_ERROR.match(line):
            where = ""
            for nxt in body[i + 1:i + 3]:
                m = RE_ARROW.match(nxt)
                if m:
                    where = " (%s)" % m.group(1)
                    break
            out.append(line.strip() + where)
    return out


RE_UNREACHABLE = re.compile(
    r"Unable to connect|Could not resolve host|Temporary failure in name resolution|"
    r"failed to lookup address|Connection timed out|Network is unreachable|"
    r"ConnectionRefused|ECONNREFUSED|ETIMEDOUT|EAI_AGAIN|"
    r"Couldn't connect to server|Could not connect to|error sending request for url")
# Two ways to say "I could not judge the tree" that must NOT be
# mistaken for "the tree is bad". A local backend refused (127.0.0.1,
# localhost) is the CAR's failure — a mocked suite reaching for a
# service it was never given — and stays a red (478347ad, corrected
# 2026-09-11 after exactly that misreading).
RE_LOCAL = re.compile(r"127\.0\.0\.1|localhost|\[::1\]")


def network_refusal(name, body):
    """`None`, or the reason a failed check is a REFUSAL rather than a
    verdict: its failure lines are connect/resolve errors against
    somewhere off this machine, and NOTHING judged the tree — no failing
    test, no panic, no compile error line that is not itself about the
    network. Gate-run 7522c115 (2026-09-11): 232 bun "Unable to connect"
    lines against registry.npmjs.org, verdict=failed against a branch
    that was never judged; the registry answered two minutes later.
    The receipt's own `fails` said "no cargo test failure" over the
    connect errors — the diagnosis was on the record and nothing acted.
    """
    hits = [l for l in body if RE_UNREACHABLE.search(l) and not RE_LOCAL.search(l)]
    if len(hits) < 3:
        return None
    if failing_tests(body) or panics(body):
        return None
    judged = [e for e in error_lines(body) if not RE_UNREACHABLE.search(e)
              and not re.search(r"InstallFailed|install failed|network", e)]
    if judged:
        return None
    return ("network unreachable during %s: %d connect/resolve error line(s) and no test, "
            "panic or compile failure - the run could not judge the tree; re-gate when the "
            "network answers (first: %s)" % (name, len(hits), hits[0].strip()[:160]))


def clip(entry):
    """One entry, one line, bounded - and saying by how much."""
    entry = " ".join(entry.split())
    if len(entry) <= ENTRY_CHARS:
        return entry
    return entry[:ENTRY_CHARS] + "... (+%d char(s); the Job log replay has the full text)" % (
        len(entry) - ENTRY_CHARS)


def detail(name, got):
    """What `fails` says about one failed check.

    A precedence ladder, and every rung names which one it is standing
    on, so a reader never has to guess whether a line was parsed or
    merely quoted.
    """
    if got is None:
        return ["%s: no ::group:: block in gate.log - it failed before it ran, or gate.sh "
                "changed its grouping and this extractor needs updating" % name]
    body, total = got
    if not body:
        return ["%s: the check produced no output at all" % name]

    out = []
    tests = failing_tests(body)
    found = panics(body)
    where = {}
    for test, loc, msg in found:
        if test is not None and test not in where:
            where[test] = (loc, msg)

    if tests:
        for test in tests[:PER_CHECK]:
            loc, msg = where.get(test, (None, None))
            if loc is None:
                out.append("%s: %s - FAILED, with no panic line for it in this check's "
                           "output" % (name, test))
            else:
                out.append("%s: %s - panicked at %s: %s" % (name, test, loc, msg))
        if len(tests) > PER_CHECK:
            out.append("%s: + %d more failing test(s) not named here (%d failed in all) - the "
                       "Job log replay lists them" % (name, len(tests) - PER_CHECK, len(tests)))
        return out

    loose = [(loc, msg) for _, loc, msg in found]
    if loose:
        for loc, msg in loose[:PER_CHECK]:
            out.append("%s: panicked at %s: %s (no failing test name in this check's "
                       "output)" % (name, loc, msg))
        if len(loose) > PER_CHECK:
            out.append("%s: + %d more panic(s) (%d in all)" % (
                name, len(loose) - PER_CHECK, len(loose)))
        return out

    errs = error_lines(body)
    if errs:
        out.append("%s: no cargo test failure in this check's output; %d error line(s), "
                   "first %d:" % (name, len(errs), min(PER_CHECK, len(errs))))
        out += ["%s: | %s" % (name, e) for e in errs[:PER_CHECK]]
        return out

    quoted = [line for line in body[-QUOTE_LINES:] if line.strip()]
    out.append("%s: nothing this parser recognises - no failing test, no panic, no error line; "
               "last %d of %d line(s) quoted verbatim:" % (name, len(quoted), total))
    out += ["%s: | %s" % (name, line) for line in quoted]
    return out


def write(entries, replay):
    with open(replay_path, "w") as fh:
        fh.write("\n".join(replay) + ("\n" if replay else ""))
    if entries is None:
        return
    if len(entries) > TOTAL_ENTRIES:
        dropped = len(entries) - TOTAL_ENTRIES + 1
        entries = entries[:TOTAL_ENTRIES - 1] + [
            "+ %d more entr%s omitted - this receipt caps `fails` at %d so it stays readable; "
            "the Job log replay has the rest" % (dropped, "y" if dropped == 1 else "ies",
                                                 TOTAL_ENTRIES)]
    RECEIPT["fails"] = [clip(e) for e in entries]
    tmp = receipt_path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(RECEIPT, fh, indent=2)
    os.replace(tmp, receipt_path)


# An unreadable receipt is evidence too, and rewriting it would destroy
# the only copy. Hand over the log's last words and leave the file alone;
# the summary block below reports `verdict: unreadable` on its own.
try:
    with open(receipt_path) as fh:
        RECEIPT = json.load(fh)
    if not isinstance(RECEIPT, dict):
        raise ValueError("receipt is not an object")
except Exception as exc:
    write(None, ["gate-runner: receipt unreadable (%s); falling back to the raw tail" % exc]
          + raw_tail())
    raise SystemExit(0)

checks = RECEIPT.get("checks")
failed = [c.get("name") for c in checks if isinstance(c, dict) and c.get("result") != "pass"] \
    if isinstance(checks, list) else []
failed = [n for n in failed if isinstance(n, str)]

if not failed:
    if RECEIPT.get("verdict") == "green":
        write([], [])
        raise SystemExit(0)
    # A verdict with no failed check means the run ended OUTSIDE a check:
    # the headroom guard refusing, or a crash before the receipt. That is
    # not a consist failure and the record has to be able to say so.
    note = ("no check is marked failed - the run ended outside a check (a headroom refusal, "
            "or a crash before the receipt)")
    if RECEIPT.get("refused_because"):
        note += "; see `refused_because` on this receipt"
    write([note], ["gate-runner: verdict is %s but %s" % (RECEIPT.get("verdict"), note)]
          + raw_tail())
    raise SystemExit(0)

found = sections(failed)
# A run whose EVERY failed check could not reach the network judged
# nothing: the receipt becomes a refusal in the shape gate.sh writes for
# its own disk floor (`verdict: refused`, `refused_because`), so the
# strike rule, the yard and `red_verdict_detail` read it as the
# infrastructure's failure, not the branch's. One judged failure among
# the failed checks keeps the red: a test that failed is a verdict.
refusals = []
for name in failed:
    got = found.get(name)
    why = network_refusal(name, got[0]) if got and got[0] else None
    if why is None:
        refusals = []
        break
    refusals.append(why)
if refusals:
    RECEIPT["verdict"] = "refused"
    RECEIPT["refused_because"] = "; ".join(refusals)
entries, replay = [], []
for name in failed:
    got = found.get(name)
    entries += detail(name, got)
    replay.append("")
    replay.append("----- FAILED: %s -----" % name)
    if got is None:
        replay.append("  (no ::group:: block for this check in gate.log - it failed before it")
        replay.append("   ran, or gate.sh changed its grouping and this extractor needs updating)")
        continue
    body, total = got
    if not body:
        replay.append("  (the check produced no output at all)")
        continue
    tail = body[-REPLAY_TAIL:]
    replay.append("  last %d of %d line(s):" % (len(tail), total))
    replay += ["  " + line for line in tail]
write(entries, replay)
PY
# --- failure detail (end) ---

# --- receipt summary (begin) ---
# THE PACKET KEEPS THE WHOLE RECEIPT.
#
# `infra/gate.sh` writes a rich account of the run — mode, scope, head,
# dirty, host, ci, free_gb, unverifiable[], every check with its result
# and its duration, and `refused_because` when it declined. Each of
# those fields exists because some reader once had to go and re-derive
# it, and its own comment says so.
#
# This block used to REDUCE that to {verdict, head, mode, fails} before
# reporting, and the rest died on /gate-target with gate.log when the
# Job was reaped. Measured on a live gate-run packet (86553d4f): the
# receipt the packet kept was 101 characters. So `boss-jobs`' yard,
# which reads `receipt.checks` to name a red gate's failing check, found
# nothing to read; `dirty`, which train.rs refuses to board on, never
# arrived; and "which check was slow" was unanswerable from the record.
#
# §Diagnosis: a verdict someone must go re-derive is not a verdict. The
# whole receipt costs a few hundred bytes on the packet, which is the
# cheapest evidence in this pipeline.
#
# COMPACT, one line: `gate-runner: receipt $SUMMARY` below is the third
# copy of the verdict — the one `kubectl logs` serves when the PVC and
# the packet do not (cf0021ae) — and a greppable line has to stay one
# line. `fails` is not re-derived here either — the block above already
# merged it into the receipt this reads, and it is NOT a copy of `checks`:
# `checks` says which check failed, `fails` says which TEST and where it
# panicked, which is the part `checks` cannot carry and a reader acts on.
# A `fails` that merely restated the check names would be a fact living
# twice inside one document with nothing holding the two equal
# (CLAUDE.md §9a), which is why it does not.
SUMMARY=$(python3 - "$RECEIPT" "$HEAD_SHA" <<'PY'
import json, sys
try:
    print(json.dumps(json.load(open(sys.argv[1])), separators=(",", ":")))
except Exception as e:
    # gate.sh died before writing a receipt, or wrote something
    # unparseable. That is its own verdict, never a silent green.
    print(json.dumps({"verdict": "unreadable", "head": sys.argv[2],
                      "error": str(e)}, separators=(",", ":")))
PY
)
# --- receipt summary (end) ---
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

REPORTED=0
if report "$VERDICT" "$SUMMARY"; then
    REPORTED=1
else
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
# the failure-detail block above replays exactly the sections that failed
# and nothing else. That distinction is the whole point — a full gate.log
# is mostly successful build chatter and dumping it whole would bury the
# three lines that matter.
#
# THIS IS A PRINTER NOW, not a second parser. The extraction moved up to
# the one block that also writes the receipt's `fails`, because "what did
# the failed checks say" is one fact and it was about to live twice
# (CLAUDE.md §9a). Its `|| tail` belt remains for the case where that
# block did not run at all; every case it DID handle — an unreadable
# receipt, a verdict with no failed check — writes its own explanation
# plus the raw tail into the file, so the fallback says why it fired.
if [ "$VERDICT" != green ]; then
    echo "=== gate-runner: replaying failed checks from gate.log ==="
    cat /gate-target/failed-checks.txt 2>/dev/null || tail -200 /gate-target/gate.log || true
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
         cp -a --reflink=auto /gate-target/target "$SEED/target.partial" &&
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
# AN UNREPORTED VERDICT IS A FAILED RUN. This used to exit on the gate
# verdict alone, so a green gate whose report never landed left a Job
# reading Complete beside a packet that never closed - the exact shape
# a reader mistakes for "nothing happened here" (2026-09-07: four such
# Jobs, each hand-re-gated). The Job status is a claim about the RUN,
# and a run that could not record its result did not finish its job.
# Exit 75 (EX_TEMPFAIL), distinct from a red gate's 1, and one greppable
# line carrying everything the packet should have received. `boss gate
# --wait` already reads a failed Job with a silent packet as "read the
# log, this is NOT a red gate" - so the verdict is not mistaken for red,
# it is found.
if [ "$REPORTED" != 1 ]; then
    echo "gate-runner: UNREPORTED verdict=$VERDICT packet=$GATE_RUN_JOB_ID head=$HEAD_SHA receipt $SUMMARY"
    exit 75
fi
[ "$VERDICT" = green ]
