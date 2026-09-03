#!/usr/bin/env bash
#
# one-stamp-per-transaction — a handler mints ONE EventStamp and every
# event in its transaction rides that one instant.
#
# WHY. The outbox contract records a fact and its events in the SAME
# transaction, and the stamp carries the authoritative timestamp. The
# accounts-children fix deliberately reordered event_stamp() minting
# BEFORE the row writes so row and record share one instant — the
# replay-divergence agent's review (d7b8158e) named the residual risk:
# a handler minting TWO stamps inside one transaction breaks the
# one-instant rule and no test notices, because the split only shows
# up as a replay-ordering subtlety long after the commit. Same family
# as no-wallclock: the instant is part of the correctness contract.
# Packet c1475969.
#
# WHAT IT CHECKS, AND WHY THIS SHAPE. The stamp constructors cannot
# see transaction scope (sqlx `begin()` is called directly in
# handlers), so a runtime assertion would mean interposing on every
# begin — real blast radius for a property that is statically
# visible. The convention in every fact-write handler is textual:
# mint once, at the top, before `begin()`. So this checks the narrow
# thing that prevents the regression: NO FUNCTION BODY CONTAINS TWO
# MINT CALLS (`event_stamp(`, `stamp_with_actor(`, `EventStamp::new(`).
#
# NOT COUNTED, and why:
#   - fns NAMED `event_stamp` — the per-crate constructor helpers;
#     their bodies necessarily contain the raw mints they wrap.
#   - `*/tests/*` paths — a test minting several stamps is exercising
#     time, not writing one transaction.
#   - mints on a match-ARM line (the line carries `=>`): the
#     publisher-optional idiom `Some(p) => p.stamp_with_actor(..),
#     None => EventStamp::new(..)` is two textual mints and one
#     dynamic mint. The cost: a real double-mint written entirely in
#     match arms goes unseen. Said out loud rather than left as a gap.
#   - a line carrying `one-stamp: allow` — the waiver for a function
#     that genuinely needs two instants; the comment IS the review
#     trail.
#   - string literals and `//` comments are scrubbed before counting,
#     for braces AND mints — steps.rs's refusal messages carry
#     unpaired braces that otherwise merge adjacent functions into
#     one count (measured: update_step false-positived exactly this
#     way at adoption).
#
# GRANDFATHERED AT ADOPTION (the ratchet floor). Two existing sites
# mint twice in one body and are NOT waived, because nobody has yet
# adjudicated whether their two instants are intended. Both are the
# same shape — a primary stamp, then a second stamp for the cascade
# the write triggers, in the SAME transaction; both comments explain
# the ACTOR choice and neither says the second INSTANT is deliberate:
#   crates/core/boss-jobs/src/http/steps.rs update_step — `stamp`
#   (~650) for the step-update events, `close_stamp` (~887) for the
#   Job-close a terminal step triggers.
#   crates/core/boss-jobs/src/http/jobs.rs create_job — `stamp`
#   (~839) for the Job events, `step_stamp` (~905) for the
#   materialized steps' STEP_CREATED events.
# Adjudication filed on packet c1475969; entries leave as it
# resolves. New sites do not join this list — they get fixed or they
# get the waiver comment WITH its review.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

GRANDFATHERED='crates/core/boss-jobs/src/http/steps.rs: fn update_step
crates/core/boss-jobs/src/http/jobs.rs: fn create_job'

# The program rides a temp file, NOT a heredoc fd: xargs splits large
# file sets into several awk invocations, and a /dev/fd heredoc is
# consumed by the first — every later batch would silently run an
# EMPTY program, and which files land in the lucky first batch is
# ordering luck. Measured: the violation list flapped between runs.
AWK_PROG=$(mktemp)
trap 'rm -f "$AWK_PROG"' EXIT
cat > "$AWK_PROG" <<'AWK'
function scrub(l) {
    if (l ~ /one-stamp: allow/) return ""
    gsub(/"([^"\\]|\\.)*"/, "\"\"", l)   # string literals out
    gsub(/'({|})'/, "''", l)             # brace char-literals out
    sub(/\/\/.*$/, "", l)                # line comments out
    return l
}
function mints_in(l) {
    if (l ~ /=>/) return 0               # match-arm constructor idiom
    return gsub(/event_stamp[[:space:]]*\(|stamp_with_actor[[:space:]]*\(|EventStamp::new[[:space:]]*\(/, "&", l)
}
function flush_fn() {
    if (mints >= 2 && fnname != "event_stamp")
        printf "%s: fn %s mints %d stamps in one body (line %d)\n", FILENAME, fnname, mints, fnline
    state = 0
}
FNR == 1 { state = 0 }   # 0=outside, 1=in signature, 2=in body
{ clean = scrub($0) }
state == 0 {
    if (clean ~ /^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+[A-Za-z0-9_]+/) {
        name = clean
        sub(/^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+/, "", name)
        sub(/[^A-Za-z0-9_].*$/, "", name)
        fnname = name; fnline = FNR; mints = mints_in(clean)
        l = clean
        ob = gsub(/{/, "{", l); cb = gsub(/}/, "}", l)
        if (ob > 0) { state = 2; depth = ob - cb; if (depth <= 0) flush_fn() }
        else state = 1
    }
    next
}
state == 1 {
    mints += mints_in(clean)
    l = clean
    ob = gsub(/{/, "{", l); cb = gsub(/}/, "}", l)
    if (ob > 0) { state = 2; depth = ob - cb; if (depth <= 0) flush_fn() }
    next
}
state == 2 {
    mints += mints_in(clean)
    l = clean
    depth += gsub(/{/, "{", l) - gsub(/}/, "}", l)
    if (depth <= 0) flush_fn()
}
AWK
violations=$(find crates -name '*.rs' -not -path '*/target/*' -not -path '*/tests/*' -print0 \
    | xargs -0 awk -f "$AWK_PROG")

new_violations=$(printf '%s' "$violations" | grep -v -F "$GRANDFATHERED" || true)

if [ -n "$new_violations" ]; then
    echo "$new_violations"
    echo "one-stamp-per-transaction: FAIL — a transaction gets ONE stamp; mint it once before begin() and pass it to every record_*_in_tx. A function that truly needs two instants carries 'one-stamp: allow' on the second mint line, with the review that earned it."
    exit 1
fi

count=$(printf '%s' "$violations" | grep -c . || true)
echo "one-stamp-per-transaction: clean — no NEW function body mints two stamps ($count grandfathered, adjudication on c1475969)"
