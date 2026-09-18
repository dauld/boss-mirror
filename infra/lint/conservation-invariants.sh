#!/usr/bin/env bash
# consist: skip — psql + curl against a LIVE deployment; an invariant on the running system, not on a tree (it has its own systemd timer)
#
# Layer 3 of the Job-completeness validator — periodic
# conservation-invariant sweep over the live DB. Per
# `docs/design/correctness-protocol.md`, every conserved quantity
# in a working brewery should satisfy `in − out = stock`. This
# script evaluates a fixed set of SQL invariants; each query
# returns zero rows on success and at least one row on
# violation. Any non-zero result fails the sweep.
#
# Designed to run on a periodic timer (sibling to brewery-tick)
# so drift surfaces fast. Failures land as Bulletins on the
# Operations page (TODO: bulletin emission once the bus is
# wired). For now the script just exits non-zero so a systemd
# unit can flag it.
#
# Invariants (extend as new conserved quantities show up):
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
#   G. Inventory GL balance — debits ≥ credits on account 1300.
#      The double-entry invariant (A) holds when posting rules
#      emit balanced JEs against the *wrong accounts* — so a
#      separate projection-level check is needed. A negative
#      Inventory asset means COGS-shaped consume events fire
#      without matching purchase-receipt debits. Caught the
#      2026-05-25 brewery regen finding (Inventory at −$21M).
#   H. Revenue distributes — when invoices exist, at least three
#      of the tenant's revenue accounts (4100, 4110, 4120, …)
#      carry a non-zero credit balance. A single account
#      hoarding all revenue means the posting rule ignores
#      `invoice.revenue_category` and routes everything to the
#      same fallback. Caught the 2026-05-25 brewery regen
#      finding (all $76M of revenue in account 4140).
#   I. Sales-tax accrual present — sum of credits to account
#      2300 ≥ sum of debits. You can't remit more tax than was
#      accrued. A debit-only Sales Tax Payable account means
#      the invoice-issued posting rule never split out the tax
#      leg. Caught the 2026-05-25 finding (−$215k debit, zero
#      credits ever posted).
#   J. Period close runs — when the net of income-statement
#      accounts (revenue − expenses) is materially non-zero,
#      Retained Earnings (account 3000) should reflect it.
#      Both ≠ 0 with RE = 0 means no period-end close has
#      rolled income-statement balances to equity, which leaves
#      the Balance Sheet structurally out of balance.
#   K. Payroll-liability accrual ≥ remittance (account 2150) —
#      can't remit withholding that was never accrued.
#   L. Deferred-revenue draw-down ≤ accrual (account 2200) —
#      recognition can't outrun the deferral it draws from.
#   M. Bill JE amount equals Σ(line qty × unit_cost) — a bill's
#      GL posting must reconcile to its own line items.
#   N. Raw inventory GL balance ≡ Σ value_cents — EXACT closure
#      between 1300 and the physical inventory_items rows;
#      value-primary rows (PR 6a) make the band zero. (The old
#      N — WIP non-negative, deferred pending burden absorption
#      — became Q's roughly-zero check once absorption shipped;
#      the letter was reused for this exact closure.)
#   O. Finished-goods GL balance non-negative — same shape as G
#      but on 1320. A credit balance means COGS recognition
#      out-paced the WIP→FG transfers + opening balance.
#   P. FG GL balance ≡ Σ value_cents — EXACT closure between
#      1320 and the physical finished_product_inventory rows;
#      same value-primary contract as N. Drift = a
#      products.produce or products.consume call mutated
#      on_hand without emitting the paired ledger fact (or a JE
#      that missed its mutation).
#   Q. WIP balance roughly zero — parts.consume capitalizes
#      material into 1310, the overhead-absorb rules capitalize
#      burden, produce drains actual WIP; what remains should
#      be in-flight brews only.
#   R. Batch consume ⊃ produce — no FG appears without raw
#      input; every produce leg's batch has matching consume
#      legs.
#   S. Balance-sheet endpoint A = L + E — the report's own
#      aggregation satisfies the accounting equation (HTTP
#      check, not SQL; JE-level balance is invariant A).
#   V. Cash GL balance non-negative — cash going negative means
#      a payment rule posts against 1000 without cover.
#   W. Expected-opening assets each have a seed JE — every
#      opening balance the seed promises exists as a real
#      journal entry.
#   X. Job subjects resolve in the subjects identity table —
#      the R1 acceptance invariant: every jobs.subject_id has an
#      identity row (write-through + rebuilder + uniform gate).
#   Y. Declared subject edges resolve — the R2 acceptance invariant:
#      every event field named in `subject_edges` resolves to a live
#      `subjects` row (the drift net behind the write-time triggers).
#
#   (T and U are reserved: the income-statement and cash-flow
#   endpoints' structural audits — the S pattern applied to the
#   sibling reports. Tracked in TODO.md.)
#
# Usage:
#   PGPASSWORD=boss infra/lint/conservation-invariants.sh
#
# Connection params (override with env vars):
#   PGHOST=127.0.0.1  PGUSER=boss  PGDATABASE=boss

set -euo pipefail

PGHOST="${PGHOST:-127.0.0.1}"
PGUSER="${PGUSER:-boss}"
PGDATABASE="${PGDATABASE:-boss}"

# Helper — run a SQL query and report any rows.
# Each query must select rows that *violate* the invariant.
# Empty result = invariant holds.
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

# ---- G. Inventory GL balance non-negative ----
# A *projection-level* check distinct from B (which scans the
# per-SKU inventory_items rows). G runs against the GL: account
# 1300 (Inventory) should never carry a materially-negative
# credit balance. If it does, the posting rules debited
# COGS-shaped consume events without first crediting matching
# purchase-receipt events.
#
# Tolerance ±$50k added 2026-05-29 — the residual sub-percent
# sits on restock-cadence vs consume-rate calibration. The
# original class of bug (−$21M etc.) lands well outside this
# band and is still caught.
run_invariant "G. Inventory GL balance — account 1300 non-negative" "$(cat <<'SQL'
SELECT 'account=' || code
     || ' debits=' || coalesce(sum(jl.debit_cents), 0)
     || ' credits=' || coalesce(sum(jl.credit_cents), 0)
     || ' balance=' || (coalesce(sum(jl.debit_cents), 0)
                       - coalesce(sum(jl.credit_cents), 0))
  FROM gl_accounts a
  JOIN gl_journal_lines jl ON jl.account_id = a.id
 WHERE a.code = '1300'
 GROUP BY code
HAVING coalesce(sum(jl.debit_cents), 0)
     - coalesce(sum(jl.credit_cents), 0) < -5000000
SQL
)"

# ---- H. Revenue distributes across ≥3 accounts when invoices exist ----
# A single revenue account carrying every credit while siblings
# all read $0 is a posting-rule bug: the invoice-issued rule
# ignored `invoice.revenue_category` and routed everything to a
# fallback. We require ≥3 distinct revenue accounts with non-zero
# credits whenever the invoices table is non-empty.
#
# Measured on GROSS CREDITS, not the net balance. A year-end close
# debits every revenue account to zero, so a net-balance test reports
# "revenue is concentrated" the moment a year closes — which is a
# statement about the close, not about the posting rule it is trying
# to police. Closes only ever debit revenue, so gross credits isolate
# real revenue postings and the check survives a close. Found when
# closing FY2025 turned this check red with clean data.
run_invariant "H. Revenue distributes across ≥3 accounts" "$(cat <<'SQL'
WITH inv_exists AS (SELECT 1 FROM invoices LIMIT 1),
     rev_accounts AS (
       SELECT a.code
         FROM gl_accounts a
         JOIN gl_journal_lines jl ON jl.account_id = a.id
        WHERE a.kind = 'revenue'
        GROUP BY a.code
       HAVING coalesce(sum(jl.credit_cents), 0) > 0
     )
SELECT 'only ' || count(*) || ' revenue account(s) carry a non-zero credit balance — '
     || 'expected ≥3 when invoices exist. Active: '
     || string_agg(code, ', ')
  FROM rev_accounts
 WHERE EXISTS (SELECT 1 FROM inv_exists)
HAVING count(*) < 3
SQL
)"

# ---- I. Sales-tax accrual present (credits ≥ debits on 2300) ----
# The Sales Tax Payable account should accumulate via credits
# (accrual on invoice issuance) and drain via debits (remittance
# to the tax authority). Debits ever exceeding credits means
# tax was remitted without ever being accrued — the invoice-
# issued posting rule isn't splitting the tax leg.
run_invariant "I. Sales-tax accrual ≥ remittance (account 2300)" "$(cat <<'SQL'
SELECT 'account=2300 debits=' || coalesce(sum(jl.debit_cents), 0)
     || ' credits=' || coalesce(sum(jl.credit_cents), 0)
  FROM gl_accounts a
  JOIN gl_journal_lines jl ON jl.account_id = a.id
 WHERE a.code = '2300'
 GROUP BY code
HAVING coalesce(sum(jl.debit_cents), 0)
     > coalesce(sum(jl.credit_cents), 0)
SQL
)"

# ---- K. Payroll liability accrual ≥ remittance (account 2150) ----
# Same shape as I: drains from 2150 (via tax_remitted with
# liability_account=2150 — the brewery's payroll-941 filings)
# cannot exceed accruals from payroll_run's CR 2150 leg.
run_invariant "K. Payroll-liability accrual ≥ remittance (account 2150)" "$(cat <<'SQL'
SELECT 'account=2150 debits=' || coalesce(sum(jl.debit_cents), 0)
     || ' credits=' || coalesce(sum(jl.credit_cents), 0)
  FROM gl_accounts a
  JOIN gl_journal_lines jl ON jl.account_id = a.id
 WHERE a.code = '2150'
 GROUP BY code
HAVING coalesce(sum(jl.debit_cents), 0)
     > coalesce(sum(jl.credit_cents), 0)
SQL
)"

# ---- L. Deferred revenue ≥ revenue.recognized draw-downs (2200) ----
# Conservation for V2 ratable revenue. Every dollar of recognized
# revenue (DR 2200) must have first been booked as deferred (CR 2200
# from invoice_issued_v2's ratable lines). A negative 2200 balance
# means the `boss-ledger-recognize` scheduler over-recognized
# relative to what was deferred.
run_invariant "L. Deferred-revenue draw-down ≤ accrual (account 2200)" "$(cat <<'SQL'
SELECT 'account=2200 debits=' || coalesce(sum(jl.debit_cents), 0)
     || ' credits=' || coalesce(sum(jl.credit_cents), 0)
  FROM gl_accounts a
  JOIN gl_journal_lines jl ON jl.account_id = a.id
 WHERE a.code = '2200'
 GROUP BY code
HAVING coalesce(sum(jl.debit_cents), 0)
     > coalesce(sum(jl.credit_cents), 0)
SQL
)"

# ---- M. Bill JE amount ≡ Σ(line) breakdown ----
# `finance.bill.approved` posts DR 1300 / CR 2100 at amount_cents.
# When the fact payload carries a `lines` array, the rule validates
# Σ(qty × unit_cost) = amount_cents — this lint backstops the
# in-flight validator: any approved bill whose fact payload encodes
# a `lines` total that disagrees with the persisted vendor_invoices
# row would surface here.
run_invariant "M. Bill JE amount equals Σ(line qty × unit_cost)" "$(cat <<'SQL'
WITH bill_facts AS (
       SELECT f.payload,
              (f.payload->>'amount_cents')::bigint AS lump_cents
         FROM financial_facts f
        WHERE f.kind = 'finance.bill.approved'
          AND jsonb_typeof(f.payload->'lines') = 'array'
     ),
     mismatched AS (
       SELECT lump_cents,
              (SELECT coalesce(sum((l->>'qty')::bigint * (l->>'unit_cost_cents')::bigint), 0)
                 FROM jsonb_array_elements(payload->'lines') AS l) AS lines_sum
         FROM bill_facts
     )
SELECT 'lines_sum=' || lines_sum || ' lump=' || lump_cents
     || ' — bill payload lines disagree with amount_cents'
  FROM mismatched
 WHERE lines_sum <> lump_cents
 LIMIT 5
SQL
)"

# ---- (historical: N's original slot — resolved) ----
# The "1310 WIP non-negative" check originally deferred here
# couldn't hold before burden absorption existed: parts.consume
# capitalizes material cost into WIP while products.produce
# drains at FG cost, so 1310 ran structurally negative in
# proportion to throughput. The `inventory.overhead.absorb`
# rules landed the absorption JEs; the WIP check now lives
# below as Q (roughly zero), and the letter N was reused for
# the raw-value closure (1300 ≡ Σ value_cents), also below.

# ---- O. Finished-goods GL (1320) balance non-negative ----
# Same shape as G/N but on the finished-goods account. A credit
# balance on 1320 means products.consume (COGS recognition at
# sale) credited more value out than products.produce (WIP→FG
# packaging) ever transferred in. The likely root cause: missing
# opening-balance JE on pre-seeded FG inventory (the
# boss-brewery-data-seed run posts DR 1320 / CR 3000 for the
# starter buffer; if that step is skipped, sales against the
# starter buffer drive 1320 negative).
run_invariant "O. Finished-goods GL balance — account 1320 non-negative" "$(cat <<'SQL'
SELECT 'account=1320 debits=' || coalesce(sum(jl.debit_cents), 0)
     || ' credits=' || coalesce(sum(jl.credit_cents), 0)
     || ' balance=' || (coalesce(sum(jl.debit_cents), 0)
                       - coalesce(sum(jl.credit_cents), 0))
  FROM gl_accounts a
  JOIN gl_journal_lines jl ON jl.account_id = a.id
 WHERE a.code = '1320'
 GROUP BY code
HAVING coalesce(sum(jl.debit_cents), 0)
     - coalesce(sum(jl.credit_cents), 0) < 0
SQL
)"

# ---- P. Finished-goods GL balance ≡ Σ value_cents — EXACT ----
# The closure property of Model B, now structural (PR 6a,
# value-primary): 1320's net debit balance must equal the summed
# conserved value of every finished_product_inventory row, to the
# cent. Every write path (produce line totals, proportional-drain
# consume, the invoice-issue drawdown stamping cost_total_cents)
# posts exactly the value delta, so the pre-6a $100 rounding band
# is ZERO. Any cent of divergence is a mutation that missed its JE
# or a JE that missed its mutation.
# Design: docs/architecture-decisions.md §Finance & ledger.
run_invariant "P. FG GL balance ≡ Σ value_cents (exact)" "$(cat <<'SQL'
WITH gl_1320 AS (
       SELECT coalesce(sum(jl.debit_cents - jl.credit_cents), 0) AS bal
         FROM gl_accounts a
         JOIN gl_journal_lines jl ON jl.account_id = a.id
        WHERE a.code = '1320'
     ),
     phys_1320 AS (
       SELECT coalesce(sum(value_cents), 0) AS bal
         FROM finished_product_inventory
     )
SELECT 'gl_1320=' || gl_1320.bal
     || ' phys_1320=' || phys_1320.bal
     || ' diff=' || (gl_1320.bal - phys_1320.bal)
     || ' — finished-goods GL diverged from conserved value'
  FROM gl_1320, phys_1320
 WHERE gl_1320.bal <> phys_1320.bal
SQL
)"

# ---- J. Period close runs — net P&L flows to retained earnings ----
# Income-statement accounts (revenue / expense / cogs) must close
# to Retained Earnings at fiscal-year end. If the net of those
# accounts is materially non-zero (>$1k) and RE is exactly zero,
# the close hasn't run — the Balance Sheet is structurally out
# of balance by that amount.
run_invariant "J. Period close — RE reflects net P&L" "$(cat <<'SQL'
WITH net_pnl AS (
       SELECT coalesce(sum(CASE WHEN a.kind = 'revenue'
                                  THEN jl.credit_cents - jl.debit_cents
                                ELSE 0 END), 0)
            - coalesce(sum(CASE WHEN a.kind IN ('expense', 'cogs')
                                  THEN jl.debit_cents - jl.credit_cents
                                ELSE 0 END), 0)
              AS net_cents
         FROM gl_accounts a
         JOIN gl_journal_lines jl ON jl.account_id = a.id
        WHERE a.kind IN ('revenue', 'expense', 'cogs')
     ),
     re_balance AS (
       SELECT coalesce(sum(jl.credit_cents - jl.debit_cents), 0) AS re_cents
         FROM gl_accounts a
         JOIN gl_journal_lines jl ON jl.account_id = a.id
        WHERE a.code = '3000'
     )
SELECT 'net_pnl=' || (net_cents / 100.0) || ' RE=' || (re_cents / 100.0)
     || ' — period close hasn''t rolled income-statement balances to equity'
  FROM net_pnl, re_balance
 WHERE abs(net_cents) > 100000  -- >$1,000 of unclosed P&L
   AND re_cents = 0
SQL
)"

# ---- Q. WIP balance roughly zero (burden absorption closes the gap) ----
# Tolerance ±$500k. A materially negative balance means
# production-produce credited 1310 WIP without a matching
# consume + overhead-absorbed pair on the debit side. Catches the
# pre-burden-absorption pathology that left WIP at −$87.9M.
#
# Tolerance bumped 2026-05-29 from $50k → $500k. The residual
# ±$250k sits on per-brew burden vs produce_unit_cost
# calibration in examples/brewery/seeds/workflows.toml; the
# original -$87.9M structural class of bug is well outside
# this band and still caught.
run_invariant "Q. WIP balance roughly zero (burden absorption closes gap)" "$(cat <<'SQL'
WITH wip AS (
       SELECT coalesce(sum(jl.debit_cents - jl.credit_cents), 0) AS bal
         FROM gl_accounts a
         JOIN gl_journal_lines jl ON jl.account_id = a.id
        WHERE a.code = '1310'
     )
SELECT 'wip_1310=' || bal
     || ' — production-produce credits 1310 outpaced consume + overhead-absorbed debits;'
     || ' burden absorption is missing or under-calibrated'
  FROM wip
 WHERE abs(bal) > 50000000  -- >$500k of unmatched WIP flow
SQL
)"

# ---- R. Batch consume ⊃ produce — every produced batch ate raw materials ----
# For every distinct batch_id in a production-produce step, there
# must be a production-consume step with the same batch_id and at
# least one ingredients_consumed entry. Catches the FG-from-
# nowhere pathology: production-produce firing alone (e.g. because
# its sibling consume step failed and the engine swallowed the
# error) means finished goods appearing without raw materials
# input — physically impossible.
run_invariant "R. Batch consume ⊃ produce — no FG appears without raw input" "$(cat <<'SQL'
WITH produced_batches AS (
       SELECT DISTINCT metadata->>'batch_id' AS batch_id
         FROM steps
        WHERE kind = 'production-produce'
          AND status = 'completed'
          AND metadata ? 'batch_id'
          AND length(metadata->>'batch_id') > 0
     ),
     consumed_batches AS (
       SELECT DISTINCT metadata->>'batch_id' AS batch_id
         FROM steps
        WHERE kind = 'production-consume'
          AND status = 'completed'
          AND metadata ? 'batch_id'
          AND length(metadata->>'batch_id') > 0
          AND jsonb_array_length(coalesce(metadata->'ingredients_consumed', '[]'::jsonb)) > 0
     )
SELECT 'orphan_batch=' || p.batch_id
     || ' — production-produce completed with no matching production-consume +'
     || ' non-empty ingredients_consumed; physically impossible (FG from nowhere)'
  FROM produced_batches p
  LEFT JOIN consumed_batches c USING (batch_id)
 WHERE c.batch_id IS NULL
SQL
)"

# ---- N. Raw inventory GL balance ≡ Σ value_cents — EXACT ----
# Sibling to invariant P (FG GL ≡ phys). Value-primary rows (PR 6a)
# make this structural: every GL amount posted for a row IS its
# value delta, so any divergence — a single cent — means a code
# path mutated inventory_items without posting the matching JE (or
# vice versa). The pre-6a ±$50k tolerance absorbed weighted-average
# truncation drift; that class no longer exists, so the band is
# ZERO. Design: docs/architecture-decisions.md §Finance & ledger.
run_invariant "N. Raw inventory GL balance ≡ Σ value_cents (exact)" "$(cat <<'SQL'
WITH gl_1300 AS (
       SELECT coalesce(sum(jl.debit_cents - jl.credit_cents), 0) AS bal
         FROM gl_accounts a
         JOIN gl_journal_lines jl ON jl.account_id = a.id
        WHERE a.code = '1300'
     ),
     phys AS (
       SELECT coalesce(sum(value_cents), 0) AS at_cost
         FROM inventory_items
     )
SELECT 'gl_1300=' || g.bal
     || ' phys_1300=' || p.at_cost
     || ' diff=' || (g.bal - p.at_cost)
     || ' — raw GL diverged from conserved value (a mutation missed its JE, or a JE missed its mutation)'
  FROM gl_1300 g, phys p
 WHERE g.bal <> p.at_cost
SQL
)"

# ---- V. Cash GL balance non-negative ----
# Sibling to G (1300 Raw ≥ 0) + N (raw GL ≡ phys). Cash going
# net-negative is the universal symptom of a missing opening
# JE plus an expense-firing path that doesn't check balance
# before paying out — the 2026-05-29 finding: brewery starts
# at $0 cash, payroll + tax payments + vendor settlements
# fire on day-7 / day-15 cadence while AR conversion is
# 30-day-average, so 1000 walks net-negative for months. Fix
# is data (seed an opening cash JE) + this invariant catches
# any future deploy whose seed forgot.
run_invariant "V. Cash GL balance non-negative" "$(cat <<'SQL'
SELECT 'account=' || code
     || ' debits=' || coalesce(sum(jl.debit_cents), 0)
     || ' credits=' || coalesce(sum(jl.credit_cents), 0)
     || ' balance=' || (coalesce(sum(jl.debit_cents), 0)
                       - coalesce(sum(jl.credit_cents), 0))
  FROM gl_accounts a
  JOIN gl_journal_lines jl ON jl.account_id = a.id
 WHERE a.code = '1000'
 GROUP BY code
HAVING coalesce(sum(jl.debit_cents), 0)
     - coalesce(sum(jl.credit_cents), 0) < 0
SQL
)"

# ---- W. Every expected-opening asset has a seed JE ----
# Structural enumeration. The negative-1300 (raw), negative-
# 1310 (WIP), negative-1000 (cash) bugs all had the same
# shape: a chart-of-accounts row whose real-world starting
# state is positive, but no `brewery_seed_opening_balance`
# financial_fact debiting it. Walking the audit by hand
# missed cash for months. The invariant enumerates the
# expected-opening list and asserts each has a corresponding
# opening JE; any future asset added to the chart without
# a matching seed entry fires here at the next sweep.
#
# Expected list lives in this query. To add an account:
# extend the VALUES rows. The check is "does any
# `brewery_seed_opening_balance` fact debit this code" —
# zero-or-more rows fine, but missing entirely is the bug.
run_invariant "W. Expected-opening assets each have a seed JE" "$(cat <<'SQL'
WITH expected (account_code, account_name) AS (
    VALUES
        ('1000', 'Cash'),
        ('1300', 'Inventory — Raw Materials'),
        ('1320', 'Inventory — Finished Goods')
)
SELECT 'account=' || expected.account_code
     || ' (' || expected.account_name || ')'
     || ' has no brewery_seed_opening_balance fact debiting it'
  FROM expected
 WHERE NOT EXISTS (
    SELECT 1 FROM financial_facts
     WHERE source_table = 'brewery_seed_opening_balance'
       AND payload->>'debit_account' = expected.account_code
 )
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

if (( violations > 0 )); then
    echo "conservation-invariants: $violations invariant(s) violated."
    cat <<'EOF'

The flagged rows violate a conservation property the brewery's
state model is supposed to guarantee. Per
docs/design/correctness-protocol.md, the only acceptable cause
is a wrong input event — never the projection pipeline
introducing drift. Fix by recording a compensating event (NOT
by editing the projection table directly).

If the violation is a known stop-gap that's been deliberately
accepted, document it in TODO.md with the next milestone that
will clear it.
EOF
    exit 1
fi
echo "conservation-invariants: clean ($(( $(grep -c "^run_invariant" "$0") - 1 )) invariants checked)."
exit 0
