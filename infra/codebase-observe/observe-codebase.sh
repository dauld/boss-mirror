#!/bin/sh
# observe-codebase — the codebase as an estate scope (0a5df32d).
#
# David, 2026-08-31: "We need a Code Base page in IT ... stats about
# size, breakdown by service, to help us anticipate where incidental
# complexity might be building up." Anticipating needs a TREND, and a
# trend needs a series — so this is an observer in the estate idiom:
# measure, POST, never interpret. Each crate (and each app) is a node
# of the codebase; the series accumulates through the same door the
# hardware estate uses, and the eventual page reads it exactly as the
# estate page reads its own. The comparison handler records this scope
# as unknown_scope — honest: nothing DECLARES what size a crate should
# be, so there is nothing to compare against, only to watch.
#
# Runs on boss-gcp against the conductor's clone (fetched first, so the
# reading is of current main, not of whenever the conductor last
# pulled). sh + awk + jq, no python (26d61c97). Line counts are
# `git ls-files | wc -l` per tracked file — crude, stable, and the
# same instrument every night, which is what a trend requires; a
# smarter counter that changes is a broken series.
#
# Env: JOBS_API (required), CLONE_DIR (default /var/lib/boss-train/repo).
set -eu

: "${JOBS_API:?JOBS_API is required}"
CLONE_DIR="${CLONE_DIR:-/var/lib/boss-train/repo}"

cd "$CLONE_DIR"
git fetch --quiet origin main
REF="origin/main"
SHA=$(git rev-parse --short "$REF")

# Per-unit stats: every crate under crates/<tier>/<name> plus the two
# apps. `git ls-files` at the REF so the working tree's state cannot
# leak into the measurement.
units=$(git ls-tree -d --name-only "$REF" crates/core/ crates/modules/ crates/orchestrators/ crates/tenants/ 2>/dev/null; printf 'apps/web\napps/simulator\n')

nodes="[]"
total_files=0
total_lines=0
for u in $units; do
    files=$(git ls-files --with-tree="$REF" "$u" | wc -l)
    [ "$files" -gt 0 ] || continue
    # One pass per unit: `git grep -c ''` counts lines in every tracked
    # text file at the REF (binaries skipped — the instrument, stated).
    lines=$(git grep -c '' "$REF" -- "$u" 2>/dev/null | awk -F: '{s+=$NF} END {print s+0}')
    total_files=$((total_files + files))
    total_lines=$((total_lines + lines))
    nodes=$(printf '%s' "$nodes" | jq \
        --arg id "$u" \
        --argjson files "$files" \
        --argjson lines "$lines" \
        '. + [{id: $id, files: $files, lines: $lines}]')
done

observation=$(jq -n \
    --arg sha "$SHA" \
    --arg observer "boss-observe-codebase" \
    --argjson nodes "$nodes" \
    --argjson total_files "$total_files" \
    --argjson total_lines "$total_lines" \
    '{
        observed_at: (now | todate),
        observer: $observer,
        scope: "codebase",
        ref: $sha,
        totals: {files: $total_files, lines: $total_lines},
        nodes: $nodes
    }')

# The estate door's posture, inherited: a failed POST fails LOUDLY with
# the target named (3ddd8333 — silent-on-curl-failure ships exactly
# once per codebase).
curl -sf -X POST "$JOBS_API/api/estate/observation" \
    -H 'content-type: application/json' \
    -H 'x-boss-user: {"id":"automation:observe-codebase","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}' \
    -d "$observation" >/dev/null \
    || { echo "observe-codebase: POST to $JOBS_API/api/estate/observation FAILED" >&2; exit 1; }

echo "observe-codebase: recorded $(printf '%s' "$nodes" | jq length) units @ $SHA (${total_lines} lines / ${total_files} files)"
