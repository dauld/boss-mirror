# Design: brewery fidelity — making Algedonic Ales run like a real brewery

**Status**: approved — all five questions resolved in review (packet `4a39c1df`); carried to a file 2026-08-22.

**Origin**: David, 2026-08-21: *"take a thorough look at our simulator of
all the other brewery functions. I think we have plenty of improvement to
making Algedonic Ales actually function like a real brewery"* — with the
constraint that anchors everything below: *"we may end up with a brewery
that looks significantly more efficient than normal, and that is okay as
long as the actors in our simulator are modeled reasonably, like a person
can't work more than 8 hours."*

Two survey passes (production floor; commercial + finance) plus direct
measurement of the live tenant produced this map. The doc names what is
already excellent, what is fake, and a ranked program for closing the gap
— with the first two protocol experiments already running on the cluster.

## The design principle: actor realism is the honesty budget

The brewery is allowed to beat industry norms. Protocols are registry
data here — cheaper to change than in any real company — so Algedonic
Ales *should* end up better-coordinated than a typical brewer of its
size. What it is not allowed to do is get there by impossible actors.

That splits step duration into two different facts the sim currently
conflates:

- **Labor-bound work** (mash-in, keg washing, a tasting panel): bounded
  by people. A person contributes at most 8 labor-hours per simulated
  day, and a step needing 3 of them takes 3 — not a uniform default.
- **Wall-clock-bound work** (fermentation, conditioning, an ACH
  settlement window): bounded by physics or third parties. No amount of
  staffing compresses it, and modeling it as one 8-hour step is not
  "efficiency" — it is fiction.

Today every step runs `typical_duration_hours` from its StepType kind,
default 8.0, so a fermentation completes in one workday and a whole brew
closes in ~19.8 uniform days. Headcount already has a conservation
invariant; labor-hours does not. The program below treats "no person
exceeds 8 labor-hours/day" as an invariant to *enforce*, not a vibe.

## Measured reality

Direct SoR queries against the live tenant, 2026-08-21:

- **batch-qc-hold has never fired.** Zero packets, ever. The workflow is
  well-formed (5 steps) but nothing spawns it — no `job_rate`, no
  dispatcher rule, no engine hook. QC exists as authored intent only.
- **Every brew closes in ~19.8 days, uniformly.** No variance by style,
  season, or anything else — the signature of default-duration steps
  rather than a modeled process.
- **All 3,491 wholesale orders are byte-identical**: the same 10 lines,
  15 kegs, $1,695.00, because order content comes verbatim from
  `metadata_defaults` and the sim engine has no per-job overlay hook.
- **The taproom closes a shift on only ~40% of days.** A real taproom
  closes the register every night it is open.
- **The sale pipeline is dead** — no packet since 2026-07-23 — and when
  it did run, every prospect terminated `onboarded`; the `qualify` step
  has an `outcome` field nothing branches on.
- **seasonal-release: none observed since April**, though the seed
  declares ~30/yr with a real seasonal curve. Discrepancy unresolved —
  see Q5.
- **Keg deposits: zero occurrences repo-wide.** `grep -rni "deposit"
  crates/ examples/ infra/` returns nothing.
- **Excise is understated ~3.7×**: flat $3.50/bbl against a stated
  ~262,000 bbl/yr (`examples/brewery/seeds/workflows.toml:145`), where
  real TTB rates are $3.50 only for the first 60,000 bbl and $16.00
  after — ≈ $3.44M/yr real vs ≈ $917k modeled. No state excise in any
  of the ten states the accounts sit in.

## What is already excellent — do not break it

The survey's strongest finding is how good the accounting engine is:

- **Full absorption process costing.** Raw → WIP at real `avg_cost`;
  labor/utilities/depreciation overhead capitalized DR 1310 at runtime
  from rule args (`infra/dispatcher/rules/excise-accrue-on-production-produce-step-done.toml`); COGS at sale
  is a drain of the row's conserved value, never a percentage plug.
- **The excise *mechanism* is exemplary** — accrual on measured barrels
  computed from actual keg allocations
  (`crates/orchestrators/boss-dispatcher-handlers/src/handlers/packaging_allocate.rs:315-323`),
  quarterly filing derived from the GL, remittance conservation-guarded.
  Only the rate table is wrong.
- **AP is real end-to-end**: demand-driven reorder → per-vendor
  lead-time counterparties → GR/IR → human bill approval → a daily
  payment run that pays actual approved-and-due invoices.
- **Demand *timing* is genuinely sophisticated**: Poisson per day with
  ramps, weekday/weekend and month multipliers, per-account dock-day
  cadence, holiday suppression, and random shocks
  (`examples/brewery/seeds/tenant.toml:366-427,739-756`).
- **20 live conservation invariants** sweep money, raw units, FG units,
  headcount, and taxes (`infra/lint/conservation-invariants.sh`).

The pattern of every gap below: the *machinery* is trustworthy; the
*content* flowing through it is thin or constant.

## The gap map (both lenses, with code anchors)

**Production floor:**

1. Fermentation-as-8h is the headline wrongness. Fix path:
   `duration_hours: Option<f64>` on StepSpec
   (`crates/core/boss-jobs/src/registry.rs:67` area); workforce prefers
   the step's own value over the kind default
   (`crates/orchestrators/boss-sim/src/workforce.rs:277-281`); the
   executor already honors computed durations (`workforce.rs:494`);
   `DURATION_CAP_SEC` (`boss-sim/src/output.rs:1822`) must go.
2. Vessels are decorative — fermenters exist as equipment Subjects but
   no brew claims one, so capacity never constrains scheduling.
3. `batch_id` is written nowhere on the production path the floor
   actually runs, despite the ledger's batch conservation invariant R.
4. QC is authored at both ends (qc-checkpoint step, batch-qc-hold
   workflow) minus the wire between them.
5. Preventive maintenance ignores the failure-mode data the catalog
   already carries.

**Commercial + finance:**

1. **Keg economics are absent entirely** — no deposit liability
   (~$500k-$1M perpetual at this volume), no keg-float asset (~$4-9M of
   PP&E for a brewer this size), no loss/reconciliation, no return leg
   on the delivery route, no keg conservation invariant. The one
   quantity a keg brewer obsesses over is the one quantity nothing
   conserves. The infrastructure to fix it already exists (`equipment`
   SubjectKind, `boss-assets`, the invariant harness).
2. **Order size is a compile-time constant** — the sim posts bare Jobs
   and `create_job_with_steps`
   (`crates/orchestrators/boss-sim/src/shape_driven/engine.rs:297-410`)
   has no per-job metadata overlay, so all the seasonal sophistication
   modulates only *count*, never magnitude. `tier` and `account_type`
   are assigned round-robin and consumed by nothing.
3. **Every sale posts to A/R and ages 30 days with 3% bad debt** —
   `invoice_issued` unconditionally debits 1100
   (`crates/modules/boss-ledger/src/rules.rs:1311`) and the `ar-aging`
   counterparty has no payload filter (`tenant.toml:816-822`), so
   walk-in taproom pours and card-prepaid e-commerce orders become
   receivables and 3% get written off. `payment_method` already exists
   in `boss-commerce/src/types.rs:208`, unused.
4. **Excise rate table** as measured above — a rate-table change on a
   correct mechanism, the cheapest large correction available.
5. **Taproom shift close and distribution contracts are revenue
   stamps** — two steps each, hard-coded $15,554 and $6,100, no till
   reconciliation, no contract terms, ignoring the 36 seeded taproom
   staff, the taproom Location, and the `commerce.service_agreement`
   entity that already ships.

Smaller but real: A/R aging is a single Bernoulli (97% paid / 3%
written off — no late payers, ever); opening FG is valued raw-only,
~2.3× under runtime cost basis; `msrp_cents` contradicts invoiced
prices; the sale pipeline quotes 2× what orders bill.

## The ranked program

Ordered by leverage per unit of work, respecting what each unlocks:

1. **Keg economics.** Deposit liability account + returnable-float
   asset + kegs-out ≡ kegs-returned + kegs-lost conservation + the
   return leg. First thread is already live (`keg-return` v1, below);
   the full model is a ledger + seeds + invariant program.
2. **Honest durations.** `duration_hours` on StepSpec, styled
   fermentation at 120–336h, `DURATION_CAP_SEC` deleted — and the
   labor/wall-clock split from the design principle. This one change
   un-fakes the entire production calendar.
3. **Per-job metadata overlay in the sim engine.** One hook unlocks
   order-size distributions, account differentiation, SKU mix, and
   every downstream metric now degenerate.
4. **Cash-vs-credit branch at invoice posting.** Small fix; repairs
   A/R aging, DSO, cash flow, and bad-debt expense simultaneously.
5. **Excise rate table.** TTB graduated tiers + state excise as data.
6. **Wire QC.** Spawn `batch-qc-hold` from failed qc-checkpoints; the
   live `tasting-panel` protocol is the sensory layer of the same
   program.
7. **Backlog:** taproom shift fidelity (till count, staff, Location),
   real distribution contracts on `service_agreement`, sale win/loss
   fork, past-due→eventually-paid A/R transitions, opening FG
   valuation, msrp/quote/invoice price reconciliation.

## The live experiment

Protocols are cheap here, so two are already running rather than
waiting for this doc:

- **`tasting-panel` v1** (active 2026-08-21): brew close spawns a
  sensory panel — pour-flight (qa-tech) → panel-verdict
  (qa-supervisor, `release|hold`) → terminals released/held. Measures
  a *hold rate* the brewery has never had.
- **`keg-return` v1** (active 2026-08-21): a delivered wholesale order
  spawns the fleet-return leg — log-fleet-out (kegs, deposit) →
  receive-returns (returned/lost counts) → wash-and-restock →
  reconciled. Measures a *keg loss rate* — the first thread of program
  item 1.
- Spawn rules ride the dispatcher registry (`jobs.job.closed` filtered
  by kind). Authoring lesson recorded: rule create makes a **draft**;
  `POST /api/dispatcher/rules/{name}/publish` activates and hot-loads
  it — same contract as workflows, no restart needed.
- Under test: spawned packets must inherit `simulated` through the sim
  event chain so the workforce drives them and the real board stays
  clean. First spawns will confirm or refute.

## Decision history

All five questions were resolved by David in the design review, 2026-08-22
(packet `4a39c1df`).

- **Q1: How deep does keg economics v1 go?** — Full balance-sheet model: deposit liability, returnable-container float asset, keg conservation invariant, and the return leg. David: "Let's go for the full balance-sheet model."

- **Q2: Do steps carry labor-hours and wall-clock-hours as two fields?** — Split them. StepSpec grows both `labor_hours` and `wall_clock_hours`; `duration_hours` (landed 2026-08-22) becomes the wall-clock leg's precursor. David: "We can split those two times."

- **Q3: Does the 8h/person/day labor budget become a live invariant?** — No sweep check. Actor realism is a configuration expectation, not an enforced invariant: "It does not have to be an invariant, but I wouldn't expect a realistic configuration to include people working abnormal hours." Review protocol configs against it; don't wire it into the conservation sweep.

- **Q4: Excise rate table - registry data or rule args?** — Registry data. Graduated TTB tiers and state rates land as versionable registry rows the accrual handler reads.

- **Q5: Where did seasonal-release go?** — Unresolved cause, resolved direction: "I am not sure, but we can bring it back." Investigate the sampler, then revive the flow.
