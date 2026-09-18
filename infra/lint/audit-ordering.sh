#!/usr/bin/env bash
# consist: skip — psql against a live database; a tree cannot answer it, and it has its own systemd timer
#
# Causal / temporal ordering sweep over the audit-log-derived
# projections. Per docs/design/correctness-protocol.md, the audit log
# is generated through the live API, whose write-time guards
# (step-blocker 409, ready_when readiness, ledger period-lock) force
# realistic event ordering. This script re-reads the resulting
# projection tables and asserts that ordering actually held — a
# regression net that fires if a seed shortcut, a reordered dispatcher
# handler, or a bad happened_on ever lands events out of causal order.
#
# Unlike conservation-invariants.sh (which checks *steady-state*
# conserved quantities — FG-at-cost closure, WIP burndown, etc. that
# only settle over a full regen), these invariants hold at ANY point in
# the sim, so they are safe to gate a short-window install smoke test.
#
# Each query SELECTs rows that *violate* the invariant; empty = holds.
#
# Usage:
#   PGPASSWORD=boss infra/lint/audit-ordering.sh
# Connection params (override with env vars):
#   PGHOST=127.0.0.1  PGUSER=boss  PGDATABASE=boss

set -euo pipefail

PGHOST="${PGHOST:-127.0.0.1}"
PGUSER="${PGUSER:-boss}"
PGDATABASE="${PGDATABASE:-boss}"

violations=0
run_invariant() {
    local label="$1"
    local sql="$2"
    local rows
    rows=$(psql -h "$PGHOST" -U "$PGUSER" -d "$PGDATABASE" -At -c "$sql" 2>&1) || {
        echo "[ERROR] $label — query failed:"
        echo "$rows" | sed 's/^/    /'
        violations=$((violations + 1))
        return
    }
    if [[ -n "$rows" ]]; then
        echo "[VIOLATION] $label"
        head -10 <<< "$rows" | sed 's/^/    /'
        local n
        n=$(echo "$rows" | wc -l)
        if (( n > 10 )); then
            echo "    ...and $((n - 10)) more"
        fi
        violations=$((violations + 1))
    fi
}

echo "Audit-ordering sweep starting…"
echo

# ---- 1. Per-aggregate lifecycle dates ascend ----
# An entity's lifecycle stamps must be monotonic: you can't pay an
# invoice before it's issued, close a job before it's opened, or
# deliver a shipment before it's shipped.
run_invariant "1. Causal ordering — lifecycle dates ascend per aggregate" "$(cat <<'SQL'
SELECT 'invoice.paid_on<issued_on '          || id::text FROM invoices        WHERE paid_on     IS NOT NULL AND paid_on     < issued_on
UNION ALL SELECT 'invoice.due_on<issued_on ' || id::text FROM invoices        WHERE due_on < issued_on
UNION ALL SELECT 'job.closed_on<opened_on '  || id::text FROM jobs            WHERE closed_on   IS NOT NULL AND closed_on   < opened_on
UNION ALL SELECT 'vendor_invoice.matched_on<received_on '  || id::text FROM vendor_invoices WHERE matched_on  IS NOT NULL AND matched_on  < received_on
UNION ALL SELECT 'vendor_invoice.approved_on<received_on ' || id::text FROM vendor_invoices WHERE approved_on IS NOT NULL AND approved_on < received_on
UNION ALL SELECT 'vendor_invoice.paid_on<approved_on '     || id::text FROM vendor_invoices WHERE paid_on IS NOT NULL AND approved_on IS NOT NULL AND paid_on < approved_on
UNION ALL SELECT 'shipment.shipped_on<created_on '   || id::text FROM shipments WHERE shipped_on   IS NOT NULL AND shipped_on   < created_on
UNION ALL SELECT 'shipment.delivered_on<shipped_on ' || id::text FROM shipments WHERE delivered_on IS NOT NULL AND shipped_on IS NOT NULL AND delivered_on < shipped_on
SQL
)"

# ---- 2. Workflow version pin — Jobs open under the active version ----
# docs/architecture-decisions.md §Jobs, Workflows, Steps: creation is
# blocked against draft/retired kinds, and in-flight Jobs pin to the
# version they opened under. So every Job's (kind, workflow_version)
# must resolve to a real workflows row, and that row must NOT be a
# `draft` — a Job can never open under a draft. It MAY be `retired`:
# an in-flight Job whose version was superseded by a later publish.
# (Catches the regression where workflow_version silently defaulted to
# 1 instead of being stamped with the kind's active version on create.)
run_invariant "2. Workflow version pin — every Job opened under a non-draft version" "$(cat <<'SQL'
SELECT 'job ' || j.id::text || ' kind=' || j.kind || ' v=' || j.workflow_version ||
       CASE WHEN k.kind IS NULL THEN ' — no such workflows row'
            ELSE ' — opened under a draft version' END
  FROM jobs j
  LEFT JOIN workflows k
    ON k.kind = j.kind AND k.version = j.workflow_version
 WHERE k.kind IS NULL
    OR k.status = 'draft'
SQL
)"

# ---- 3. Temporal skeleton — every financial_fact lands in a period ----
# A financial_fact dated outside every defined gl_period means an event
# landed before the books opened (or beyond the period skeleton) — a
# temporal-realism violation period-lock can't catch on its own (it only
# blocks back-dating into *closed* periods). The opening-snapshot facts
# (the day before sim day-1) are fine: they sit inside the opening month
# period (e.g. gl_periods starts 2025-03-01 for a 2025-04-01 epoch).
run_invariant "3. Temporal skeleton — financial_facts fall within a gl_period" "$(cat <<'SQL'
SELECT 'financial_fact ' || ff.id::text || ' happened_on=' || ff.happened_on
  FROM financial_facts ff
 WHERE NOT EXISTS (
    SELECT 1 FROM gl_periods p
     WHERE ff.happened_on BETWEEN p.starts_on AND p.ends_on
 )
SQL
)"

echo
if (( violations > 0 )); then
    echo "audit-ordering: $violations invariant(s) violated."
    cat <<'EOF'

Events landed in an order that couldn't happen in a real run. The audit
log is the system of record, so this is a generation bug (a seed
shortcut, a reordered side-effect handler, or a bad happened_on), never
something to patch in the projection. Trace the offending aggregate's
events in audit_log and fix the Workflow / handler that emitted them out
of order. See docs/design/correctness-protocol.md.
EOF
    exit 1
fi
echo "audit-ordering: clean ($(( $(grep -c '^run_invariant' "$0") - 1 )) invariants checked)."
exit 0
