#!/usr/bin/env bash
#
# The BREWERY's conservation-invariant sweep — the fourteen invariants
# written against Algedonic Ales' chart of accounts and its brewing
# process, moved here from infra/lint/conservation-invariants.sh on
# 2026-09-18 (consolidation H12, backlog 236529aa). They were 14 of that
# platform sweep's 22, and every one names something only this tenant
# has: GL account numbers 1000/1300/1310/1320/2150/2200/2300/3000, the
# production-consume / production-produce batch steps,
# finished_product_inventory.value_cents, the brewery_seed_opening_balance
# fact, revenue spread across >=3 accounts, a period close into 3000. On
# a tenant with its own chart (Algedonic LLC: 1000/1010/2100/3000/3200/
# 4200/4300/4400/6xxx) they are meaningless, so they live with the
# tenant that means them — the same rule that keeps the brewery's
# vocabulary and seeds under examples/brewery/.
#
# WHO RUNS IT. The in-cluster chore
# (infra/cluster/manifests/boss-conservation-invariants.yaml) runs the
# platform sweep on every instance and then `$BOSS_TENANT_DIR/
# conservation-invariants.sh` when the tenant directory the image ships
# carries one — the playground's brewery does. And
# infra/postgres/validate-brewery-sim.sh runs both after a regen, so the
# brewery's release gate keeps all 22.
#
# Each query selects the rows that VIOLATE its invariant; no rows is a
# pass. The helper and the connection rule (DATABASE_URL, else
# PGHOST/PGUSER/PGDATABASE) are infra/lint/lib/conservation.sh's.
#
# Invariants:
#
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
#   V. Cash GL balance non-negative — cash going negative means
#      a payment rule posts against 1000 without cover.
#   W. Expected-opening assets each have a seed JE — every
#      opening balance the seed promises exists as a real
#      journal entry.
#
# Usage:
#   DATABASE_URL=postgres://… examples/brewery/conservation-invariants.sh
#   PGPASSWORD=boss examples/brewery/conservation-invariants.sh   # PGHOST=127.0.0.1 PGUSER=boss PGDATABASE=boss

set -euo pipefail

# shellcheck source=infra/lint/lib/conservation.sh
. "${BOSS_CONSERVATION_LIB:-$(dirname "${BASH_SOURCE[0]}")/../../infra/lint/lib/conservation.sh}"

echo "Brewery conservation-invariant sweep starting…"
echo

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

conservation_verdict brewery-conservation-invariants "$0"
