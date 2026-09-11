#!/usr/bin/env bash
# journal-read.sh — read a host's journal over the HTTP door, but state
# the door's FRESHNESS first and refuse to read it when it is stale.
# READ-ONLY: one GET for the door's cutoffs, one GET for the entries.
# Nothing on the host is mutated and nothing is written to the SoR.
#
# WHY THIS EXISTS. On 2026-09-10 03:40 UTC `systemd-journal-gatewayd` on
# the forge (10.20.0.15:19531) served a journal whose newest entry was
# 2026-09-09 20:40:53 — seven hours behind — while returning HTTP 200 to
# everything. A unit-filtered query for `disk-floor-sweep.service`, which
# HAD run in that window, came back with zero rows. Zero rows from a
# stale door is indistinguishable from "that unit never ran", so the read
# was about to be reported as "the forge disk sweep is not running": a
# false finding filed against a healthy loop (packet 8bea0c9c). The
# documented reachability check for this door was `GET /machine` -> 200,
# which does not detect this at all.
#
# That is CLAUDE.md §Doors' rule verbatim — "a wrong target answers
# instead of erroring" — and §Diagnosis' class in its purest form: the
# record existed, the channel to read it did not, and the channel did not
# say so. This script is the channel saying so.
#
# WHAT IT ASSERTS. Before any filtered read it computes how far behind
# the door is and compares that against a threshold:
#
#   FRESH  -> prints both timestamps, then the entries. Zero rows is
#             then a REAL "nothing to report", and the script says so in
#             those words, naming the retained window so "rotated out"
#             stays distinguishable from "never logged".
#   STALE  -> REFUSES (exit 4). Prints the newest entry the door holds,
#             the time now, the gap, the threshold, and the sentence that
#             matters: an empty result from this door is not evidence
#             about any unit. Names the independent door to use instead.
#   UNREACHABLE / UNPARSEABLE -> SKIPS LOUDLY (exit 3). Never 0. A door
#             that cannot be reached has not answered the question; the
#             posture infra/lint/the-live-rules-are-the-authored-rules.sh
#             takes for the same reason.
#
# TWO SOURCES, NOT ONE. The age is computed from the OLDER of two
# independent readings: `/machine`'s `cutoff_to_realtime` and the
# `__REALTIME_TIMESTAMP` of an actual unfiltered tail entry. We never
# established which internal view went stale in the incident (see
# WHAT WAS ESTABLISHED below), so letting one field vouch for the other
# would be trusting the thing under suspicion. When the two disagree by
# more than the threshold, that disagreement is itself the finding, and
# it is printed.
#
# THE THRESHOLD: 30 minutes (override with --max-age or
# BOSS_JOURNAL_MAX_AGE_S). Measured 2026-09-10 against the forge's own
# retained journal: across the 7 days it holds (2026-09-03 14:15 ..
# 2026-09-10 16:05 UTC) the longest silence between consecutive cron
# heartbeats — the sparsest unconditional writer on the host — was 10.0
# minutes, which was also the p99.9. The densest systemd heartbeat is
# cluster-watchdog at 5 min; the slowest of the 5-15 min loops is
# estate-observe-host at 15 min. So 30 min is 3x the worst legitimate
# silence measured and 2x the slowest heartbeat, while being ~14x smaller
# than the 7-hour staleness that motivated the check. It is also the
# cadence BOSS already uses for its convergence arm (CLAUDE.md
# §Diagnosis), so an operator has no second number to remember.
#
# WHAT WAS ESTABLISHED about the 2026-09-10 staleness, measured from the
# pod through this same door once it had recovered:
#   - journald was healthy and the on-disk journal was CONTINUOUS through
#     the stale window: 264 cron entries between 09-09 19:00 and 09-10
#     06:00 UTC, and no gap over 10 minutes anywhere in the 7-day window.
#     The record existed; only the reader was behind.
#   - journald logged no rotation and no restart between 09-05 21:11 and
#     the measurement, so the packet's first suspect — a long-lived
#     reader holding a rotated journal file — is NOT supported by
#     journald's own record.
#   - the gateway process has been the SAME process since its one and
#     only `Started systemd-journal-gatewayd.service` at 2026-09-03
#     23:10:06, with no stop, restart or failure record in 7 days, and it
#     recovered WITHOUT a restart (same boot_id, cutoff ~20s behind now).
#     So: a transient fault inside a long-lived reader, mechanism NOT
#     established. Establishing it needs `lsof` or `ls -l /proc/<pid>/fd`
#     on the host, and the pod has no ssh to the forge.
# Deliberately NOT done: bounding the gateway process's lifetime with
# `RuntimeMaxSec=` to force periodic self-healing. That expiry records
# the unit as `failed`, and infra/estate/observe-units.sh reads
# `ActiveState=failed` as unhealthy — an hourly red nobody would read,
# which CLAUDE.md §Diagnosis names as the same defect as no check at all.
# The refusal below removes the harm (no wrong answers); if the door
# wedges again for hours, the repair is the one-line human command the
# refusal prints.
#
# TWO HOSTS, AND THE ADVICE FOLLOWS THE TARGET. `forge` and `boss-gcp`
# are named targets, so each door's address lives in this file once
# instead of in whoever types it. boss-gcp's door was added by backlog
# 68757702: that host is the WireGuard bastion, it carries a second,
# older BOSS stack, and it had NO read path from the pod for either logs
# or unit state — so a failing timer there could only be diagnosed by a
# human on the box, which on 2026-09-10 produced a wrong conclusion
# reasoned from the tree instead. Every line of guidance below is derived
# from the resolved target, because a refusal about one host that tells
# you to repair another is a verdict somebody has to go re-derive
# (CLAUDE.md §Diagnosis).
#
# USAGE
#   journal-read.sh [--host <ip[:port]>|forge|boss-gcp] [--max-age <seconds>]
#                   [--count <n>] [--check] [FIELD=VALUE ...]
#
#   journal-read.sh --check
#   journal-read.sh _SYSTEMD_UNIT=disk-floor-sweep.service --count 50
#   journal-read.sh SYSLOG_IDENTIFIER=forge-converge
#   journal-read.sh --host boss-gcp _SYSTEMD_UNIT=boss-ml-inference-batch.service
#
# EXITS  0 fresh (entries printed; zero rows is then a true answer)
#        2 usage
#        3 door unreachable or unparseable — SKIPPED, question unanswered
#        4 door STALE — refused
set -uo pipefail

DEFAULT_HOST="10.20.0.15:19531"
# boss-gcp over the WireGuard overlay: the hub is 10.99.0.1, and from the
# pod that is the only route to the bastion (the-dev-door-is-lan-only).
BOSS_GCP_HOST="10.99.0.1:19531"
MAX_AGE_S="${BOSS_JOURNAL_MAX_AGE_S:-1800}"
HOST=""
COUNT=100
CHECK_ONLY=0
NOW=""
MACHINE_FILE=""
TAIL_FILE=""
FILTERS=()

usage() {
    sed -n '/^# USAGE/,/^#        4 door/p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --host) HOST="${2:-}"; shift 2 || usage ;;
        --max-age) MAX_AGE_S="${2:-}"; shift 2 || usage ;;
        --count) COUNT="${2:-}"; shift 2 || usage ;;
        --check) CHECK_ONLY=1; shift ;;
        # Test hooks. A lint exercises the three postures with no network
        # by supplying the two readings as files and pinning "now", the
        # same way infra/lint/forge-install-covers-the-ops-runner.sh
        # drives install.sh with INSTALL_ETC and a stub systemctl.
        --machine-file) MACHINE_FILE="${2:-}"; CHECK_ONLY=1; shift 2 || usage ;;
        --tail-file) TAIL_FILE="${2:-}"; CHECK_ONLY=1; shift 2 || usage ;;
        --now) NOW="${2:-}"; shift 2 || usage ;;
        -h|--help) usage ;;
        -*) echo "journal-read: unknown option $1" >&2; usage ;;
        *=*) FILTERS+=("$1"); shift ;;
        *) echo "journal-read: '$1' is not a FIELD=VALUE filter" >&2; usage ;;
    esac
done

# THE NAMED TARGETS. One place per door's address, and the LABEL survives
# resolution so every message below can say which host it is talking
# about. An unnamed target keeps its own spelling as the label — there is
# nothing truer to call it — and gets the ops-runner caveat, because the
# independent path needs a runner installed on that specific host.
[ -n "$HOST" ] || HOST="forge"
HOST_LABEL="$HOST"
case "$HOST" in
    forge)    HOST="$DEFAULT_HOST" ;;
    boss-gcp) HOST="$BOSS_GCP_HOST" ;;
esac
case "$HOST" in *:*) : ;; *) HOST="${HOST}:19531" ;; esac

[[ "$MAX_AGE_S" =~ ^[0-9]+$ ]] || { echo "journal-read: --max-age wants whole seconds, got '$MAX_AGE_S'" >&2; usage; }
{ [[ "$COUNT" =~ ^[0-9]+$ ]] && [ "$COUNT" -gt 0 ]; } || { echo "journal-read: --count wants a positive integer, got '$COUNT'" >&2; usage; }
[ -n "$NOW" ] || NOW="$(date -u +%s)"
[[ "$NOW" =~ ^[0-9]+$ ]] || { echo "journal-read: --now wants epoch seconds, got '$NOW'" >&2; usage; }

BASE="http://${HOST}"
SKIP=3
STALE=4

iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "(unconvertible epoch $1)"; }

# "7h01m", not 25267s alone: the unit an operator thinks in.
human() {
    local s="$1" h m
    h=$(( s / 3600 )); m=$(( (s % 3600) / 60 ))
    if [ "$h" -gt 0 ]; then printf '%dh%02dm' "$h" "$m"; else printf '%dm%02ds' "$m" "$(( s % 60 ))"; fi
}

# The headline word is the first argument, because "I could not open a
# socket" and "something answered 200 but it was not a journal gateway"
# are different facts and the second is the wrong-target mistake
# CLAUDE.md §Doors warns about. Both are the same POSTURE: skip, never
# pass.
# THE INDEPENDENT PATH, NAMED FOR THE TARGET. Printed by both postures
# that leave the question unanswered, on stdout so each caller redirects
# it once. It used to say `host=forge` whatever was asked for, which
# since boss-gcp's door exists would send an operator at the wrong
# machine — and the caveat is there because the independent path needs an
# ops runner installed on THAT host, which is not true everywhere
# (boss-gcp's has never been installed; the-dev-door-is-lan-only).
independent_path() {
    echo "    The independent path, which shells local journalctl on the host and so"
    echo "    cannot be affected by this door at all:"
    echo "      boss-api POST /api/jobs with kind=ops-request, host=${HOST_LABEL},"
    echo "      verb=journal-tail, args='<unit> <lines>'  (infra/ops/verbs.json)"
    if [ "$HOST_LABEL" != "forge" ]; then
        echo "    CAVEAT: only the forge's ops runner is known installed. On ${HOST_LABEL} that"
        echo "    request may sit at ready with nothing behind it — which is not an answer"
        echo "    either. Check for a boss-ops-runner there before you wait on one."
    fi
}

# The repair, on the host that actually owns the door.
repair_command() {
    echo "    To repair the door (a human command on ${HOST_LABEL}, as root; the process has"
    echo "    no periodic restart by design — infra/forge/OPERATIONS.md §The BOSS units):"
    echo "      systemctl restart systemd-journal-gatewayd.service"
}

skip() {
    local what="$1"; shift
    echo "journal-door: ${HOST} ${what} — SKIPPED, the question is NOT answered" >&2
    for line in "$@"; do [ -n "$line" ] && printf '    %s\n' "$line" >&2; done
    echo "    A door that did not answer is not a door that said 'nothing to report'." >&2
    independent_path >&2
    exit "$SKIP"
}

# --- reading 1: the door's own declared cutoffs ------------------------
machine=""
if [ -n "$MACHINE_FILE" ]; then
    machine="$(cat "$MACHINE_FILE" 2>/dev/null)" || skip UNREADABLE "--machine-file $MACHINE_FILE is unreadable"
else
    code=0
    raw="$(curl -s -m 10 -w '\n%{http_code}' "${BASE}/machine" 2>&1)" || code=$?
    status="${raw##*$'\n'}"
    machine="${raw%$'\n'*}"
    [ "$code" -eq 0 ] || skip UNREACHABLE \
        "GET ${BASE}/machine did not complete (curl exit $code)" "${machine}"
    [ "$status" = "200" ] || skip "ANSWERED HTTP ${status}" \
        "GET ${BASE}/machine answered ${status}, not 200 — wrong port, wrong host, or the gateway is down" \
        "${machine:0:200}"
fi

field() { printf '%s' "$machine" | sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([0-9]*\)\".*/\1/p" | head -1; }
cut_to="$(field cutoff_to_realtime)"
cut_from="$(field cutoff_from_realtime)"
[ -n "$cut_to" ] || skip "NOT A JOURNAL GATEWAY" \
    "${BASE}/machine answered 200 but carried no cutoff_to_realtime, so it is" \
    "something other than systemd-journal-gatewayd. It said: ${machine:0:200}"

# --- reading 2: an actual entry, so one field cannot vouch for itself --
tail_ts=""
tail_src="unfiltered tail entry"
ts_of() { sed -n 's/.*"__REALTIME_TIMESTAMP"[[:space:]]*:[[:space:]]*"\([0-9]*\)".*/\1/p' | sort -n | tail -1; }
if [ -n "$TAIL_FILE" ]; then
    tail_ts="$(ts_of < "$TAIL_FILE" 2>/dev/null)"
elif [ -z "$MACHINE_FILE" ]; then
    body="$(curl -s -m 20 -H 'Accept: application/json' -H 'Range: entries=:-1:1' "${BASE}/entries" 2>/dev/null)" || body=""
    tail_ts="$(printf '%s' "$body" | ts_of)"
fi

# The OLDER of the two readings is the one that must clear the bar.
newest_us="$cut_to"
newest_src="/machine cutoff_to_realtime"
if [ -n "$tail_ts" ] && [ "$tail_ts" -lt "$newest_us" ]; then
    newest_us="$tail_ts"
    newest_src="$tail_src"
fi

newest_s=$(( newest_us / 1000000 ))
age_s=$(( NOW - newest_s ))
[ "$age_s" -lt 0 ] && age_s=0
disagree_s=0
if [ -n "$tail_ts" ]; then
    disagree_s=$(( (cut_to - tail_ts) / 1000000 ))
    [ "$disagree_s" -lt 0 ] && disagree_s=$(( -disagree_s ))
fi

# --- the verdict ------------------------------------------------------
if [ "$age_s" -gt "$MAX_AGE_S" ]; then
    {
        echo "journal-door: ${HOST} STALE — REFUSING TO READ"
        echo "    newest entry the door holds: $(iso "$newest_s")   (${newest_src})"
        echo "    now:                         $(iso "$NOW")"
        echo "    behind by:                   ${age_s}s ($(human "$age_s")) — threshold ${MAX_AGE_S}s ($(human "$MAX_AGE_S"))"
        if [ -n "$tail_ts" ]; then
            echo "    the two readings:            /machine cutoff $(iso $(( cut_to / 1000000 ))), tail entry $(iso $(( tail_ts / 1000000 )))"
            if [ "$disagree_s" -gt "$MAX_AGE_S" ]; then
                echo "    those two DISAGREE by $(human "$disagree_s") — the door's own views of itself are inconsistent"
            fi
        else
            echo "    the two readings:            only /machine answered; no tail entry came back"
        fi
        echo "    An empty or short result from this door right now is NOT evidence that a"
        echo "    unit did not run. It is evidence that this reader is $(human "$age_s") behind."
        independent_path
        repair_command
    } >&2
    exit "$STALE"
fi

echo "journal-door: ${HOST} FRESH — newest entry $(iso "$newest_s"), now $(iso "$NOW") ($(human "$age_s") behind, threshold $(human "$MAX_AGE_S"))"
if [ -n "$tail_ts" ]; then
    echo "journal-door: both readings agree within $(human "$disagree_s") (/machine cutoff and an unfiltered tail entry)"
else
    echo "journal-door: only /machine answered the freshness read, so one field vouched for itself"
fi
[ "$CHECK_ONLY" -eq 1 ] && exit 0

# --- the read the caller actually wanted ------------------------------
query=""
for f in ${FILTERS[@]+"${FILTERS[@]}"}; do
    query="${query:+${query}&}${f}"
done
url="${BASE}/entries${query:+?${query}}"
desc="${query:-<unfiltered>}"

entries="$(curl -s -m 30 -H 'Accept: application/json' -H "Range: entries=:-${COUNT}:${COUNT}" "$url" 2>/dev/null)" || entries=""
rows="$(printf '%s' "$entries" | grep -c '__REALTIME_TIMESTAMP')" || rows=0

if [ "${rows:-0}" -eq 0 ]; then
    echo "journal-door: 0 entries for ${desc}"
    echo "    The door was FRESH ($(human "$age_s") behind) at the moment of this read, so"
    echo "    this IS a real 'nothing to report': nothing matching ${desc} is in the"
    echo "    journal's retained window, $(iso $(( ${cut_from:-0} / 1000000 ))) .. $(iso "$newest_s")."
    echo "    Anything older than that window is rotated out, not absent."
    exit 0
fi

echo "journal-door: ${rows} entries for ${desc} (newest ${COUNT} requested)"
printf '%s\n' "$entries"
exit 0
