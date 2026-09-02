#!/usr/bin/env bash
# Known-vulnerability scan of Cargo.lock via cargo-audit — REPORT-ONLY.
#
# WHY THIS EXISTS. Backlog item ffc2c00c ("No security scanning on the
# forge gate — CodeQL only runs at publish"): the only advisory scan in
# the pipeline ran on the public GitHub mirror, weeks after code
# landed. This puts the dependency-advisory check where the code
# actually gates, so a vulnerable crate is named the day it is added,
# not the month it is published.
#
# REPORT-ONLY, deliberately. This script ALWAYS exits 0. Its whole job
# is to print what cargo-audit found so the report rides every gate
# transcript; it reds nothing. Two reasons:
#   1. First-fortnight posture (triage on ffc2c00c, 2026-09-02): watch
#      the signal before wiring it to the brake. An advisory in a
#      transitive dep we cannot upgrade today should not stop every
#      unrelated car.
#   2. The advisory DB is a moving external input. A gate that reds
#      because the OUTSIDE WORLD changed overnight fails determinism —
#      the same tree must gate the same way twice. Enforcement, when it
#      comes, needs an allowlist file so a known-and-accepted advisory
#      is pinned in-repo; that design is recorded on ffc2c00c.
#
# SOFT-SKIPS, loudly. Unlike its roster neighbours this check needs a
# binary the image may not carry yet and a network fetch of the
# advisory DB. Either being absent prints a named skip and exits 0 —
# a missing scanner must not invent a red, but it must SAY it did not
# scan, so a green line here never silently means "nothing checked".
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

if ! command -v cargo-audit >/dev/null 2>&1; then
    echo "cargo-advisories: cargo-audit not in this image — scan SKIPPED"
    echo "cargo-advisories: (report-only; the image addition is the second half of ffc2c00c)"
    exit 0
fi

out=$(cargo audit 2>&1)
status=$?

if [ "$status" -eq 0 ]; then
    echo "cargo-advisories: no known advisories against Cargo.lock"
    exit 0
fi

echo "$out"
if echo "$out" | grep -qi 'error: couldn.t fetch\|failed to fetch\|network\|Connection refused\|Could not resolve'; then
    echo "cargo-advisories: advisory DB unreachable — scan SKIPPED (report-only)"
    exit 0
fi

count=$(echo "$out" | grep -c '^ID:')
echo "cargo-advisories: REPORT-ONLY — ${count} advisor$( [ "$count" -eq 1 ] && echo y || echo ies) above, gate NOT failed"
echo "cargo-advisories: enforcement posture is recorded on backlog item ffc2c00c"
exit 0
