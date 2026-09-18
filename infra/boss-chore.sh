#!/usr/bin/env bash
#
# boss-chore — run an in-cluster chore and record its verdict on BOTH
# legs (backlog 480e183c, 2026-09-18).
#
#   boss-chore.sh <kind> "<title>" -- <check> [args...]
#   boss-chore.sh maintenance-audit-integrity "Audit-log integrity check" -- boss-audit-integrity-check
#
# A CronJob's ExecStopPost. The bare-metal timers record a failed run
# through systemd (boss-step.sh reads $SERVICE_RESULT from ExecStopPost=,
# which runs whether ExecStart succeeded or not — the 2026-09-05 fix its
# header describes). A Kubernetes CronJob has no ExecStopPost, and the
# chores that replaced those timers carried this instead, each its own
# copy, under `set -euo pipefail`:
#
#     boss-maintenance-wrap.sh <kind> "<title>"
#     <the check>
#     boss-step.sh <kind> run result=ok
#
# When the check exits nonzero the script ends on line two and line
# three never runs: the packet stays OPEN, looking exactly like a run
# in progress. For a sweep whose purpose is to surface a violated
# invariant, an open packet is silence — the shape the wrap's header
# names, one layer over. Measured 2026-09-18: eight of the ten CronJobs
# under infra/cluster/manifests carried that dance, none recorded a
# failure. This script is the one copy.
#
# WHAT IT DOES, in order:
#   1. opens (or reuses) the packet through boss-maintenance-wrap.sh —
#      BEST-EFFORT. The packet is visibility, never a precondition
#      (CLAUDE.md §Diagnosis, "an arm that needs the patient is not an
#      arm"): a wrap that cannot reach the system of record, or is
#      refused by it, is said out loud and the check runs anyway.
#   2. runs the check with stdout+stderr captured WHOLE to a file and
#      streamed to the container log as it goes — never a tail
#      (§Diagnosis: a reduction before storing discards the only copy;
#      the pod log keeps the whole, the packet gets an excerpt).
#   3. records the verdict through boss-step.sh: `result=ok` or
#      `result=failed`, `exit_status=<rc>` (the key the systemd leg uses,
#      so both legs read alike), and `output=` — the check's own words,
#      head and tail of the capture with the omitted count named.
#      BEST-EFFORT too: a lost HTTP call must not report a good run as
#      failed, nor spend backoffLimit re-running a whole check for it.
#   4. exits with the CHECK's status, so the Kubernetes Job shows Failed
#      when the check failed — whatever the packet could be told.
#
# The `--` is load-bearing: the title carries spaces, and everything
# after the separator is one argv handed to exec, not re-parsed. A
# check that needs shell (two sweeps, a conditional) is `bash -c '…'`
# after the `--`, written in the manifest where it can be read.
#
# BOSS_JOBS_URL is read by the two helpers, never here; their refusal
# without it (exit 78, no localhost default — the 2026-08-17 lesson in
# their headers) is one of the failures this script logs and goes past.
# The check still runs; only its visibility is lost.
set -euo pipefail

me=$(basename "$0")
usage() {
    echo "usage: $me <kind> \"<title>\" -- <check> [args...]" >&2
    exit 2
}
[ $# -ge 4 ] || usage
KIND="$1"; TITLE="$2"; shift 2
[ "$1" = "--" ] || usage
shift
[ $# -ge 1 ] || usage

# The helpers resolve next-to-self — the image keeps the three together
# in /usr/local/bin, a checkout keeps them together in infra/ — with
# PATH as the fallback, the way boss-step.sh finds boss-api-curl.sh.
here=$(dirname "$0")
WRAP="$here/boss-maintenance-wrap.sh"; [ -x "$WRAP" ] || WRAP=boss-maintenance-wrap.sh
STEP="$here/boss-step.sh";             [ -x "$STEP" ] || STEP=boss-step.sh

# How much of the capture rides the packet. A step's metadata is a
# record, not a log: the first lines say what the run was doing, the
# last say how it ended, and the pod log holds the rest. Long lines are
# cut too, so one pathological line cannot outgrow the request body.
HEAD_LINES=20
TAIL_LINES=40
LINE_CHARS=1000

# 1. The packet — best-effort, loud.
rc=0
"$WRAP" "$KIND" "$TITLE" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "$me: the packet for $KIND did not open (boss-maintenance-wrap.sh exit $rc) — the check runs anyway; only this run's visibility is at risk" >&2
fi

# 2. The check — captured whole, streamed live.
capture=$(mktemp "${TMPDIR:-/tmp}/boss-chore.XXXXXX")
trap 'rm -f "$capture"' EXIT
set +e
"$@" 2>&1 | tee -- "$capture"
check_rc=${PIPESTATUS[0]}
set -e

# 3. The verdict — best-effort, loud; the excerpt is the check's own words.
total=$(wc -l < "$capture")
if [ "$total" -le $((HEAD_LINES + TAIL_LINES)) ]; then
    excerpt=$(cut -c1-"$LINE_CHARS" -- "$capture")
else
    omitted=$((total - HEAD_LINES - TAIL_LINES))
    excerpt=$(
        head -n "$HEAD_LINES" -- "$capture" | cut -c1-"$LINE_CHARS"
        echo "[... $omitted lines omitted — the pod log has the whole output ...]"
        tail -n "$TAIL_LINES" -- "$capture" | cut -c1-"$LINE_CHARS"
    )
fi
# $(...) strips the trailing newline; put one back so the excerpt reads
# as the lines it is.
[ -z "$excerpt" ] || excerpt="$excerpt"$'\n'

if [ "$check_rc" -eq 0 ]; then result=ok; else result=failed; fi
rc=0
"$STEP" "$KIND" run "result=$result" "exit_status=$check_rc" "output=$excerpt" || rc=$?
if [ "$rc" -ne 0 ]; then
    echo "$me: the packet for $KIND did not record result=$result (boss-step.sh exit $rc) — the check exited $check_rc and that is what this Job reports; only this run's visibility is lost" >&2
fi

# 4. The check's own status.
exit "$check_rc"
