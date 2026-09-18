#!/usr/bin/env bash
# consist: skip — psql + curl against a LIVE deployment; an invariant on the running system, not on a tree (it has its own systemd timer)
#
# Layer 3 of the Job-completeness validator — the PLATFORM's
# conservation-invariant sweep over a live BOSS database. Per
# `docs/design/correctness-protocol.md`, every conserved quantity
# should satisfy `in − out = stock`. This script evaluates the
# invariants that hold for EVERY tenant — they name platform tables
# and platform facts only, never a chart of accounts — and each query
# returns zero rows on success and at least one row on violation. Any
# non-zero result fails the sweep.
#
# WHO RUNS IT. The in-cluster chore
# infra/cluster/manifests/boss-conservation-invariants.yaml, hourly,
# on every instance against that instance's own database (DATABASE_URL
# from its Secret), filing a maintenance-conservation-invariants packet
# on that instance's system of record; and
# infra/postgres/validate-brewery-sim.sh after a regen. Until
# 2026-09-18 (consolidation H12, backlog 236529aa) it ran ONLY on
# boss-gcp against PGHOST=127.0.0.1 — the retired second stack's
# postgres, never the system of record — and 14 of its 22 invariants
# were the brewery's chart of accounts and brewing process written into
# a platform sweep. Those fourteen (G H I J K L M N O P Q R V W) now
# live where they mean something: examples/brewery/
# conservation-invariants.sh, which the same chore runs after this one
# when the tenant directory the image ships carries it.
#
# Invariants (extend as new conserved quantities show up — and a new
# one that names an account code or a tenant's table belongs in the
# tenant's sweep, not here):
#
#   A. Trial balance — every gl_journal_entry's lines sum to
#      zero (debits = credits). Already enforced by a DB
#      trigger, but the sweep catches a manual INSERT that
#      sneaks past.
#   B. Inventory non-negative — no `inventory_items.on_hand <
#      0`. The schema doesn't enforce this.
#   C. Closed jobs have closed_on — every job whose status is
#      `closed` must have a non-null closed_on.
#   D. Paid invoices have paid_on — same shape on the AR side.
#   E. Provenance — every financial_facts row whose
#      `source_table='steps'` must point to a real `steps.id`.
#      The `seed-vs-emergent-state.md` principle in action: any
#      orphan fact is a forged fact.
#   F. AP cover — every `vendor_invoices.status='paid'` row
#      must have a non-null paid_on.
#   X. Job subjects resolve in the subjects identity table —
#      the R1 acceptance invariant: every jobs.subject_id has an
#      identity row (write-through + rebuilder + uniform gate).
#   Y. Declared subject edges resolve — the R2 acceptance invariant:
#      every event field named in `subject_edges` resolves to a live
#      `subjects` row (the drift net behind the write-time triggers).
#   S. Balance-sheet endpoint A = L + E — the report's own
#      aggregation satisfies the accounting equation (HTTP
#      check against LEDGER_BASE, not SQL; JE-level balance is
#      invariant A). Not counted in the "invariants checked"
#      line, which counts the SQL invariants.
#
#   (T and U are reserved: the income-statement and cash-flow
#   endpoints' structural audits — the S pattern applied to the
#   sibling reports. Tracked in TODO.md.)
#
# Usage:
#   DATABASE_URL=postgres://… infra/lint/conservation-invariants.sh
#   PGPASSWORD=boss infra/lint/conservation-invariants.sh   # PGHOST=127.0.0.1 PGUSER=boss PGDATABASE=boss
#   LEDGER_BASE=http://host:7080   # invariant S; the in-cluster chore names the instance's ledger door

set -euo pipefail

# The helper, the connection rule and the verdict are shared with the
# brewery's sweep — one definition of run_invariant.
# shellcheck source=infra/lint/lib/conservation.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/conservation.sh"

echo "Conservation-invariant sweep starting…"
echo

# ---- A. Trial balance ----
run_invariant "A. Trial balance — every JE sums to zero" "$(cat <<'SQL'
SELECT je.id || ': debits=' || sum(jl.debit_cents) || ', credits=' || sum(jl.credit_cents)
  FROM gl_journal_entries je
  JOIN gl_journal_lines jl ON jl.journal_entry_id = je.id
 GROUP BY je.id
HAVING sum(jl.debit_cents) <> sum(jl.credit_cents)
SQL
)"

# ---- B. Inventory non-negative ----
run_invariant "B. Inventory non-negative — no negative on_hand" "$(cat <<'SQL'
SELECT part_sku || ': on_hand=' || on_hand
  FROM inventory_items
 WHERE on_hand < 0
SQL
)"

# ---- C. Closed jobs have closed_on ----
run_invariant "C. Closed jobs have closed_on" "$(cat <<'SQL'
SELECT id::text
  FROM jobs
 WHERE status = 'closed' AND closed_on IS NULL
 LIMIT 50
SQL
)"

# ---- D. Paid invoices have paid_on ----
run_invariant "D. Paid invoices have paid_on" "$(cat <<'SQL'
SELECT id
  FROM invoices
 WHERE status = 'paid' AND paid_on IS NULL
 LIMIT 50
SQL
)"

# ---- E. Provenance — financial_facts.source_table='steps' must resolve ----
# Note: today's seed bundle still aggregate-seeds via source_table='opening',
# 'invoices', 'payroll_runs' etc; once Job-level fact production ships, the
# canonical source_table for engine-emitted facts becomes 'steps'. Until then
# this is a future-state guard — passes trivially when no source_table='steps'
# rows exist.
run_invariant "E. Provenance — every steps-sourced fact resolves" "$(cat <<'SQL'
SELECT f.id::text
  FROM financial_facts f
 WHERE f.source_table = 'steps'
   AND f.source_id IS NOT NULL
   AND NOT EXISTS (
       SELECT 1 FROM steps s WHERE s.id::text = f.source_id
   )
 LIMIT 50
SQL
)"

# ---- F. AP cover — paid vendor invoices have paid_on ----
run_invariant "F. Paid vendor invoices have paid_on" "$(cat <<'SQL'
SELECT id
  FROM vendor_invoices
 WHERE status = 'paid' AND paid_on IS NULL
 LIMIT 50
SQL
)"
# ---- X. Every Job subject resolves in the subjects identity table ----
# The R1 acceptance invariant (subject-model design, approved
# 2026-07-15): jobs.subject_id + kind must have an identity row —
# write-through mints on create, the rebuilder reproduces from the
# log, and the uniform existence gate refuses ghosts at the door.
# A violation here means a mint path was missed or a subject was
# removed without retiring its identity.
run_invariant "X. Job subjects resolve in the subjects identity table" "$(cat <<'SQL'
SELECT j.kind || ': ' || j.subject_kind || '/' || j.subject_id
  FROM jobs j
 WHERE NOT EXISTS (
     SELECT 1 FROM subjects s
      WHERE s.kind = j.subject_kind AND s.id = j.subject_id
 )
 LIMIT 20
SQL
)"

# ---- Y. Every declared subject edge resolves ----
# The R2 acceptance invariant (subject-model design): the E-invariant
# generalized over the `subject_edges` registry. The outbox/audit_log
# triggers ABORT a write referencing a missing subject at write time;
# this sweep is the drift net — it walks every declared edge against
# the whole audit_log and catches anything that predates its rule or
# rode in under `on_missing='warn'`. For each edge (source_kind,
# field_path → target_kind) it finds any event of source_kind whose
# payload field names a subject that isn't in `subjects`.
run_invariant "Y. Declared subject edges resolve in the subjects table" "$(cat <<'SQL'
SELECT a.kind || '.' || e.field_path || ' -> '
       || COALESCE(e.target_kind,
                   a.payload #>> string_to_array(e.target_kind_path, '.'))
       || ':' || (a.payload #>> string_to_array(e.field_path, '.'))
  FROM subject_edges e
  JOIN audit_log a ON a.kind = e.source_kind
 WHERE (a.payload #>> string_to_array(e.field_path, '.')) IS NOT NULL
   AND (a.payload #>> string_to_array(e.field_path, '.')) <> ''
   AND COALESCE(e.target_kind,
                a.payload #>> string_to_array(e.target_kind_path, '.')) IS NOT NULL
   AND COALESCE(e.target_kind,
                a.payload #>> string_to_array(e.target_kind_path, '.')) <> ''
   AND NOT EXISTS (
       SELECT 1 FROM subjects s
        WHERE s.kind = COALESCE(e.target_kind,
                                a.payload #>> string_to_array(e.target_kind_path, '.'))
          AND s.id = a.payload #>> string_to_array(e.field_path, '.')
   )
 LIMIT 20
SQL
)"
# ---- S. Balance-sheet endpoint: A = L + E ----
# Asserts that GET /api/ledger/balance-sheet's own aggregation
# satisfies the fundamental accounting equation. Distinct from
# the JE-level trial balance (invariant A): every JE balances
# by trigger, but a reporting endpoint can still mis-roll the
# categories (the 2026-05-29 finding: a fiscal-year-start date
# filter on the YTD-net-income calc silently excluded all
# pre-Jan-1 revenue + expense, leaving the BS off by ~$22M on
# any tenant whose first year hadn't closed yet). The invariant
# closes that bug class structurally — any future change to the
# aggregation that breaks the equation fires here.
echo
LEDGER_BASE="${LEDGER_BASE:-http://127.0.0.1:7080}"
# `/api/ledger/*` is policy-gated (a `ledger` read grant), so this
# sweep has to present an identity like any other reader. It used to
# call bare, which stopped working the day the gate landed and turned
# a real invariant into a permanently-erroring check — a sweep that
# always errors is a sweep nobody reads.
LEDGER_READER='{"id":"automation:conservation-sweep","role":"audit-readonly","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'
bs_response=$(curl -sS --fail -H "x-boss-user: $LEDGER_READER" "$LEDGER_BASE/api/ledger/balance-sheet" 2>&1) || {
    echo "[ERROR] S. Balance-sheet endpoint — fetch failed:"
    echo "$bs_response" | sed 's/^/    /'
    violations=$((violations + 1))
}
if [[ -n "$bs_response" ]]; then
    imbalance=$(echo "$bs_response" | jq -r '
        (.total_assets_cents // 0)
        - ((.total_liabilities_cents // 0) + (.total_equity_cents // 0))
    ' 2>/dev/null)
    if [[ -z "$imbalance" ]]; then
        echo "[ERROR] S. Balance-sheet endpoint — response not JSON-parseable"
        violations=$((violations + 1))
    elif [[ "$imbalance" != "0" ]]; then
        echo "[VIOLATION] S. Balance-sheet endpoint — A != L + E"
        echo "    imbalance=$imbalance cents — the endpoint's aggregation drifted from trial balance"
        violations=$((violations + 1))
    fi
fi

conservation_verdict conservation-invariants "$0"
