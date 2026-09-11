#!/usr/bin/env bash
#
# a-new-style-has-a-caller — a class added to styles.css is used by
# something.
#
# WHY. apps/web/src/styles.css declares 886 class selectors and 474 of
# them appear nowhere in apps/, libs/ or infra/step-plugins/ (measured
# 2026-08-29). The packet that first counted them said 439 on
# 2026-08-22, so the dead set grew by ~35 in a week. A sweep alone loses
# to that rate; this is the ratchet that makes the sweep worth doing,
# and it is user-feedback 887321b6's own done-when.
#
# WHAT IT CHECKS, AND WHY THIS SHAPE. Not "how many dead classes are
# there" — answering that for the BASE commit means reconstructing the
# whole source tree at that commit, which is expensive and fragile. It
# checks the narrower thing that actually prevents growth: every class
# selector this change ADDS to styles.css must be referenced somewhere.
#
# It therefore does NOT catch deleting the last caller of an existing
# class, which also grows the dead set. That is rarer, and a check that
# is cheap and exact about one direction beats one that is slow and
# approximate about both. Said out loud rather than left as a gap.
#
# THE TRAP THIS AVOIDS, learned the hard way while measuring: step
# plugin bundles live in infra/step-plugins/*.js, OUTSIDE apps/ and
# libs/. A scan of the two obvious source trees reports four
# step-checklist-* classes as unreferenced when the checklist plugin
# renders them. Any tooling here must read the bundles, or it will
# recommend deleting live CSS with nothing in apps/ to point at the
# cause.
#
# DYNAMIC CONSTRUCTION is why this is scoped to ADDED classes only. A
# class may be built rather than written — `chip-${status}` — so a
# whole-file sweep cannot conclude "unreferenced means dead" without a
# human. For a class you are adding right now, in the same change, the
# author knows; if it is built dynamically, reference the prefix in a
# comment and this passes.

set -uo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$LINT_DIR/lib/trunk-ref.sh"

LINT=a-new-style-has-a-caller
CSS="apps/web/src/styles.css"
[ -f "$CSS" ] || {
    echo "$LINT: $CSS not found — skipping"
    exit 0
}

# Trunk resolution is lib/trunk-ref.sh (§9a: this walk was copied into
# four lints and had the same defect in all four). A trunk ref that is
# genuinely absent stays a SKIP here — with no baseline, "every class is
# new" would blame this branch for hundreds of pre-existing selectors —
# but a git that could not answer is not an absent ref, and skipping on
# it exits 0, which a gate records as a pass on a tree nothing read
# (measured 2026-09-11, backlog 6b2f4a1a).
BASE=$(resolve_trunk_ref "$LINT"); rc=$?
if [ "$rc" -eq "$LINT_CANNOT_ANSWER" ]; then
    exit "$LINT_CANNOT_ANSWER"
elif [ "$rc" -ne 0 ]; then
    echo "$LINT: no trunk ref found (tried $(trunk_candidates)) — skipping"
    exit 0
fi
MB=$(resolve_merge_base "$LINT" "$BASE" HEAD) || exit $?

# Class selectors present now, minus those present at the base. Parsed
# the same way in both directions so a reformat is not read as an add.
selectors() {
    python3 -c '
import re, sys
print("\n".join(sorted(set(
    re.findall(r"\.([a-zA-Z_][\w-]*)\s*(?=[,{:.\s>+~\[])", sys.stdin.read())
)))) '
}

NOW=$(selectors < "$CSS")
# Is there a baseline AT ALL, asked separately from reading it. `git show
# "$MB:$CSS" 2>/dev/null | selectors` answered "no baseline" for three
# different situations — the file is new on this branch, the repo is
# unreadable, python3 is missing — and skipped with exit 0 for all three.
# `ls-tree` answers only the first: present, or absent, or a real failure.
BASE_HAS_CSS=$(git_answer "$LINT" 0 ls-tree --name-only "$MB" -- "$CSS") || exit $?
if [ -z "$BASE_HAS_CSS" ]; then
    # No baseline to diff against. Skipping is right here and refusing
    # is not: unlike a ratchet over a whole file, "every class is new"
    # would flag hundreds of pre-existing selectors as this change's
    # fault.
    echo "$LINT: no baseline for $CSS at $MB — skipping"
    exit 0
fi
# Two statements, not a pipeline: in `git_answer … | selectors` the status
# bash reports is the PARSER's, so the read's refusal would be lost again.
WAS_FILE=$(git_answer "$LINT" 0 show "$MB:$CSS") || exit $?
WAS=$(printf '%s\n' "$WAS_FILE" | selectors)
if [ -z "$WAS" ]; then
    # The baseline EXISTS and parsed to nothing, which is a different
    # fact and not a skip: the selector parse is how both sides are read,
    # so a parse that sees no class in a 1,500-line stylesheet would make
    # every selector look new.
    echo "$LINT: $CSS exists at $MB but parsed to zero class selectors —" >&2
    echo "  the parse broke, so neither a clean result nor an addition can be" >&2
    echo "  claimed. Fix the selector scraper in this script." >&2
    exit 1
fi

ADDED=$(comm -13 <(printf '%s\n' "$WAS") <(printf '%s\n' "$NOW"))
if [ -z "$ADDED" ]; then
    echo "a-new-style-has-a-caller: clean — no class selectors added"
    exit 0
fi

# One pass over every consumer, including the plugin bundles.
BLOB=$(mktemp)
trap 'rm -f "$BLOB"' EXIT
find apps libs infra/step-plugins -type f \
    \( -name '*.svelte' -o -name '*.ts' -o -name '*.tsx' -o -name '*.js' -o -name '*.html' \) \
    ! -path "*/$CSS" -print0 2>/dev/null | xargs -0 cat > "$BLOB" 2>/dev/null

orphans=0
while read -r cls; do
    [ -z "${cls:-}" ] && continue
    grep -qF "$cls" "$BLOB" && continue
    if [ "$orphans" -eq 0 ]; then
        echo "a-new-style-has-a-caller: a class was added to $CSS with no caller." >&2
        echo "  474 of the 886 selectors already there are unreferenced; this is the" >&2
        echo "  ratchet that stops that number growing (user-feedback 887321b6)." >&2
        echo "  If the class is built dynamically, name its prefix in a comment beside" >&2
        echo "  the construction site and this passes." >&2
    fi
    echo "    .$cls" >&2
    orphans=$((orphans + 1))
done <<< "$ADDED"

if [ "$orphans" -gt 0 ]; then
    exit 1
fi

echo "a-new-style-has-a-caller: clean — $(printf '%s\n' "$ADDED" | grep -c .) added selector(s), all referenced"
