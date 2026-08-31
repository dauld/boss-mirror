# Seeds vs. emergent state — what belongs in seed data and what doesn't

**Status**: living guidance. Treat any seed file that violates this as a known stop-gap, not a target.

## Frame

The system contributes zero error of its own. Once an event is
recorded correctly, every projection (financial books, inventory
position, headcount, taxes payable) is correct by construction.
If a projection is wrong, the only honest fix is to record a
compensating event, not edit the projection. Seeds that
shortcut artifacts straight into projection tables undermine
this guarantee — they introduce projection rows with no event
provenance, which means a replay of the audit log won't
reproduce them, which means the system has drift it can't
diagnose. See [correctness-protocol.md](correctness-protocol.md)
for the five-property protocol this falls out of.

## The principle

> **Seeds set initial conditions only. Everything downstream emerges from running the system.**

A seed is the brewery's t=0 snapshot — Subjects, registries,
opening positions, in-flight Jobs at their starting step. Once
the simulator (or a real human) starts advancing those Jobs, the
artifacts of the business — invoices, journal entries, vendor
invoices, payroll runs, shipments, support cases — should
emerge from real Job-step transitions: the `boss-dispatcher`
handlers off `step.done.<kind>` write `financial_facts` rows and
mutate `inventory_items`, and every change lands in the audit log.

If a fresh tenant + the simulator running for N ticks doesn't
produce a coherent Finance / Inventory / Shipping page, that's
the signal we want — it tells us a Workflow is incompletely
modeled. Topping up the table with `INSERT INTO ...` hides the
signal and decorates the gap.

## Bypass smell

Two patterns are almost always evidence of bypass:

1. **Aggregate-seed projection** — writing journal entries (or
   any other downstream artifact) directly into the table
   without a paired Job step that should have produced them.
   Shape: a seed that writes month-aggregate JEs because no
   Workflow emits `wholesale-revenue-monthly` / `payroll-monthly`
   financial facts when its `done` transition fires. The right
   fix is at the Workflow layer, not the seed.

2. **Per-row seed of an artifact that should have provenance** —
   `INSERT`ing a year of invoices to fill the trailing-12 because
   `wholesale-keg-order` Jobs lack a `record-invoice` step. Same
   diagnosis: model gap, not seed gap.

## Allowed in seeds

- Opening balances (day-zero JEs, recorded as `financial_facts`
  with `source_table='brewery_seed_opening_balance'`, emitted by
  `boss-brewery-data-seed`). These are the only journal entries
  the seed authors directly.
- Subjects: accounts, vendors, employees, recipes, locations.
- Registries: classes, Workflows, StepTypes, gl_accounts,
  gl_periods (the time skeleton, not the entries inside).
- In-flight Jobs at their starting step. The simulator advances
  them; that's where the rest of the brewery's life-cycle data
  comes from.
- Reference data the simulator needs as input (e.g. recipe
  ingredient ratios, posting rules).

## Not allowed in seeds

- `invoices` / `invoice_line_items`
- `gl_journal_entries` / `gl_journal_lines` (except the single
  opening-balance entry)
- `financial_facts` (except the `source_table='brewery_seed_opening_balance'` openings)
- `vendor_invoices`
- `payroll_runs` / `payroll_run_lines`
- `shipments`
- Inventory drawdowns or stock changes
- `support_cases` produced by account events
- Any row that, in production, would be a side effect of a Job
  step completing.

## Enforcement

Two automatic mechanisms turn this principle into something the
system enforces:

### 1. Provenance check — runtime
Every `financial_facts` row carries `source_table` + `source_id`
naming the upstream record it was projected from — `invoices`,
`payroll_runs`, `tax_filings`, `bank_settlements`,
`revenue_schedules`, or `brewery_seed_opening_balance` for the
openings (the projection rules live in
`crates/modules/boss-ledger/src/rebuild_facts.rs`).
`infra/lint/conservation-invariants.sh` (run hourly by
`boss-conservation-invariants.timer`) sweeps these for orphans.
Invariant E rejects any row whose `source_table='steps'` but
whose `source_id` doesn't resolve to a real `steps` row — a
forged or orphaned fact. That check is a forward guard today: it
passes trivially because no fact is `steps`-sourced yet, and goes
live once Job steps emit facts directly (the planned end state
where a Step's `done` transition is the fact's provenance).

### 2. Bypass-smell lint — CI
`infra/lint/seed-bypass-smell.sh` (wired into CI at
`.github/workflows/ci.yml`) scans every
`examples/<tenant>/seeds/sql/*.sql` file and rejects
`INSERT INTO` statements against a flat denylist: `invoices`,
`invoice_line_items`, `gl_journal_entries`, `gl_journal_lines`,
`financial_facts`, `vendor_invoices`, `payroll_runs`,
`payroll_run_lines`, `shipments`, `tax_filings`.

The denylist forces seeds-side authors to either (a) extend a
Workflow so the simulator produces the artifact, (b) author a
new Workflow, or (c) accept the empty view until the model
catches up. The lint passes with zero seed SQL files in the
tree — emergent state is produced by the sim, not seeded.

## Related

- `crates/modules/boss-ledger/` — the GL layer this principle's
  projections feed; `financial_facts` is the ledger input.
- `docs/design/human-powered-state-machine.md` — the framing
  this principle falls out of: Jobs are the program, and the
  program is the source of truth for downstream state.
