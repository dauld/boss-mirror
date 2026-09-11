#!/usr/bin/env bash
#
# a-manifest-the-converge-ignores-is-refused — every entry in
# infra/cluster/manifests/ is something the cluster converge applies.
#
# WHY THIS EXISTS (backlog e37a833d)
# ----------------------------------
# `manifests_with_image` stages the apply directory with
# `cp "$src"/*.yaml "$dst"/`, so the converge applies files ending `.yaml`
# and nothing else. A manifest committed as `.yml` or `.json`, filed in a
# subdirectory, or named with a leading dot is in the tree, reviewed,
# merged — and never reaches the cluster. Before this check nothing said
# so: no lint, no converge warning, no drift alarm, and `kubectl apply`
# carries no `--prune`, so nothing downstream could see the omission
# either. check-manifests-applied.sh walks `$DIR/*.yaml` and so never
# learns the file exists; the orphan lint derives its declared set the
# same way, and would report the object — if one were ever created by
# hand — as an ORPHAN rather than as an un-converged declaration.
#
# That is the failure CLAUDE.md §Diagnosis forbids outright: a change
# that looks delivered and is not. And it is indistinguishable, from the
# outside, from a manifest that has merely not converged YET — which is
# the whole reason it has to be a lint. Only the lint can tell those two
# apart before a human does.
#
# WHY `.yaml` IS THE ONLY LEGAL NAME, AND THE CONVERGE DID NOT WIDEN
# ------------------------------------------------------------------
# SEVEN readers derive "what the tree declares to the cluster" from this
# one directory, each with its own `*.yaml`:
#
#   infra/forge/cluster-deploy-lib.sh          the apply itself
#   infra/cluster/undeclared-objects.sh        the declared set
#   infra/cluster/check-manifests-applied.sh   tree → cluster
#   infra/lint/a-deleted-manifest-leaves-no-object.sh   properties A and B
#   infra/lint/a-workload-declares-the-user-it-runs-as.sh
#   infra/lint/timers-leave-a-packet.sh        (two globs)
#
# Widening the converge to `.yml`/`.json` leaves all seven free to
# disagree about the set — a reader that widened to one extension and not
# the other reports an object as DECLARED that the converge will never
# create, which is the direction that loses an object. Refusing instead
# makes `*.yaml` select the WHOLE directory, so those seven globs are
# equal to each other BY CONSTRUCTION: not seven edits, and not seven
# equality tests either. CLAUDE.md §9a prefers the collapse, and this is
# a collapse achieved by constraining the data rather than the code.
#
# The other half of the argument is that widening adds a code path
# nothing needs. Twenty manifests, one naming convention, zero offenders
# the day this landed: the guard is the whole value either way, and the
# guard is cheaper against a narrow set than a wide one.
#
# ONE DEFINITION, TWO READERS
# ---------------------------
# Which entries the converge would drop is `manifests_the_converge_ignores`
# in infra/forge/cluster-deploy-lib.sh — the apply's OWN function, sourced
# here rather than re-spelled. The converge refuses to stage a directory
# it could only partly apply; this refuses the same directory at the gate,
# which is why the converge's refusal should never fire. A second `case`
# list in this file would be exactly the pair §9a is about.
#
# EXIT
#   0  every entry is a manifest the converge applies (or documentation)
#   1  an entry would never reach the cluster, the manifests directory is
#      missing, or it holds too few manifests for this to mean anything
set -uo pipefail

NAME="a-manifest-the-converge-ignores-is-refused"
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

DIR="infra/cluster/manifests"
LIB="infra/forge/cluster-deploy-lib.sh"

# Non-vacuity floor. Twenty manifests are declared as of 2026-09-11; the
# floor sits well below that so retiring a few does not red the tree,
# while a scrape that has lost its subject still does. A FLOOR, never an
# exact count — an exact total is a second copy of the directory listing
# (§9a) and has to be edited on every legitimate change. Same number the
# orphan lint's static half uses for the same reason.
MIN_MANIFESTS=10

[ -d "$DIR" ] || {
    echo "$NAME: $DIR does not exist." >&2
    echo "  That is 'unknown', not 'clean' — this check has no subject, so a pass" >&2
    echo "  would assert something about a directory it never read." >&2
    exit 1
}
[ -f "$LIB" ] || {
    echo "$NAME: $LIB is missing, so the set of entries the converge applies" >&2
    echo "  cannot be derived from the converge itself. Refusing rather than" >&2
    echo "  growing a second copy of it here." >&2
    exit 1
}

# shellcheck source=/dev/null
. "$LIB"

shopt -s nullglob
applied=("$DIR"/*.yaml)
shopt -u nullglob

ignored=$(manifests_the_converge_ignores "$DIR")

fail=0
if [ -n "$ignored" ]; then
    echo "$NAME: the converge applies $DIR/*.yaml and nothing else, so these never reach the cluster:" >&2
    while IFS= read -r path; do
        [ -n "$path" ] || continue
        case "$path" in
            *.yml)  why="a manifest spelled .yml — the converge's \`cp\` does not match it" ;;
            *.json) why="a manifest spelled .json — valid for \`kubectl apply\`, not for the converge's \`cp\`" ;;
            */.*)   why="a dot-named entry — \`*\` never expands onto a leading dot, so even .yaml is skipped here" ;;
            *)      if [ -d "$path" ]; then
                        why="a directory — neither the \`cp\` nor \`kubectl apply -f <dir>\` recurses"
                    else
                        why="not a .yaml manifest and not documentation"
                    fi ;;
        esac
        echo "    $path — $why" >&2
    done <<< "$ignored"
    echo "  A manifest that cannot converge is indistinguishable from one that has not" >&2
    echo "  converged YET: it is in the tree, it reads as infrastructure, and no observer" >&2
    echo "  downstream can see that the apply skipped it (there is no --prune)." >&2
    echo "  FIX: rename it to .yaml — the one name every reader of $DIR agrees on —" >&2
    echo "  or move it out of the directory if it is not a cluster declaration." >&2
    fail=1
fi

if [ "${#applied[@]}" -lt "$MIN_MANIFESTS" ]; then
    echo "$NAME: only ${#applied[@]} manifest(s) in $DIR, floor is $MIN_MANIFESTS." >&2
    echo "  Either the manifests moved or the scrape broke. A check over an empty" >&2
    echo "  directory passes vacuously, having asserted nothing about the cluster." >&2
    fail=1
fi

[ "$fail" = 0 ] || exit 1
echo "$NAME: OK — $DIR holds ${#applied[@]} manifests the converge applies, and nothing it would ignore"
exit 0
