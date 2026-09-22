# wt-target-dir.sh — the ONE name for a worktree's own cargo target
# dir, and the ONE decision about when a build must leave a shared one.
# Sourced by infra/dev/wt-cargo (which seeds the dir) and by
# infra/gate.sh (which must not check into a dir another checkout
# owns). Not executable; it runs nothing by itself.
#
# WHY (backlog 955c99b6, 2026-09-22). The pod's manifest exports
# CARGO_TARGET_DIR=/scratch/target for the whole container
# (infra/cluster/manifests/boss-dev.yaml). wt-cargo overrides it per
# worktree, so every builder's real build is isolated — but gate.sh
# set only CARGO_INCREMENTAL, so its cargo inherited the SHARED dir.
# Six builders ran concurrently that day: six source trees, one target
# directory, and `infra/gate.sh --lint` — the door CLAUDE.md names
# before pushing — false-redded a clean tree three times for one
# builder, with `no variant named List found for enum boss_expr::Expr`
# against source that compiles cleanly under wt-cargo.
#
# THE MECHANISM, reproduced in a two-package fixture rather than
# inferred: cargo's artifact identity does not include the workspace
# PATH — two packages of the same name and version at different paths
# were given the same `-C metadata` hash and shared one rmeta — and
# freshness is judged by resolving the dep-info's cwd-RELATIVE source
# paths against the current directory. A neighbour's rlib written
# minutes ago is therefore newer than this tree's checked-out sources,
# reads as fresh, and this tree compiles against the neighbour's API.
# The failure is indistinguishable from a real compile error and names
# a real-looking symbol, which is what makes it expensive: a door that
# reds a clean tree teaches builders to distrust it or to re-run until
# green, and either habit destroys the door (CLAUDE.md §Diagnosis, "a
# check nobody reads is a check that is not running").
#
# WHY A LINKED WORKTREE IS THE WHOLE TEST. It is the only checkout
# that shares a target dir with a checkout it does not own. The
# cluster gate runner `git clone`s into a per-run emptyDir and exports
# its own /gate-target/target (infra/gate-runner/run.sh), forge CI
# clones too, and the operator's /work/boss owns /scratch/target — all
# three are MAIN worktrees, all three are left exactly as they are.
# Redirecting a gate would trade a false red for a cold ~74 GB build.

# wt_target_dir <target-root> <repo-root>
#
# The per-worktree dir's name, spelled once. Keyed on the worktree's
# basename, which is what wt-cargo has seeded since 2026-09-12 — so
# gate.sh lands in a dir the builder's own build already warmed rather
# than a third one.
wt_target_dir() {
    printf '%s/target-%s\n' "$1" "$(basename "$2")"
}

# wt_isolate_target_dir [repo-root]
#
# Points CARGO_TARGET_DIR at this worktree's own dir when the ambient
# one belongs to another checkout, and says so on stderr. Silent and
# inert in every other case — an unset variable, a main checkout, a
# dir already inside this tree, a dir that is already the answer.
wt_isolate_target_dir() {
    # Nothing set means cargo uses <workspace>/target, which is
    # per-worktree by construction. Do NOT invent a /scratch here: a
    # machine without one would get a dir nothing seeds.
    [ -n "${CARGO_TARGET_DIR:-}" ] || return 0

    local root gitdir common mine
    root=${1:-}
    if [ -z "$root" ]; then
        root=$(git rev-parse --show-toplevel 2>/dev/null) || return 0
    fi
    [ -n "$root" ] || return 0

    # A LINKED worktree has its own git dir under the common one; a
    # main checkout has them equal. This is the question "does another
    # checkout own the dir I was handed", asked of git rather than of
    # a path pattern that a rename would falsify.
    gitdir=$(git -C "$root" rev-parse --git-dir 2>/dev/null) || return 0
    common=$(git -C "$root" rev-parse --git-common-dir 2>/dev/null) || return 0
    [ "$gitdir" != "$common" ] || return 0

    # Already this tree's own.
    case "$CARGO_TARGET_DIR" in
        "$root" | "$root"/*) return 0 ;;
    esac

    # Beside the shared one, so the isolated dir lands on the same
    # filesystem — /scratch is the XFS with reflink that makes
    # wt-cargo's seed free.
    mine=$(wt_target_dir "$(dirname "$CARGO_TARGET_DIR")" "$root")
    [ "$mine" != "$CARGO_TARGET_DIR" ] || return 0

    # One line, naming both dirs and the packet, because a builder who
    # sees this run use a different target dir than the run before
    # must be able to read why without opening a script.
    printf '%s\n' "gate: this is a linked worktree — building into $mine, \
not the shared $CARGO_TARGET_DIR that other checkouts write (backlog 955c99b6). \
Cargo keys artifacts by package name and version, not by workspace path, and \
judges freshness against cwd-relative sources, so a neighbour's stale rlib \
reads as fresh here and reds a clean tree." >&2
    if [ ! -d "$mine" ]; then
        printf '%s\n' "gate: $mine does not exist yet, so this build starts cold; \
wt-cargo reflink-seeds it from the shared dir in seconds." >&2
    fi
    export CARGO_TARGET_DIR="$mine"
}
