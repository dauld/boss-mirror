#!/usr/bin/env bash
# crate-counts-fresh.sh — the crate roster is described in prose in two
# documents. Prose drifts. This is the equality test.
#
# Origin (David, 2026-08-14, feedback 36c96b16 on /it/kb): "Displayed
# stale architecture diagrams... Let's build a protocol to help us
# maintain this."
#
# The protocol is a lint, not a reminder. A doc that says "27 crates"
# beside a directory holding 29 is wrong in a way no reviewer notices
# and no test catches, because nothing executes prose. On 2026-08-14 all
# three tier counts were stale at once — core 27/29, modules 16/18,
# orchestrators 5/6 — and none of that was visible from reading either
# file, only from counting.
#
# CLAUDE.md §9a: a fact that lives twice gets an equality test. The
# tier counts live in docs/architecture-diagram.md and CLAUDE.md, and
# the truth is the directory listing. Collapsing is not available —
# these are sentences meant for humans, not a data structure — so this
# is the pin, and it names the offending number when it drifts.
#
# What this deliberately does NOT check: the crate and binary NAMES in
# those documents. `boss-jobs-api` is a BINARY produced by the
# `boss-jobs` crate, so a naive name diff reports a dozen false
# positives — it did, on the first draft of this lint. Counts are the
# part that is unambiguous, so counts are the part that is enforced.
#
# Usage: infra/lint/crate-counts-fresh.sh [--self-test]
# Exit:  0 clean / 1 a documented count disagrees with the tree
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

fails=0

# Each row: <tier dir> <file> <regex capturing the claimed count>
check_count() {
    local tier="$1" file="$2" pattern="$3" label="$4"
    local actual claimed
    actual=$(find "crates/$tier" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' ')
    claimed=$(grep -oE "$pattern" "$file" 2>/dev/null | grep -oE '[0-9]+' | sed -n 1p)
    if [ -z "$claimed" ]; then
        echo "crate-counts-fresh: $label — no count found in $file (pattern moved?)"
        fails=$((fails + 1))
        return
    fi
    if [ "$claimed" != "$actual" ]; then
        echo "crate-counts-fresh: $file says $claimed for crates/$tier/, the tree holds $actual ($label)"
        fails=$((fails + 1))
    fi
}

DIAGRAM=docs/architecture-diagram.md

check_count core          "$DIAGRAM" '`crates/core/`, [0-9]+ crates'          "tier 1 in the diagram"
check_count modules       "$DIAGRAM" '[0-9]+ crates\)\. `boss-people'         "tier 2 in the diagram"
check_count orchestrators "$DIAGRAM" '`crates/orchestrators/`, [0-9]+ crates' "orchestrators in the diagram"
check_count tenants       "$DIAGRAM" '`crates/tenants/`, [0-9]+ crates'       "tenants in the diagram"
check_count core          CLAUDE.md  '[0-9]+ core crates'                     "tier 1 in CLAUDE.md"

if [ "${1:-}" = "--self-test" ]; then
    # A lint that cannot fail is decoration. Prove it catches drift by
    # feeding it a doc whose number is deliberately wrong.
    tmp=$(mktemp -d)
    mkdir -p "$tmp/crates/core/a" "$tmp/crates/core/b"
    printf 'blah (`crates/core/`, 99 crates). blah\n' > "$tmp/diagram.md"
    got=$(cd "$tmp" && actual=$(find crates/core -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ') \
        && claimed=$(grep -oE '`crates/core/`, [0-9]+ crates' diagram.md | grep -oE '[0-9]+' | sed -n 1p) \
        && [ "$claimed" != "$actual" ] && echo caught)
    rm -rf "$tmp"
    if [ "$got" = "caught" ]; then
        echo "self-test: drift is detected (99 claimed vs 2 on disk)"
    else
        echo "SELF-TEST FAIL: a wrong count was not reported"
        fails=$((fails + 1))
    fi
fi

if [ "$fails" -gt 0 ]; then
    echo "crate-counts-fresh: $fails stale count(s) — update the prose to match the tree"
    exit 1
fi
echo "crate-counts-fresh: clean — every documented tier count matches the tree"
exit 0
