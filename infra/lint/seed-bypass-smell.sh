#!/usr/bin/env bash
#
# Seed bypass-smell lint — enforces the principle in
# `docs/design/seed-vs-emergent-state.md`: seed files set initial
# conditions only, never downstream artifacts that should emerge
# from a Job step.done.
#
# This lint scans every SQL file that reaches a database at bootstrap
# — the migrations under `infra/postgres/schema/` and any `.sql` under
# `examples/` — for `INSERT INTO <denylist>` statements. Hits are
# reported with file + line number + matched table; any hit returns a
# non-zero exit code. The protocol-derivation lives in
# `docs/design/correctness-protocol.md`.
#
# WHERE IT LOOKS, AND WHY THAT CHANGED (backlog cdf2d959, audit H2,
# 2026-09-18). From the playground bundles' retirement until then this
# lint globbed `examples/*/seeds/sql/*.sql`, a directory that no longer
# existed, printed "no seed sql files" and exited 0 on every gate — a
# check that scanned nothing and certified the claim anyway. The seeds
# are TOML and JSON now and reach the system through the API; the one
# place SQL still writes rows into every database at bootstrap is the
# migration set, which is exactly where a projection INSERT would land
# if one came back (01-registries.sql and 40-ledger.sql already seed
# reference rows that way — backlog 718ac982). So the migrations are the
# scan, examples/ stays in scope as the forward guard against a returned
# SQL seed, and a scan of zero files is refused (lib/scanned.sh).
#
# Existing files known to violate the principle are listed in
# the ALLOWLIST below as known stop-gaps. New files MUST pass
# without an allowlist entry, and every entry must name a file
# that exists (lib/allowlist.sh).
#
# Usage:
#   infra/lint/seed-bypass-smell.sh [--strict]
#
# In strict mode, the allowlist is ignored — useful when
# clearing a stop-gap to verify it's actually clean.

set -euo pipefail

LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$LINT_DIR/../.." && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh"
# shellcheck source=infra/lint/lib/allowlist.sh
. "$LINT_DIR/lib/allowlist.sh"

LINT=seed-bypass-smell

# Tables whose rows should only come from a Job step.done +
# matching SideEffectHandler. Any seed-side INSERT INTO against
# these tables is bypass smell.
DENYLIST=(
    "invoices"
    "invoice_line_items"
    "gl_journal_entries"
    "gl_journal_lines"
    "financial_facts"
    "vendor_invoices"
    "payroll_runs"
    "payroll_run_lines"
    "shipments"
    "tax_filings"
)

# Known stop-gaps. Each entry is a file path *relative to the
# repo root*. Removing an entry should cause the lint to flag
# the violation — the workflow is: complete the matching
# Workflow protocol-proof, delete the stop-gap seed, drop the
# allowlist entry.
ALLOWLIST=(
    # All previous entries were the playground_*.sql bundles under
    # examples/brewery/seeds/sql/. Those files have been retired
    # — see docs/design/projection-rebuilders.md § F. The bootstrap
    # path is now `--audit-log-seed` + boss-rebuild-all, so any
    # projection content lands via the event log rather than direct
    # INSERTs. The allowlist is intentionally empty so any new
    # direct-INSERT file trips the smell check on its first commit.
)

STRICT=0
if [[ "${1:-}" == "--strict" ]]; then
    STRICT=1
fi

cd "$REPO_ROOT"

# Build a single grep -E pattern from the denylist. Match
# `INSERT INTO <table>` case-insensitively, allowing for
# whitespace + comments after the table name.
PATTERN="^[[:space:]]*INSERT[[:space:]]+INTO[[:space:]]+($(IFS='|'; echo "${DENYLIST[*]}"))\\b"

# Every SQL file that reaches a database at bootstrap: the migration
# set, and any .sql an example tenant carries (none today; the guard
# against one returning). Sorted so two hosts report in one order.
SEED_FILES=()
while IFS= read -r f; do
    [[ -n "$f" ]] && SEED_FILES+=("$f")
done < <(find infra/postgres/schema examples -type f -name '*.sql' | LC_ALL=C sort)

# Zero files is the defect this lint carried, not a clean tree.
lint_scanned "$LINT" "${#SEED_FILES[@]}" "sql file(s) under infra/postgres/schema and examples"
allowlist_paths_exist "$LINT" "${ALLOWLIST[@]}"

is_allowlisted() {
    local target="$1"
    if (( STRICT )); then
        return 1
    fi
    local entry
    for entry in "${ALLOWLIST[@]}"; do
        if [[ "$target" == "$entry" ]]; then
            return 0
        fi
    done
    return 1
}

violations=0
allowed_with_hits=()

for f in "${SEED_FILES[@]}"; do
    # Pull every matching line + line number. Use grep -i so
    # case quirks don't sneak past.
    matches=$(grep -niE "$PATTERN" "$f" || true)
    if [[ -z "$matches" ]]; then
        continue
    fi

    if is_allowlisted "$f"; then
        allowed_with_hits+=("$f")
        continue
    fi

    echo
    echo "[BYPASS] $f — bootstrap SQL inserts directly into a projection table:"
    while IFS= read -r line; do
        echo "    $line"
    done <<< "$matches"
    violations=$((violations + 1))
done

echo
if (( violations > 0 )); then
    cat <<'EOF'
seed-bypass-smell: bypass detected.

The flagged seed files insert rows into projection tables that
should come from a Job step.done emitting a financial_fact (or
similar). Per docs/design/seed-vs-emergent-state.md, seeds set
initial conditions only. Fix: extend a Workflow so the simulator
produces this artifact, or accept the empty view until the
model catches up.

If this is a known stop-gap that's been deliberately accepted
for now, add it to the ALLOWLIST in this script with a comment naming
the packet that owns it.
EOF
    exit 1
fi

echo "seed-bypass-smell: clean (${#SEED_FILES[@]} sql file(s) checked)."
if (( ${#allowed_with_hits[@]} > 0 )); then
    echo
    echo "  $(echo ${#allowed_with_hits[@]}) known stop-gap(s) skipped via ALLOWLIST:"
    for f in "${allowed_with_hits[@]}"; do
        echo "    $f"
    done
    echo
    echo "  Run with --strict to verify a stop-gap has actually been cleared."
fi
exit 0
