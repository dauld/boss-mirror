#!/usr/bin/env bash
# ci-images-are-pruned-by-age — the forge's per-train CI images are
# dropped on a CADENCE rather than only at the disk floor, and the
# cadence collects by the FACT the system of record holds — the train
# landed — with the age window as its fallback.
#
# WHY. Measured on the forge host's own journal (disk-floor-sweep.service,
# Sep 03 -> Sep 09 2026): 145 hourly runs, 45 of them BELOW the floor, 26
# ending `FLOOR UNMET`, and the system-daemon image prune — the one
# remediation that touches per-train `boss-ci:<sha>` images — ran 41
# times and freed 238GB, a mean of 5.8GB a pass. Every one of those 41
# passes happened below the floor; the other 100 runs logged "nothing to
# do" while the pile grew back. On 2026-09-05 that pile was 81GB of the
# host's 228GB disk. A disk floor refusal happens BEFORE any check runs,
# so the cost lands on clean cars: four of them lost five departures
# (2026-08-22) and a whole day went to holding (2026-09-05).
#
# The machinery was never missing — it was gated behind the floor. So
# this pins the three things that make the routine pass real:
#
#   A. THE LOOP'S BEHAVIOUR, driven through the real library beside a
#      stub docker: sha-tagged images older than the window go, named
#      tags (`rust1.96`, `latest`) never do, the newest N survive
#      whatever their age, and ONE item that cannot be removed or read
#      does not take the rest of the pass down with it (25b54ae8: one
#      `?` on a per-item call aborted the branch sweep's whole loop
#      every pass).
#   B. THE WRONG DAEMON IS A REFUSAL, NOT A SUCCESS. This host runs two
#      docker daemons and the CI images live in the SYSTEM one. A prune
#      aimed at the rootless daemon reports success and frees nothing,
#      which is worse than not running — so the library reads the
#      daemon's root directory and refuses, loudly, when it is not the
#      one it was asked for.
#   C. THE WIRING. The routine pass runs on every hourly pass — BEFORE
#      the floor early-return — and its window is strictly looser than
#      the below-floor emergency window, so the two stay ordered:
#      routine keeps more, the backstop deletes harder.
#   D. THE RECORD BEFORE THE CLOCK, and its safety property (backlog
#      9195a2a6). An age window is a proxy for "this image will not be
#      needed again", and the proxy broke when the train rate rose from
#      5-14 a day to ~60: measured 2026-09-11, all 13 images in the
#      daemon were inside the six-hour window and the pass could legally
#      collect NOTHING. So a tag whose train is DONE is collectable at
#      any age, and the age window covers only what the record cannot
#      vouch for. EVERY failure of that lookup must prune LESS: an
#      unreachable system of record, a reply that does not parse, a reply
#      that lists no trains (an unauthenticated read is handed an empty
#      page, not an error) and an unset URL all yield an empty set, and
#      an empty set is exactly the pass that shipped before.
#      The behaviour of one landed image inside the window is pinned
#      beside the full stub-driven suite in
#      crates/core/boss-testing/tests/an_image_outlives_its_train_not_its_clock.rs,
#      which RUNS disk-floor-sweep.sh; this lint is the build-free half
#      that also runs in `infra/gate.sh --quick`.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="$here/../forge/prune-ci-images.lib.sh"
resolver="$here/../forge/landed-train-shas.lib.sh"
sweep="$here/../forge/disk-floor-sweep.sh"
for f in "$lib" "$resolver" "$sweep"; do
    [ -f "$f" ] || { echo "ci-images-are-pruned-by-age: missing $f" >&2; exit 1; }
done

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
problems=0
fail() { echo "FAIL: $*" >&2; problems=$((problems + 1)); }

# ---------------------------------------------------------------------
# A stub docker, so the loop is exercised without a daemon.
# ---------------------------------------------------------------------
cat >"$tmp/docker" <<'STUB'
#!/usr/bin/env bash
echo "$*" >>"$STUB_DIR/calls"
case "$1" in
    info)
        [ "${STUB_INFO_FAIL:-0}" = 1 ] && exit 1
        printf '%s\n' "$STUB_ROOT"; exit 0 ;;
    images)
        cat "$STUB_DIR/listing"; exit 0 ;;
    image)
        shift
        [ "$1" = inspect ] || exit 1
        id="${!#}"
        line="$(grep "^$id " "$STUB_DIR/meta")" || exit 1
        printf '%s\n' "${line#* }"; exit 0 ;;
    rmi)
        if [ -n "${STUB_RMI_FAIL:-}" ] && [ "${2##*:}" = "$STUB_RMI_FAIL" ]; then
            echo "Error response from daemon: conflict: unable to delete $2 (cannot be forced) - image is being used by running container 9f1c" >&2
            exit 1
        fi
        echo "Untagged: $2"; exit 0 ;;
esac
exit 127
STUB
chmod +x "$tmp/docker"

REPO="10.20.0.15:3000/david/boss-ci"
now=$(date -u +%s)
at() { date -u -d "@$((now - $1 * 3600))" +%Y-%m-%dT%H:%M:%SZ; }   # $1 = hours ago
GB=1073741824

# id                                        tag(= the id, as CI tags are full shas)   age(h)  bytes
#  newest, young        -> kept_newest
#  young                -> kept_young
#  over the window      -> removed   (and the LAST one proves the loop did not abort)
#  named tag            -> never a candidate
#  inspect unreadable   -> skipped, named
#  rmi refuses (in use) -> skipped, named, loop continues
: >"$tmp/listing"; : >"$tmp/meta"
add() { # $1 id/tag  $2 hours-ago  $3 bytes
    printf '%s %s\n' "$1" "$1" >>"$tmp/listing"
    printf '%s %s %s\n' "$1" "$(at "$2")" "$3" >>"$tmp/meta"
}
c0=cccccccccccccccccccccccccccccccccccccccc   # 2h  newest
f0=ffffffffffffffffffffffffffffffffffffffff   # 3h  young
b0=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb   # 10h removed
d0=dddddddddddddddddddddddddddddddddddddddd   # 20h unreadable
e0=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee   # 25h rmi refuses
a0=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa   # 30h removed, and processed LAST
add "$c0" 2  $((3 * GB))
add "$f0" 3  $((3 * GB))
add "$b0" 10 $((4 * GB))
add "$e0" 25 $((5 * GB))
add "$a0" 30 $((2 * GB))
add rust1.96 400 $((3 * GB))
# Listed, but its metadata cannot be read: the pass must name it and move on.
printf '%s %s\n' "$d0" "$d0" >>"$tmp/listing"

# $1 = the root the stub daemon reports, $2 = the root the caller asks
# for, $3 = a tag whose `rmi` docker refuses, $4 = 1 to make `info` fail,
# $5 = the newline-separated landed-train key set (empty = the system of
# record could not be read, which is the pass that shipped before).
# Window 6h, keep the newest 1 — the same shape the sweep runs.
run_pass() {
    ( set -euo pipefail
      export STUB_DIR="$tmp" STUB_ROOT="$1" STUB_RMI_FAIL="${3:-}" STUB_INFO_FAIL="${4:-0}"
      # shellcheck source=infra/forge/prune-ci-images.lib.sh
      . "$lib"
      prune_ci_images "$tmp/docker" "$2" "$REPO" 6 1 selftest "${5:-}" ) \
      >"$tmp/out" 2>&1
    echo $?
}

# ---------------------------------------------------------------------
# A. the loop
# ---------------------------------------------------------------------
: >"$tmp/calls"
rc=$(run_pass /var/lib/docker /var/lib/docker "$e0")
[ "$rc" = 0 ] || fail "a pass that could read the daemon returned $rc"
grep -q "removed=2" "$tmp/out" || fail "expected removed=2 (the 10h and 30h images); got: $(grep -o 'images=.*' "$tmp/out")"
grep -q "kept_newest=1" "$tmp/out" || fail "the newest image was not kept regardless of age"
grep -q "kept_young=1" "$tmp/out" || fail "the 3h image was not kept as young"
grep -q "kept_named=1" "$tmp/out" || fail "the rust1.96 base tag was not counted as a named keep"
grep -q "unreadable=1" "$tmp/out" || fail "the image whose metadata cannot be read was not named"
grep -q "failed=1" "$tmp/out" || fail "the image docker refused to remove was not counted as failed"
# 4GB + 2GB = 6144MiB, reported in numbers rather than "done".
grep -q "reclaimed=6144MiB" "$tmp/out" || fail "the pass does not report the bytes it reclaimed: $(cat "$tmp/out")"
# Per-item isolation, the 25b54ae8 guard: the refused removal is processed
# BEFORE the oldest image, so an aborting loop would leave that one behind.
grep -qx "rmi $REPO:$a0" "$tmp/calls" || fail "one failed removal aborted the loop — the oldest image was never reached (25b54ae8)"
grep -q "$e0" "$tmp/out" || fail "the refused removal is not named in the log"
grep -q "using its referenced image\|being used by running container" "$tmp/out" \
    || fail "docker's reason for refusing is not printed (a failure that cannot be read)"
grep -qx "rmi $REPO:rust1.96" "$tmp/calls" && fail "the rust1.96 base tag was removed — only sha-shaped per-train tags are candidates"
grep -qx "rmi $REPO:$c0" "$tmp/calls" && fail "the newest image was removed"
grep -qx "rmi $REPO:$f0" "$tmp/calls" && fail "an image inside the window was removed"
grep -qx "rmi $REPO:$d0" "$tmp/calls" && fail "an image whose metadata could not be read was removed anyway"

# ---------------------------------------------------------------------
# B. the wrong daemon is a refusal
# ---------------------------------------------------------------------
: >"$tmp/calls"
rc=$(run_pass /home/david/.local/share/docker /var/lib/docker)
[ "$rc" = 0 ] && fail "a prune aimed at the wrong daemon returned 0 — it would report success and free nothing"
grep -q "SKIPPED" "$tmp/out" || fail "the wrong-daemon refusal is not loud"
grep -q "/home/david/.local/share/docker" "$tmp/out" || fail "the refusal does not name the daemon it actually found"
grep -q "rmi" "$tmp/calls" && fail "the wrong daemon was pruned anyway"

: >"$tmp/calls"
rc=$(run_pass /var/lib/docker /var/lib/docker "" 1)
[ "$rc" = 0 ] && fail "a daemon that cannot be read returned 0 — a sudo refusal must not read as a clean pass"
grep -q "SKIPPED" "$tmp/out" || fail "an unreadable daemon is not reported loudly"

# ---------------------------------------------------------------------
# C. the wiring in disk-floor-sweep.sh
# ---------------------------------------------------------------------
grep -q 'prune-ci-images.lib.sh' "$sweep" \
    || fail "$sweep does not source the prune library — a second pruning mechanism is a drifting pair (CLAUDE.md 9a)"
grep -q 'landed-train-shas.lib.sh' "$sweep" \
    || fail "$sweep does not source the landed-train resolver, so the routine pass is back to the clock alone — which at ~60 trains a day can legally collect nothing (9195a2a6)"

# Code lines only: both phrases also appear in the prose above them, and
# a check that reads a comment as the mechanism is no check at all.
code_line() { grep -n "$1" "$sweep" | grep -vE '^[0-9]+:[[:space:]]*#' | head -1 | cut -d: -f1; }
prune_line=$(code_line 'prune_ci_images "')
floor_line=$(code_line 'nothing to do')
if [ -z "$prune_line" ] || [ -z "$floor_line" ]; then
    fail "cannot locate the age pass and the floor early-return in $sweep"
elif [ "$prune_line" -gt "$floor_line" ]; then
    fail "the age pass (line $prune_line) runs AFTER the floor early-return (line $floor_line), so it only fires below the floor — which is the defect (e5dc60e4)"
fi

routine=$(grep -oE '^CI_IMAGE_AGE_HOURS="?\$\{BOSS_CI_IMAGE_AGE_HOURS:-[0-9]+' "$sweep" | grep -oE '[0-9]+$')
floor_age=$(grep -oE '^CI_IMAGE_FLOOR_AGE_HOURS=[0-9]+' "$sweep" | grep -oE '[0-9]+$')
if [ -z "$routine" ] || [ -z "$floor_age" ]; then
    fail "$sweep does not name both windows as CI_IMAGE_AGE_HOURS and CI_IMAGE_FLOOR_AGE_HOURS, so nothing can check that they stay ordered"
elif [ "$routine" -le "$floor_age" ]; then
    fail "the routine window (${routine}h) is not looser than the below-floor window (${floor_age}h): the emergency pass must delete harder than the cadence, or the backstop is just the cadence again"
fi

# An SoR outage must not weaken the floor defence: everything below the
# floor early-return owes the record nothing.
floor_half="$(sed -n '/reclaiming regenerable docker caches/,$p' "$sweep")"
for token in LANDED landed_train_shas BOSS_JOBS_URL; do
    printf '%s' "$floor_half" | grep -q "$token" \
        && fail "the below-floor remediations in $sweep reference \`$token\` — an arm that needs the system of record is not an arm when the record is what is down"
done

# ---------------------------------------------------------------------
# D. the record before the clock, and the failure paths that prune LESS
# ---------------------------------------------------------------------
# The 3h image is INSIDE the window. Handed its train's key, the pass
# collects it; handed nothing, it keeps it — which is the whole defect and
# the whole safety property in one pair of runs.
: >"$tmp/calls"
rc=$(run_pass /var/lib/docker /var/lib/docker "$e0" 0 "$(printf '%s\n' "${f0:0:7}")")
[ "$rc" = 0 ] || fail "a pass handed a landed-train key returned $rc"
grep -qx "rmi $REPO:$f0" "$tmp/calls" \
    || fail "the 3h image whose train has LANDED was not collected — the pass is still bounded by the clock (9195a2a6): $(cat "$tmp/out")"
grep -q "landed=1" "$tmp/out" \
    || fail "the summary does not count what the record collected: $(cat "$tmp/out")"
grep -q "its train is done" "$tmp/out" \
    || fail "the removal does not say WHY it was collectable — a record that says THAT and not WHAT is the defect class that cost a day"

: >"$tmp/calls"
rc=$(run_pass /var/lib/docker /var/lib/docker "$e0" 0 "")
[ "$rc" = 0 ] || fail "a pass with no landed-train keys returned $rc"
grep -qx "rmi $REPO:$f0" "$tmp/calls" \
    && fail "an EMPTY landed set collected the 3h image anyway — a lookup that cannot answer must leave the pass exactly as it was, pruning less and never more"
grep -q "removed=2" "$tmp/out" \
    || fail "an empty landed set changed what the age window alone collects: $(cat "$tmp/out")"
grep -q "NO landed-train shas were available" "$tmp/out" \
    || fail "a pass that could not read the record does not say so — an operator cannot tell it from one that consulted the record"

# The resolver itself: nothing on stdout unless it can answer, and never
# a non-zero return. Visibility is best-effort; destruction is not.
cat >"$tmp/curl" <<'CURLSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STUB_DIR/curl-calls"
cat "$STUB_DIR/reply.json"
exit "${STUB_CURL_EXIT:-0}"
CURLSTUB
chmod +x "$tmp/curl"
resolve() { # $1 = reply body, $2 = curl exit, $3 = jobs url (default set)
    printf '%s' "$1" >"$tmp/reply.json"
    ( set -uo pipefail
      export STUB_DIR="$tmp" STUB_CURL_EXIT="${2:-0}"
      # shellcheck source=infra/forge/landed-train-shas.lib.sh
      . "$resolver"
      landed_train_shas "$tmp/curl" "${3-http://10.20.0.34:7900}" 120 selftest ) \
      >"$tmp/keys" 2>"$tmp/why"
    echo $?
}
LANDED_REPLY='{"total":9,"data":[
  {"status":"closed","steps":[{"metadata":{"train_ref":"train/20260911-1044@abc1234"}},
                              {"metadata":{"merge_ref":"def5678abcd"}}]},
  {"status":"open","steps":[{"metadata":{"train_ref":"train/20260911-1144@def5678"}}]}
]}'
: >"$tmp/curl-calls"
rc=$(resolve "$LANDED_REPLY")
[ "$rc" = 0 ] || fail "the resolver returned $rc on a good read — it must always return 0"
grep -qx abc1234 "$tmp/keys" || fail "a closed train's train_ref sha is not collectable: $(cat "$tmp/keys") / $(cat "$tmp/why")"
grep -qx def5678 "$tmp/keys" && fail "a sha an OPEN train still references was reported as landed — the open reference must veto the closed one"
grep -q 'x-boss-user' "$tmp/curl-calls" || fail "the read is unsigned, and an unauthenticated read is handed an EMPTY page rather than an error — which would retire this mechanism silently"
rc=$(resolve "$LANDED_REPLY" 7)
[ "$rc" = 0 ] || fail "the resolver returned $rc when curl failed — an unreachable record must not fail the sweep that defends the disk"
[ -s "$tmp/keys" ] && fail "the resolver invented keys from a failed read: $(cat "$tmp/keys")"
grep -q 'AGE WINDOW' "$tmp/why" || fail "an unreadable record is silent about the fallback"
rc=$(resolve '<html>502</html>')
[ "$rc" = 0 ] || fail "the resolver returned $rc on a reply that is not JSON"
[ -s "$tmp/keys" ] && fail "the resolver invented keys from a reply that is not JSON: $(cat "$tmp/keys")"
grep -q 'AGE WINDOW' "$tmp/why" || fail "a reply that does not parse is silent about the fallback"
# An EMPTY page is a FAILED read, not the fact that no train exists: an
# out-of-scope or unauthenticated read is handed a smaller world (measured
# total 0 where the signed read sees the row). The keys are empty either
# way, so what this pins is that the pass SAYS it could not read —
# otherwise the mechanism could retire itself and still look healthy.
rc=$(resolve '{"total":0,"data":[]}')
grep -q 'AGE WINDOW' "$tmp/why" \
    || fail "the resolver read an EMPTY page as fact rather than as a failed read: $(cat "$tmp/why")"
grep -q 'listed NO pr-train packets' "$tmp/why" \
    || fail "an empty page does not name itself in the fallback reason, so nobody can tell a denied scope from a quiet pass"
: >"$tmp/curl-calls"
rc=$(resolve "$LANDED_REPLY" 0 "")
[ -s "$tmp/keys" ] && fail "the resolver answered without being told which system of record to ask"
[ -s "$tmp/curl-calls" ] && fail "the resolver read a guessed instance — two jobs APIs exist and the wrong one answers 'no trains' instead of erroring"

# The label and the filter are ONE fact (CLAUDE.md 9a). The journal said
# "older than 24h" for an `until=4h` filter for days — a record that
# describes work nobody did.
grep -q 'older than 24h' "$sweep" && fail "$sweep hardcodes an age in a log label; derive it from the variable that drives the filter"
grep -q 'sudo -n docker' "$sweep" || fail "$sweep does not name the system daemon's docker explicitly — detection is how a prune hits the rootless daemon and frees nothing"

if [ "$problems" -gt 0 ]; then
    echo "" >&2
    echo "  $problems problem(s): the per-train CI images are not collected when their train lands, or a failure path prunes more rather than less." >&2
    exit 1
fi
echo "ci-images-are-pruned-by-age: ok — the routine pass runs every hour before the floor check, collects a tag whose train is done at any age, falls back to the age window whenever the record cannot be read, refuses the wrong daemon, reports its bytes, and one stuck image does not stop it"
exit 0
