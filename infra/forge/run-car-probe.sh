#!/usr/bin/env bash
# run-car-probe.sh <car-id> — run the probe a LANDED car recorded at
# park time, against production, and write the verdict on the car.
#
# WHAT. The machine half of `boss prove`. A car parked with `boss gate
# --park-probe '<cmd>' --park-expect '<string>'` carries the pair as
# `metadata.proof_probe` / `metadata.proof_expect` (boss_jobs::car).
# When its train arrives, the dispatcher rule
# run-car-probes-on-train-arrived files an ops-request for this host
# (verb run-car-probe, one arg: the car's id) and the ops-runner runs
# this script. It re-reads the car from the system of record, refuses
# unless the car has MERGED and recorded a probe and still has its
# `proven` step open, runs the probe, and judges it by the two rules
# `boss prove` applies: exit 0, and the expected string printed (on
# either stream) — a numeric expectation as a whole token, never as a
# substring, so 10 is not proof of 1 (421b3032). On green it completes `proven` with the SAME proof
# record `boss prove` writes — probe, expect, exit, stdout, stderr,
# host, cwd, at, as a JSON string under `proof`, with `verified`
# defaulting to the car's summary — so `boss prove <car> --recheck`
# can re-run it later. Otherwise it stamps `proof_attempt` {at, exit,
# stdout, stderr, host, probe, expect} on the car's metadata, leaves `proven`
# ready, and exits 1 so the ops-request's exit_code carries the
# verdict too. Backlog 28ac45ab.
#
# THE ATTEMPT RECORDS THE TWO STREAMS SEPARATELY — `stdout` and
# `stderr`, the same two keys the proof record carries, never one merged
# `output`. It used to merge them, and the one field then read `""` for
# the case that matters most: a probe that exited nonzero and said
# NOTHING (backlog 4fccc595, car a0ab90a5 — 18 hours unproven). An empty
# merged field cannot tell a reader whether both streams were empty or
# the record dropped them, and which stream a line came from is half of
# reading it. Same reason the `why` below now names ONE thing rather than
# offering a reader two possibilities to go re-derive.
#
# WHERE A PROBE RUNS — AND WHERE IT WAS WRITTEN. Here: the forge host,
# as david, in the converged checkout, with the forge's tools. NOT the
# dev pod, where the builder typed it. Those are different machines:
# the forge sits outside the cluster and holds no kubeconfig, so a
# probe that starts with `kubectl` is correct from the pod and
# impossible here. Backlog f9304366, the auto-proof loop's first live
# run: two cars, two kubectl probes, two exit codes with EMPTY streams
# — evidence that reads exactly like a false claim. The streams were
# empty because the probes swallowed their own diagnostics
# (`kubectl ... 2>&1 | grep -q ...` pipes `command not found` into the
# grep), so no amount of care with stderr here recovers it. The fix is
# a channel the probe cannot redirect: fd 9, opened before the probe's
# text runs, with a `command_not_found_handle` writing every unfound
# command to it. When that channel has anything in it the attempt is
# stamped `unrunnable` with `missing_tools`, and the script exits 3 —
# "this probe cannot run here" told apart from "the claim is false",
# without a reader re-deriving it (CLAUDE.md §Diagnosis).
#
# `boss gate --park-probe` refuses, at gate time, a probe invoking a
# tool listed in infra/forge/host-absent-tools.txt, so the common case
# never reaches this host at all. This is the backstop for the rest.
#
# WHY HERE, WHY AS DAVID. The dispatcher must not run shells (it is
# the queue watcher, and a hung probe would hold a consumer); this
# host holds the production vantage most probes need — the SoR over
# the LAN, the converged checkout at /home/david/boss, the forge
# journal, docker and the registry. It does NOT hold the cluster's:
# no kubectl, no kubeconfig. (This paragraph used to list `kubectl as
# the converge user` among the vantages here, which is where the
# f9304366 mistake was learned from.) The ops-runner runs verbs as
# root, so the probe is dropped to $BOSS_PROBE_USER (default david)
# with `runuser`; it never runs as root.
#
# THE BOUND, STATED HONESTLY. The ops-runner's posture is that it
# never executes a packet-supplied string, and the verb's argument
# obeys that (a uuid, pattern-checked twice). The PROBE is program
# text a builder wrote — the same builder whose branch just merged
# to main and was converged onto the cluster by this host, as this
# same user. What it may run is bounded to: a ship-a-change car that
# has `merged = "true"`, the one command recorded on it, as david,
# in the checkout, under a timeout, output capped and recorded
# verbatim. Nothing on this host is mutated by the script itself.
#
# Env:
#   BOSS_JOBS_URL      (required, no default — the unit pins it)
#   BOSS_PROBE_USER    (default david)  the user the probe runs as
#   BOSS_PROBE_DIR     (default /home/david/boss)  its cwd, recorded
#   BOSS_PROBE_TIMEOUT (default 60)  seconds before the probe is killed
#   BOSS_OPS_ACTOR     (default automation:run-car-probe)
#   BOSS_MACHINE_TOKEN (optional) forwarded as x-boss-machine-token
#   BOSS_PROBE_NOTFOUND (set here) the file fd 9 records unfound commands in
#   BOSS_PROBE_READER_ACTOR (default automation:run-car-probe-reader)
#                      the id the probe's READ-ONLY actor signs as
#
# What the probe's own env gets, and nothing else: BOSS_JOBS_URL,
# BOSS_PROBE_NOTFOUND, BOSS_SOR_USER (a read-scoped actor — never this
# script's platform-admin one), BOSS_SOR_PORTS (the door's `name=port`
# table, read from the checkout — see WHICH PORT below), and a PATH
# led by infra/forge/probe-bin, which holds `boss-sor-read`. See the
# READ-ONLY READER block below (backlog 61085a9e).
set -uo pipefail

me="run-car-probe"
say() { echo "$me: $*"; }
refuse() { echo "$me: REFUSED — $*" >&2; exit 2; }
fail() { echo "$me: FAILED — $*" >&2; exit 1; }

[[ $# -eq 1 ]] || refuse "usage: run-car-probe.sh <car-id>"
car="$1"
[[ "$car" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
    || refuse "the car must be a full uuid, got '$car'"
[[ -n "${BOSS_JOBS_URL:-}" ]] \
    || refuse "BOSS_JOBS_URL is not set, and there is no safe default (the ops-runner's unit pins it)"
BASE="${BOSS_JOBS_URL%/}"
PROBE_USER="${BOSS_PROBE_USER:-david}"
PROBE_DIR="${BOSS_PROBE_DIR:-/home/david/boss}"
PROBE_TIMEOUT="${BOSS_PROBE_TIMEOUT:-60}"
# Streams are recorded verbatim up to this much — the same cap as
# `boss prove` (prove.rs MAX_STREAM), pinned by a test there.
MAX_STREAM=4000
ACTOR="${BOSS_OPS_ACTOR:-automation:run-car-probe}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

# HOW THE PROBE READS THE SYSTEM OF RECORD — as a NAMED, READ-ONLY
# reader. Backlog 61085a9e.
#
# The header above is this script's, for its own three calls (read the
# car, write the verdict, patch the attempt). It was never given to the
# probe, and the probe's env carried only BOSS_JOBS_URL — so a probe
# doing `curl $BOSS_JOBS_URL/api/...` read as `operator:unidentified`
# and policy answered a NARROWER WORLD in silence. Measured 2026-09-10,
# one backend, one commit: ?kind=ship-a-change&status=open was 21 rows
# for the operator and 0 unidentified; /api/yard/status came back with
# trains, dock, held, recent and dock_depth ALL ZERO — a confident,
# well-formed, completely idle yard. /api/workflows was byte-identical
# between the two readers, so there is nothing in an answer to say
# which kind you hit.
#
# That is worse than a broken read. A PRESENCE assertion fails for the
# wrong reason and someone investigates. An ABSENCE assertion PASSES
# FALSELY — "no open job of kind X remains" is green against an empty
# page the probe was never allowed to see — and the script above then
# records it as a proof, on a car that closes.
#
# NOT this script's own actor, one line though it would be: that is
# platform-admin with WRITE capability, handed to program text a
# builder authored upstream and run here as $PROBE_USER. The privilege
# matches the job instead — `audit-readonly`, which core policy
# (boss-policy-client::defaults) grants Read at Scope::All on every
# shipped resource and NO other action anywhere. Verified by effect on
# the live deployment: with this actor, PATCH /api/jobs/{id}/metadata
# and a step PUT both answered 403 "no active rule for role
# audit-readonly on job:update" / "…on step:update", while every list
# read matched the operator's exactly. boss-testing's
# run_car_probe_sh.rs pins the role below against those default rules,
# because the fact lives in two places and cannot be collapsed
# (CLAUDE.md §9a).
#
# The markers are the extraction point for that test.
# PROBE-READER-BEGIN
READER_ACTOR="${BOSS_PROBE_READER_ACTOR:-automation:run-car-probe-reader}"
READER_USER="{\"id\":\"$READER_ACTOR\",\"role\":\"audit-readonly\",\"access_tier\":\"auditor\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"
# PROBE-READER-END

# And the reader itself, first on the probe's PATH: GET-only, base and
# identity supplied from here rather than by the probe's text, because
# the cheap thing to type has to be the right thing. It is IN THE TREE,
# so the converged checkout carries it with no install step; a checkout
# too old to have it makes the probe fail as
# `boss-sor-read: command not found`, which fd 9 turns into an
# `unrunnable` verdict naming the tool instead of a false claim.
# `boss gate --park-probe` refuses a probe that reads the system of
# record any other way.
PROBE_BIN="$PROBE_DIR/infra/forge/probe-bin"

# WHICH PORT THE READER USES — design 28d2bed9 (David, 2026-09-17). The
# LAN machine door carries every read surface of the instance on one
# IP, one port per service (boss-jobs-internal.yaml), and the reader
# routes a path to its port from this table: `name=port` entries,
# handed over as BOSS_SOR_PORTS. It is read from the SAME checkout the
# reader comes from, as data (never sourced — this script is root), so
# a converge that brings the reader brings its table, and no unit
# carries a second copy to reinstall. A checkout without the file hands
# the reader an empty table, which is the reader it was: jobs only.
# The file is pinned to boss-ports by
# the_machine_door_carries_every_read_surface.rs; the markers are that
# test's extraction point for this read.
# SOR-PORTS-BEGIN
SOR_PORTS=""
if [[ -r "$PROBE_DIR/infra/forge/sor-ports.env" ]]; then
    SOR_PORTS=$(grep -Ev '^[[:space:]]*(#|$)' "$PROBE_DIR/infra/forge/sor-ports.env" \
        | tr -s '[:space:]' ' ' | sed 's/^ //; s/ $//')
fi
# SOR-PORTS-END

for t in curl jq timeout; do
    command -v "$t" >/dev/null 2>&1 || refuse "$t is not installed on this host"
done

workdir=$(mktemp -d) || exit 1
# The not-found channel. It lives OUTSIDE $workdir (0700 root) because
# the probe runs as another user and must be able to append to it; it
# holds command names and nothing else.
notfound=$(mktemp) || exit 1
chmod 666 "$notfound" 2>/dev/null || true
trap 'rm -rf "$workdir" "$notfound"' EXIT

# The prelude every probe's shell gets, ahead of the probe's own text.
# fd 9 is opened here, before the probe can redirect anything, so a
# probe that pipes its own stderr into a grep still cannot hide which
# command was missing. The handler ALSO prints bash's usual message, so
# nothing is taken away. If fd 9 cannot be opened the probe still runs.
#
# The markers are the extraction points: boss-testing's
# run_car_probe_sh.rs lifts what lies between them and RUNS it, so the
# mechanism cannot rot into a comment.
probe_prelude='
# PROBE-PRELUDE-BEGIN
{ exec 9>>"${BOSS_PROBE_NOTFOUND:-/dev/null}"; } 2>/dev/null || exec 9>/dev/null
command_not_found_handle() {
    printf "%s: command not found\n" "$1" >&2
    printf "%s\n" "$1" >&9
    return 127
}
# PROBE-PRELUDE-END
'

# 1. The car, re-read from the system of record — never trusted from
#    the packet that asked.
if ! curl -fsS -H "x-boss-user: $BOSS_USER" "$BASE/api/jobs/$car" \
        > "$workdir/car" 2> "$workdir/err"; then
    fail "GET /api/jobs/$car — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
fi
kind=$(jq -r '.kind // ""' "$workdir/car")
[[ "$kind" == "ship-a-change" ]] || refuse "${car:0:8} is a $kind, not a ship-a-change car"
merged=$(jq -r '.metadata.merged // ""' "$workdir/car")
[[ "$merged" == "true" ]] \
    || refuse "${car:0:8} has not merged (metadata.merged='$merged') — a probe against production proves nothing about unshipped code"
probe=$(jq -r '.metadata.proof_probe // ""' "$workdir/car")
if [[ -z "$probe" ]]; then
    event=$(jq -r '.metadata.proof_event // ""' "$workdir/car")
    [[ -n "$event" ]] && refuse "${car:0:8} is EVENT-BOUND, not probed: $event — it stays hand-proven"
    refuse "${car:0:8} recorded no proof_probe — it stays hand-proven (boss prove <car> --probe ...)"
fi
expect=$(jq -r '.metadata.proof_expect // ""' "$workdir/car")
[[ -n "$expect" ]] \
    || refuse "${car:0:8} recorded a proof_probe but no proof_expect — a probe asserting nothing is 'echo hi'"
step=$(jq -c '
    ((.steps // []) | map(select(.spec_slug == "proven")) | .[0])
    // ((.steps // []) | map(select(.title == "Proven in prod")) | .[0])
    // empty' "$workdir/car")
[[ -n "$step" ]] || refuse "${car:0:8} has no proven step"
status=$(printf '%s' "$step" | jq -r '.status // ""')
case "$status" in
    ready|active) ;;
    completed) say "${car:0:8} is already proven — nothing to run"; exit 0 ;;
    *) refuse "${car:0:8}'s proven step is '$status' (pending = not merged; skipped = abandoned)" ;;
esac
step_id=$(printf '%s' "$step" | jq -r '.id')

# 2. Run — as the probe user, in the checkout, under a timeout. The
#    probe is program text by design (see the header); it reaches the
#    shell as ONE argument, and the shell it reaches is not root's.
say "${car:0:8}  \$ $probe"
if [[ "$(id -u)" -eq 0 ]]; then
    id -u "$PROBE_USER" >/dev/null 2>&1 || refuse "probe user '$PROBE_USER' does not exist on this host; refusing to run a car's probe as root"
    home=$(getent passwd "$PROBE_USER" | cut -d: -f6)
    timeout -k 5 "$PROBE_TIMEOUT" runuser -u "$PROBE_USER" -- \
        env --chdir="$PROBE_DIR" HOME="$home" USER="$PROBE_USER" \
            PATH="$PROBE_BIN:$PATH" \
            BOSS_JOBS_URL="$BASE" BOSS_PROBE_NOTFOUND="$notfound" \
            BOSS_SOR_USER="$READER_USER" BOSS_SOR_PORTS="$SOR_PORTS" \
            bash -c "$probe_prelude$probe" \
        > "$workdir/out" 2> "$workdir/errs" < /dev/null
    rc=$?
else
    # Run by hand, by a person who is already the probe user.
    timeout -k 5 "$PROBE_TIMEOUT" env --chdir="$PROBE_DIR" BOSS_JOBS_URL="$BASE" \
        PATH="$PROBE_BIN:$PATH" \
        BOSS_PROBE_NOTFOUND="$notfound" BOSS_SOR_USER="$READER_USER" \
        BOSS_SOR_PORTS="$SOR_PORTS" \
        bash -c "$probe_prelude$probe" \
        > "$workdir/out" 2> "$workdir/errs" < /dev/null
    rc=$?
fi
[[ "$rc" -eq 124 ]] && printf '\n[run-car-probe: killed at %ss timeout]\n' "$PROBE_TIMEOUT" >> "$workdir/errs"

clip() {
    local size
    size=$(wc -c < "$1")
    head -c "$MAX_STREAM" "$1"
    if [[ "$size" -gt "$MAX_STREAM" ]]; then
        printf '\n… [%s more bytes]' "$((size - MAX_STREAM))"
    fi
}
stdout=$(clip "$workdir/out")
stderr=$(clip "$workdir/errs")
at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
host=$(hostname 2>/dev/null || echo unknown)

# --- expectation-match: mirrors prove.rs `observed` (421b3032) ---
# printed_expectation <expect> <file>... — did any of the files print it?
# A token expectation is a substring test, which is what every good
# probe relies on. A NUMERIC one must appear as a WHOLE token: a
# substring test accepted 10 as proof of 1, so the proof checked
# nothing. The Rust side and this one are pinned equal by a test.
printed_expectation() {
    local want="$1" f re
    shift
    if [[ "$want" =~ ^[+-]?([0-9]+\.?[0-9]*|\.[0-9]+)([eE][+-]?[0-9]+)?$ ]]; then
        # Only digits . + - e can be here, so escaping . and + is enough.
        re=$(printf '%s' "$want" | sed 's/[.+]/\\&/g')
        for f in "$@"; do
            grep -qE -- "(^|[^[:alnum:]._])${re}([^[:alnum:]._]|\$)" "$f" && return 0
        done
        return 1
    fi
    for f in "$@"; do
        grep -qF -- "$want" "$f" && return 0
    done
    return 1
}
# --- end expectation-match ---

# WHAT THE PROBE COULD NOT FIND. fd 9 collected every command bash
# could not resolve, on a channel the probe's own redirections cannot
# reach. Anything here means the probe did not RUN — a different fact
# from a probe that ran and disagreed, and the one the empty-stream
# attempts of 2026-09-09 could not tell anybody.
missing_json=$(sort -u "$notfound" 2>/dev/null \
    | jq -Rsc 'split("\n") | map(select(length > 0))' 2>/dev/null)
[[ -n "$missing_json" ]] || missing_json='[]'
missing_list=$(printf '%s' "$missing_json" | jq -r 'join(", ")' 2>/dev/null)
if [[ "$missing_json" == "[]" ]]; then
    unrunnable=false
else
    unrunnable=true
fi

# 3. Judge — the two rules `boss prove` applies, and no third. The
#    markers are an extraction point: prove.rs's
#    the_forge_runner_gives_the_same_verdict_for_every_outcome lifts
#    these lines with the matcher above and the verdict block below and
#    runs one record through both authors (a44e16aa).
# PROBE-JUDGE-BEGIN
ok=1
[[ "$rc" -eq 0 ]] || ok=0
if [[ "$ok" -eq 1 ]] && ! printed_expectation "$expect" "$workdir/out" "$workdir/errs"; then
    ok=0
fi
# PROBE-JUDGE-END

# The proof record, in `boss prove`'s shape (prove.rs proof_json) —
# field names are a contract `--recheck` reads.
proof=$(jq -cn --arg probe "$probe" --arg expect "$expect" --argjson exit "$rc" \
    --arg stdout "$stdout" --arg stderr "$stderr" --arg host "$host" \
    --arg cwd "$PROBE_DIR" --arg at "$at" \
    '{probe:$probe, expect:$expect, exit:$exit, stdout:$stdout, stderr:$stderr, host:$host, cwd:$cwd, at:$at}')

if [[ "$ok" -eq 1 ]]; then
    # 4a. Proven. `verified` is the car's own claim (the summary), the
    #     same default `boss prove --from-car` uses; `method` is left
    #     unstamped rather than guessed. Merge, never replace: PUT
    #     swaps step metadata wholesale.
    verified=$(jq -r '.metadata.summary // ""' "$workdir/car")
    [[ -n "$verified" ]] || verified="proven by the probe the car recorded at park time"
    printf '%s' "$step" | jq -c --arg v "$verified" --arg proof "$proof" --arg at "$at" '
        {status: "completed",
         metadata: ((.metadata // {})
                    + {verified:$v, proof:$proof, completed_at:$at, proven_by:"run-car-probe"})}' \
        > "$workdir/payload"
    if ! curl -fsS -X PUT -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            --data-binary @"$workdir/payload" \
            "$BASE/api/jobs/$car/steps/$step_id" > /dev/null 2> "$workdir/err"; then
        fail "the probe passed but recording it on ${car:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); re-file the ops-request or run boss prove <car> --from-car"
    fi
    say "PROVEN ${car:0:8} — exit 0 and printed '$expect'"
    [[ -n "$stdout" ]] && printf '%s\n' "$stdout"
    exit 0
fi

# 4b. Not proven. The attempt is evidence too — recorded on the car,
#     `proven` left ready for a person or a re-run, exit 1 so the
#     ops-request carries the verdict in its exit_code.
# The verdict names what failed, in one sentence a reader does not have
# to re-derive (CLAUDE.md §Diagnosis).
# THE VERDICT — one sentence a reader does not have to re-derive
# (CLAUDE.md §Diagnosis, and backlog 4fccc595 for what the old wording
# cost). The markers are the extraction point: boss-testing's
# run_car_probe_sh.rs lifts what lies between them and RUNS it over the
# four outcomes, so the verdict cannot rot into a comment; and prove.rs's
# the_forge_runner_gives_the_same_verdict_for_every_outcome runs the
# same block beside `boss prove`'s judge_probe on one record per branch,
# so the load-bearing phrases below — the ALL-CAPS word and the reason
# clause — cannot drift from the Rust that has the unit tests (a44e16aa:
# "printing NOTHING" here vs "printed NOTHING" there, "neither stream
# contained" vs "never printed", found the day the pin was written).
#
# WHAT THE PROBE SAID, in one line: the first non-empty line of stderr,
# else of stdout. stderr first because that is where a tool puts its
# diagnosis — jq's `error(…)`, curl's message, bash's command-not-found.
# PROBE-VERDICT-BEGIN
said=$(sed -n '/[^[:space:]]/{s/^[[:space:]]*//;p;q;}' "$workdir/errs" "$workdir/out" 2>/dev/null | cut -c1-300)
# WHAT BASH SAYS WHEN `[` OR `((` IS HANDED A NON-NUMBER (68081368).
# The first stderr line carrying one of these is a CRASH, whatever
# exit code the `||` branch behind it chose. The same three phrases
# are prove.rs's NUMERIC_CRASH_MARKERS; its test
# the_forge_runner_names_the_same_crashes runs this block over each
# of them, so adding one here and not there (or the reverse) fails by
# name (CLAUDE.md §9a).
crash=$(grep -m1 -E 'integer expression expected|unary operator expected|syntax error: invalid arithmetic operator' "$workdir/errs" 2>/dev/null | sed 's/^[[:space:]]*//' | cut -c1-300)
# THE FLAG IS THE SENTENCE'S (5461b899). not_yet is what orient, the
# yard and recheck-failing-probes-daily read; $why is what the operator
# reads; they ride one record. Until 2026-09-14 the flag was computed
# AFTER this block from rc alone — 75, runnable, no crash — so an exit
# 75 with both streams empty recorded THE FAILURE CANNOT BE READ beside
# not_yet=true, the yard said "not yet" over a missing record, and the
# recheck re-ran it forever. `boss prove` derives the flag from the
# sentence and cannot disagree with itself; so, now, does this. True in
# the one branch that writes the NOT YET sentence, and nowhere else.
not_yet=false
if [[ "$unrunnable" == true ]]; then
    why="THE PROBE DID NOT RUN on $host: $missing_list not found. A recorded probe runs on the forge host as $PROBE_USER in $PROBE_DIR, with this host's tools — not on the dev pod where it was written, which is where cluster tools like kubectl live. This says nothing about whether the change works; re-probe from a vantage this host has, or record the car as event-bound."
elif [[ -n "$crash" ]]; then
    # A CRASHED NUMERIC TEST IS NOT NOT-YET. Cars dd1d872d and 2e4d3bce
    # (2026-09-14) recorded the not-yet sentence below while stderr held
    # `bash: line 10: [: null: integer expression expected`: jq printed
    # null, `[ "$n" -ge 1 ]` exited 2, and the `||` branch meant for "no
    # such packet yet" exited 75. Nothing was judged, and the daily
    # recheck would re-run the crash forever. Checked before the exit
    # code is read, because the code is the branch's, not the crash's.
    why="THE PROBE CRASHED comparing a non-number (jq printed null?) on $host, so exit $rc is not a verdict on the claim — it is whichever '||' branch caught the crash. bash's [ said: $crash. Guard the value before the numeric test — '// empty' in the jq filter, or a case \"\$n\" in ''|*[!0-9]*) echo \"not yet: no number\"; exit 75;; esac check — so a missing value says not-yet BY NAME instead of crashing into the not-yet branch. Fix the probe and re-park; a recheck re-runs a crash forever (68081368)."
elif [[ "$rc" -ne 0 && -z "$said" ]]; then
    # A NONZERO EXIT WITH BOTH STREAMS EMPTY IS NOT A VERDICT — it is a
    # missing record, and saying so is the whole of 4fccc595. Car
    # a0ab90a5 sat 18 hours on exactly this: `jq -e` exited 4 because
    # the claim HELD (its success branch emitted `empty`, which is no
    # output), a trailing `|| exit 1` rewrote the 4 to a 1, and the old
    # wording here — "not holding, or the probe is wrong" — read
    # identically to a real regression.
    why="THE FAILURE CANNOT BE READ: the probe ran on $host, exited $rc and printed NOTHING on either stream, so this is not a verdict on the claim — it is a missing record. Nothing here says whether the change is in production. The usual causes are a bare '|| exit <n>', which replaces the status that named the cause and prints nothing, and a swallowed stderr ('2>&1 | grep -q'). Re-park with a probe that echoes what failed, with \$?, before it exits — and check for the shape 4fccc595 measured: under 'jq -e' a success branch of 'empty' exits 4, so the probe fails PRECISELY when the claim holds."
elif [[ "$rc" -eq 75 ]]; then
    # NOT YET (75 = EX_TEMPFAIL): the probe ran, found the world not
    # ready to judge the claim, and said so. Four of the eight probes
    # recorded on 2026-09-12 were exactly this — "no disk-report request
    # carrying for_sweep yet, the sweeps fire daily" — and exit 1 made
    # them read as regressions in the shed and in orient. This is not a
    # verdict against the change; the daily recheck runs it again.
    not_yet=true
    why="NOT YET: the probe ran on $host and said the claim cannot be judged until something happens — $said. Not a verdict against the change; recheck-failing-probes-daily runs it again."
elif [[ "$rc" -ne 0 ]]; then
    why="the probe RAN on $host and exited $rc, so it is not proof of anything. What it said: $said"
elif [[ -z "$said" ]]; then
    why="the probe RAN on $host and exited 0 but never printed '$expect' — it printed NOTHING on either stream, so it did not observe what was claimed. An exit code alone is a weak assertion — 'echo hi' exits 0 too. Re-park with a probe that prints a named token on success."
else
    why="the probe RAN on $host and exited 0 but never printed '$expect', so it did not observe what was claimed. What it printed instead: $said. An exit code alone is a weak assertion — 'echo hi' exits 0 too. Either the change is not in prod, or the probe is looking in the wrong place."
fi
# PROBE-VERDICT-END
attempt=$(jq -cn --arg at "$at" --argjson exit "$rc" --arg host "$host" \
    --arg probe "$probe" --arg expect "$expect" --arg why "$why" \
    --argjson unrunnable "$unrunnable" --argjson missing_tools "$missing_json" \
    --argjson not_yet "$not_yet" \
    --arg stdout "$stdout" --arg stderr "$stderr" \
    '{at:$at, exit:$exit, stdout:$stdout, stderr:$stderr, host:$host, probe:$probe,
      expect:$expect, why:$why, unrunnable:$unrunnable, not_yet:$not_yet, missing_tools:$missing_tools}')
printf '%s' "$attempt" | jq -c '{proof_attempt: .}' > "$workdir/payload"
if ! curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
        ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
        --data-binary @"$workdir/payload" \
        "$BASE/api/jobs/$car/metadata" > /dev/null 2> "$workdir/err"; then
    fail "probe exited $rc and recording the attempt on ${car:0:8} failed too — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
fi
if [[ "$unrunnable" == true ]]; then
    say "NOT RUN ${car:0:8} — $why"
elif [[ "$not_yet" == true ]]; then
    say "NOT YET ${car:0:8} — $why"
else
    say "NOT PROVEN ${car:0:8} — $why"
fi
say "proof_attempt recorded, proven stays ready"
printf '  stdout: %s\n  stderr: %s\n' "${stdout:-(empty)}" "${stderr:-(empty)}"
# 3 = could not run here; 75 = ran and said not yet; 1 = ran and did
# not prove. Three different things to do about it, so three exit codes
# for the ops-request to carry.
[[ "$unrunnable" == true ]] && exit 3
[[ "$not_yet" == true ]] && exit 75
exit 1
