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
# output, host, probe, expect} on the car's metadata, leaves `proven`
# ready, and exits 1 so the ops-request's exit_code carries the
# verdict too. Backlog 28ac45ab.
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
        env --chdir="$PROBE_DIR" HOME="$home" USER="$PROBE_USER" PATH="$PATH" \
            BOSS_JOBS_URL="$BASE" BOSS_PROBE_NOTFOUND="$notfound" \
            bash -c "$probe_prelude$probe" \
        > "$workdir/out" 2> "$workdir/errs" < /dev/null
    rc=$?
else
    # Run by hand, by a person who is already the probe user.
    timeout -k 5 "$PROBE_TIMEOUT" env --chdir="$PROBE_DIR" BOSS_JOBS_URL="$BASE" \
        BOSS_PROBE_NOTFOUND="$notfound" bash -c "$probe_prelude$probe" \
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

# 3. Judge — the two rules `boss prove` applies, and no third.
ok=1
[[ "$rc" -eq 0 ]] || ok=0
if [[ "$ok" -eq 1 ]] && ! printed_expectation "$expect" "$workdir/out" "$workdir/errs"; then
    ok=0
fi

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
if [[ "$unrunnable" == true ]]; then
    why="THE PROBE DID NOT RUN on $host: $missing_list not found. A recorded probe runs on the forge host as $PROBE_USER in $PROBE_DIR, with this host's tools — not on the dev pod where it was written, which is where cluster tools like kubectl live. This says nothing about whether the change works; re-probe from a vantage this host has, or record the car as event-bound."
elif [[ "$rc" -ne 0 ]]; then
    why="the probe RAN on $host and exited $rc — the claim it makes is not holding, or the probe is wrong."
else
    why="the probe RAN on $host and exited 0, but neither stream contained '$expect' — the claim it makes is not holding, or the expected string is wrong."
fi
attempt=$(jq -cn --arg at "$at" --argjson exit "$rc" --arg host "$host" \
    --arg probe "$probe" --arg expect "$expect" --arg why "$why" \
    --argjson unrunnable "$unrunnable" --argjson missing_tools "$missing_json" \
    --arg output "$(printf '%s\n%s' "$stdout" "$stderr")" \
    '{at:$at, exit:$exit, output:$output, host:$host, probe:$probe, expect:$expect,
      why:$why, unrunnable:$unrunnable, missing_tools:$missing_tools}')
printf '%s' "$attempt" | jq -c '{proof_attempt: .}' > "$workdir/payload"
if ! curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
        ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
        --data-binary @"$workdir/payload" \
        "$BASE/api/jobs/$car/metadata" > /dev/null 2> "$workdir/err"; then
    fail "probe exited $rc and recording the attempt on ${car:0:8} failed too — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
fi
if [[ "$unrunnable" == true ]]; then
    say "NOT RUN ${car:0:8} — $why"
else
    say "NOT PROVEN ${car:0:8} — $why"
fi
say "proof_attempt recorded, proven stays ready"
printf '  stdout: %s\n  stderr: %s\n' "${stdout:-(empty)}" "${stderr:-(empty)}"
# 3 = could not run here; 1 = ran and did not prove. Two different
# things to do about it, so two exit codes for the ops-request to carry.
[[ "$unrunnable" == true ]] && exit 3
exit 1
