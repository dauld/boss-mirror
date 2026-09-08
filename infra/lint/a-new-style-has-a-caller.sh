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
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

CSS="apps/web/src/styles.css"
[ -f "$CSS" ] || {
    echo "a-new-style-has-a-caller: $CSS not found — skipping"
    exit 0
}

resolve_base() {
    local ref
    for ref in ${BOSS_TRUNK_REF:-} forge/main origin/main main; do
        if git rev-parse --verify --quiet "$ref" >/dev/null 2>&1; then
            echo "$ref"; return 0
        fi
    done
    return 1
}

BASE=$(resolve_base) || {
    echo "a-new-style-has-a-caller: no trunk ref found — skipping"
    exit 0
}
MB=$(git merge-base "$BASE" HEAD 2>/dev/null) || MB="$BASE"

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
WAS=$(git show "$MB:$CSS" 2>/dev/null | selectors)
if [ -z "$WAS" ]; then
    # No baseline to diff against. Skipping is right here and refusing
    # is not: unlike a ratchet over a whole file, "every class is new"
    # would flag hundreds of pre-existing selectors as this change's
    # fault.
    echo "a-new-style-has-a-caller: no baseline for $CSS at $MB — skipping"
    exit 0
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
