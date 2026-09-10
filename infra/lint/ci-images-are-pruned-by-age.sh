#!/usr/bin/env bash
# ci-images-are-pruned-by-age — the forge's per-train CI images are
# dropped on a CADENCE, not only when the disk hits its floor.
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
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="$here/../forge/prune-ci-images-by-age.lib.sh"
sweep="$here/../forge/disk-floor-sweep.sh"
for f in "$lib" "$sweep"; do
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
# for, $3 = a tag whose `rmi` docker refuses, $4 = 1 to make `info` fail.
# Window 6h, keep the newest 1 — the same shape the sweep runs.
run_pass() {
    ( set -euo pipefail
      export STUB_DIR="$tmp" STUB_ROOT="$1" STUB_RMI_FAIL="${3:-}" STUB_INFO_FAIL="${4:-0}"
      # shellcheck source=infra/forge/prune-ci-images-by-age.lib.sh
      . "$lib"
      prune_ci_images_by_age "$tmp/docker" "$2" "$REPO" 6 1 selftest ) \
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
grep -q 'prune-ci-images-by-age.lib.sh' "$sweep" \
    || fail "$sweep does not source the age-prune library — a second pruning mechanism is a drifting pair (CLAUDE.md 9a)"

# Code lines only: both phrases also appear in the prose above them, and
# a check that reads a comment as the mechanism is no check at all.
code_line() { grep -n "$1" "$sweep" | grep -vE '^[0-9]+:[[:space:]]*#' | head -1 | cut -d: -f1; }
prune_line=$(code_line 'prune_ci_images_by_age "')
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

# The label and the filter are ONE fact (CLAUDE.md 9a). The journal said
# "older than 24h" for an `until=4h` filter for days — a record that
# describes work nobody did.
grep -q 'older than 24h' "$sweep" && fail "$sweep hardcodes an age in a log label; derive it from the variable that drives the filter"
grep -q 'sudo -n docker' "$sweep" || fail "$sweep does not name the system daemon's docker explicitly — detection is how a prune hits the rootless daemon and frees nothing"

if [ "$problems" -gt 0 ]; then
    echo "" >&2
    echo "  $problems problem(s): the per-train CI images are not pruned by age." >&2
    exit 1
fi
echo "ci-images-are-pruned-by-age: ok — the age pass runs every hour before the floor check, refuses the wrong daemon, reports its bytes, and one stuck image does not stop it"
exit 0
