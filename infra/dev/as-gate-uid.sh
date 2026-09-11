#!/usr/bin/env bash
#
# Run a command in a workspace the GATE'S OWN UID created, so a check
# that reads git, or reads file ownership, answers here what it will
# answer on the gate-runner.
#
#     infra/dev/as-gate-uid.sh cargo test -p boss-cli --all-features
#     infra/dev/as-gate-uid.sh infra/gate.sh --quick
#     infra/dev/as-gate-uid.sh --print-ids
#
# WHY THIS SCRIPT EXISTS (backlog cc9ddc5d, measured 2026-09-11). About
# ten builder briefs said "verify under `setpriv --reuid=65534`" and all
# ten were wrong about how: they named a REQUIREMENT and not a METHOD.
# `chown 65534` is not available on this pod — `CapEff` is 0xc0, which
# is CAP_SETUID and CAP_SETGID and no CAP_CHOWN — so chowning a
# checkout fails on /tmp, /scratch and /work alike. Three builders
# resolved that three different ways in one session: one found the route
# below, one substituted a world-writable copy (sound for a check that
# does not read git, wrong for one that does, and it said nothing about
# which it was), and one measured the wrong thing. One instruction,
# three resolutions, because a requirement is not a method.
#
# THE METHOD. A uid with no CAP_CHOWN can still own files it CREATES.
# So the target uid creates the tree itself: this script bundles the
# commit under test (a bundle is an ordinary file, readable by anyone,
# which is the point — a `git clone` of a root-owned repo would run
# upload-pack as the target uid and hit git's dubious-ownership refusal,
# the exact failure infra/gate.sh's safe.directory slot exists for), and
# then the target uid inits, fetches and checks out. Every file in the
# workspace is created by that uid, so it owns all of them and needs no
# capability at all.
#
# `origin/main` RIDES IN THE BUNDLE, because several lints derive a
# branch's own commits from a trunk ref and answer "no trunk ref found"
# without one — a confident wrong diagnosis of a missing fixture. With
# it present, `git merge-base --is-ancestor origin/main HEAD` answers
# here too, which is the base check the same briefs ask for.
#
# WHAT IT DOES NOT CARRY: uncommitted work. A bundle holds commits. That
# is not a limitation to route around — it is the builder order
# (test, fmt, COMMIT AND PUSH, then any local check), so an uncommitted
# tracked change is refused by name rather than silently verified away.
set -uo pipefail

die() { printf 'as-gate-uid: %s\n' "$*" >&2; exit 2; }

repo="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree — run this from the repo you are verifying"
cd "$repo" || die "cannot enter $repo"

MANIFEST="infra/gate-runner/gate-runner.yaml"
ENV_FILE="infra/dev/pod-build.env"
[ -f "$MANIFEST" ] || die "$MANIFEST is missing — the gate's uid has no authority to read"
[ -f "$ENV_FILE" ] || die "$ENV_FILE is missing — the cargo bound has no authority to read"

# THE UID AND GID ARE READ, NOT TYPED. The gate container in the runner
# manifest is the only place that decides them (#310 moved them from
# root once already); a number copied into this script would be the
# §9a defect that this whole car is about, one layer down. Scoped to the
# `gate` container so the postgres sidecar's 999 cannot be picked up,
# and stopped at the next container so a later one cannot either.
ids() {
    awk '
        /^        - name: gate$/ { inside = 1; next }
        inside && /^        - name: / { exit }
        inside && $1 == "runAsUser:"  { uid = $2 }
        inside && $1 == "runAsGroup:" { gid = $2 }
        END { if (uid != "" && gid != "") printf "%s %s\n", uid, gid }
    ' "$MANIFEST"
}
read -r GATE_UID GATE_GID <<<"$(ids)"
[ -n "${GATE_UID:-}" ] && [ -n "${GATE_GID:-}" ] \
    || die "could not read the gate container's runAsUser/runAsGroup from $MANIFEST"

if [ "${1:-}" = "--print-ids" ]; then
    printf '%s %s\n' "$GATE_UID" "$GATE_GID"
    exit 0
fi
[ "$#" -gt 0 ] || die "give a command to run, or --print-ids"

# A tracked change that is not in HEAD would not ride the bundle, and a
# verification of a tree that is not the tree is worse than none.
if ! git diff --quiet HEAD -- 2>/dev/null; then
    git diff --name-only HEAD -- >&2
    die "the files above differ from HEAD and a bundle carries only commits.
             Commit (and push) first — that is the builder order, not a detour."
fi
head_sha="$(git rev-parse HEAD)" || die "cannot read HEAD"
git rev-parse --verify --quiet origin/main >/dev/null \
    || die "origin/main is missing — 'git fetch origin' first, or the lints that
             need a trunk ref will fail here claiming the ref does not exist"

# A scratch root this process owns, made traversable for the target uid.
# `chmod` is the owner's to give; `chown` is what we do not have. Named
# per-pid AND per-uid for the reason boss_testing::scratch is (a leftover
# from an exited process of another account is the failure, and a fixed
# name under a sticky /tmp is how you meet it).
root="${TMPDIR:-/tmp}/as-gate-uid-$(id -u)-$$"
rm -rf "$root" || die "cannot clear $root"
mkdir -p "$root" || die "cannot create $root"
chmod 0777 "$root" || die "cannot make $root traversable for uid $GATE_UID"

bundle="$root/under-test.bundle"
work="$root/repo"
git bundle create -q "$bundle" HEAD origin/main \
    || die "could not bundle HEAD and origin/main"
chmod 0644 "$bundle" || die "cannot make the bundle readable by uid $GATE_UID"

# Already that uid? Then run directly. setpriv to one's own uid happens
# to be allowed, but a script that only works as root is a script that
# reds for everyone downstream of it, so the no-op case is explicit.
if [ "$(id -u)" = "$GATE_UID" ]; then
    as() { "$@"; }
    how="already running as uid $GATE_UID; the workspace was created by this process"
else
    as() { setpriv --reuid="$GATE_UID" --regid="$GATE_GID" --clear-groups "$@"; }
    how="uid $GATE_UID / gid $GATE_GID created the workspace by fetching a bundle into it (no ownership change)"
fi

# THE WORKSPACE IS DELETED BY THE UID THAT MADE IT, and that is not a
# nicety: this pod's root has the same CapEff 0xc0, so it holds no
# CAP_DAC_OVERRIDE either and `rm -rf` as root fails on every file
# uid 65534 created ("Permission denied", thousands of lines of it).
# Measured on the first run of this script — the same capability gap
# the briefs got wrong, pointed the other way.
cleanup() {
    [ -n "${BOSS_KEEP_GATE_UID_WORKSPACE:-}" ] && return 0
    as rm -rf "$work" 2>/dev/null
    rm -rf "$root" 2>/dev/null
    return 0
}
trap cleanup EXIT

as git init -q "$work" || die "uid $GATE_UID could not init $work"
as git -C "$work" fetch -q "$bundle" \
    "HEAD:refs/heads/under-test" \
    "refs/remotes/origin/main:refs/remotes/origin/main" \
    || die "uid $GATE_UID could not fetch the bundle into $work"
as git -C "$work" symbolic-ref HEAD refs/heads/under-test \
    || die "cannot point HEAD at the commit under test"
as git -C "$work" reset -q --hard under-test \
    || die "uid $GATE_UID could not check out $head_sha"

# HOME must be writable: 65534's passwd entry points at /nonexistent,
# and the first thing several checks do is write a git config. Created
# by the target uid for the same reason the workspace is.
as mkdir -p "$work/.as-gate-uid-home" || die "cannot create a writable HOME"

printf 'as-gate-uid: %s\n' "$how"
printf 'as-gate-uid: workspace %s at %s\n' "$work" "$head_sha"
printf 'as-gate-uid: running: %s\n' "$*"

# The cargo bound comes from the one file that holds it. A cold clone
# means a cold target dir unless the caller points CARGO_TARGET_DIR at
# one this uid can write; that is the honest cost of verifying in a tree
# you own, and it is stated rather than worked around.
set -a
# shellcheck disable=SC1090
. "$repo/$ENV_FILE"
set +a

cd "$work" || die "cannot enter $work"
as env HOME="$work/.as-gate-uid-home" "$@"
status=$?
printf 'as-gate-uid: %s exited %s (verified as uid %s, %s)\n' "$1" "$status" "$GATE_UID" "$how"
exit "$status"
