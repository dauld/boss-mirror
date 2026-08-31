# The BOSS correctness protocol

**Status**: load-bearing thesis. Treat this as the design north
star for every Workflow, every projection, every adapter.

## The claim

Every company already runs on a model of the world encoded in
its books, its CRM, its CMMS, its HR system, its inventory
records. Most of those models are silently inaccurate — the
books drift from reality, the CRM lies about pipeline, the
inventory thinks it has 200 kegs when there are 187. Companies
spend enormous amounts of human time reconciling these models
against each other and against reality, and they still close
the year with material adjustments.

BOSS's bet is to make that model **provably accurate**. The
system contributes zero error of its own. Every observed
inaccuracy is provably attributable to a wrong *input event*,
never to the projection pipeline introducing drift. Corrections
flow as compensating events, not edits to the projection.

That gives operators a single-question diagnostic — "which
event is wrong?" — instead of the open-ended "where did the
books drift?" that companies without this guarantee live with
every quarter.

## The five properties

Every Workflow, projection, and adapter must satisfy all five.
A Workflow that can't answer all five is incomplete; ship it
that way only as a known stop-gap.

### 1. Provenance
Every recorded fact traces to a real event. Concretely: every
`financial_facts` row carries a non-null `source_table` +
`source_id` that resolves to a real `steps` row (or to the
single allowed `opening` fact, the day-zero bootstrap). State
tables like `inventory_items` inherit the same chain — every
`on_hand` move is paired in one transaction with the
`financial_facts` row that records it (a raw-stock consume
writes the matching COGS fact in the same `consume_part_at` tx).
No orphan rows, no manual `INSERT` stand-ins.

### 2. Conservation
For every conserved quantity, `in − out = stock` to the unit.
Money via double-entry (debits = credits). Inventory units via
purchase facts vs drawdown facts. Headcount via onboard vs
offboard. Taxes payable via accrual vs remittance. The books
already enforce conservation for money; BOSS extends the same
discipline to every conserved quantity in the model.

### 3. Closure
Every Job that opens reaches a terminal state. Stranded
mid-flight Jobs (owner left, subject archived, blocked-by chain
broken) are loose ends, and a periodic sweep surfaces them as
Bulletins on Operations.

### 4. Idempotence
Replaying the audit log into a fresh DB lands at the live state
byte-for-byte. Divergence between live ledger and replay = bug,
period, not "drift to be reconciled monthly". This is what
makes #1 and #2 verifiable to a third-party auditor.

### 5. Determinism
Same inputs produce same outputs across runs. No
wall-clock-derived random numbers in side effects, no
nondeterministic ordering of within-step writes.

## Architectural consequences

These follow from the protocol; they're not separate choices.

- **No projection-table writes outside the projection
  pipeline.** Every `gl_journal_entries` / `invoices` /
  `inventory_items` row exists because a `step.done` fired
  in the same transaction. The projection pipeline is the only
  caller. Seeds may not bypass this — see
  [seed-vs-emergent-state.md](seed-vs-emergent-state.md).
- **Side effects commit transactionally with the step row.**
  No "step said done but the JE didn't post" gap.
- **Corrections are events, not edits.** A miscounted
  inventory becomes a `correction` event; a misposted JE
  becomes a reversing JE; a wrong invoice gets a credit memo.
- **Append-only event log + hash chain** (live: the `prev_hash` /
  `row_hash` insert trigger in `schema/02-events.sql` + `verify_chain`
  in `boss-events/src/integrity.rs`) is what makes the audit-log replay
  externally verifiable. An auditor independently replays the
  log and checks the projection.

## The dual benefit — efficiency falls out of the discipline

Structural discipline at write-time also unlocks computational
shortcuts at read-time that conventional CRUD stacks can't
access. From the same single append-only log:

- **No reconciliation layer.** There's nothing to reconcile
  against. Monthly close and audit prep collapse into "did the
  log replay?"
- **Time-travel is free.** "What did the GL look like on
  2025-08-31?" is the projection folded over events where
  `happened_on <= '2025-08-31'`. No as-of tables, no quarterly
  archives, no temporal-table gymnastics.
- **Incremental aggregates.** Projections are folds, so live
  counters are O(new_events) per tick, never O(full table).
  The Finance overview that takes a SaaS finance tool minutes
  is a streaming counter here.
- **Cheap parallel projections.** The same log feeds GL,
  inventory ledger, cap table, HR trail, tax position with no
  write contention. New projection = scan once, subscribe to
  the tail.
- **Diff is trivial.** "What changed since yesterday" = the
  events in that window. No table-vs-table hash diff.
- **Operator queries conventional stacks can't ask.** "Every
  Job in flight last Wednesday at 3pm." "Open requisitions when
  this candidate was hired." "12-month windows with the most
  inventory churn." All one-line folds over the log.
- **Pushdown to columnar storage.** Append-only event streams
  are the access pattern Parquet / DuckDB / ClickHouse are
  built for. Multi-year company-history analytics on a laptop.

Provable accuracy *plus* faster computation than systems that
don't even attempt accuracy. The two compound: BOSS does what
conventional stacks can't, and does the things they do better.

## Enforcement

Five mechanisms turn the protocol into something the system
checks for itself, not something authors have to remember.

1. **Static lint on Workflow specs.** `validate_workflow` (in
   `boss-jobs/src/workflow_lint.rs`) proves a Workflow is a
   well-formed program before it can run. Phase 0 checks that
   every value in a step's `metadata_defaults` matches its
   StepType field's declared type. Phases 1–3 check the
   predicate graph for viability: at least one trigger
   (`ready_when = "true"`) and one terminal, every
   `steps.<slug>` reference resolves, the graph is acyclic,
   every step is reachable, and every fork point covers all
   values of its discriminating enum. Runs at author time
   (`/system/workflows`), at publish, and at boot
   (boss-jobs-api refuses to start against a broken registry).
   The runtime metadata validator in `step_registry.rs` is the
   complement: it enforces a StepType's `required` fields when
   a step flips to `done`, not at author time.

2. **Runtime dispatch at step.done.** The core `boss-dispatcher`
   service binds a durable JetStream consumer and matches each
   step-completion event against its rule registry, keyed by
   `step.done.<kind>` topics (`step.done.procurement`,
   `step.done.billing`, `step.done.shipment`, ...). A matched rule
   fires its registered side-effect handlers (each a pure HTTP
   client to the relevant module API — commerce / inventory /
   ledger / products / shipping / people) against the live event
   payload. Side effects are therefore driven off the event log,
   deterministically: same log → same matches → same handler calls.
   Each event is ACK'd only after its handlers succeed; a handler
   failure NAKs the message for redelivery, so a transient downstream
   hiccup self-heals instead of silently dropping a side effect.

3. **Conservation-invariant sweep.** `infra/lint/conservation-invariants.sh`
   ships with a paired `boss-conservation-invariants.timer`
   systemd unit (hourly). 20 SQL invariants — trial balance per
   JE, inventory non-negative, closed jobs have closed_on, paid
   (customer and vendor) invoices have paid_on, financial_facts
   provenance resolves, each GL control account (cash,
   raw-inventory 1300, finished-goods 1320) non-negative and
   tied back to physical counts, accruals ≥ remittances for
   sales tax / payroll / deferred revenue, period close reflects
   net P&L, and more. Each query selects only the rows that
   *violate* its invariant; any non-empty result exits the
   script non-zero. The journal is the sink today (a future
   hook lands violations as Operations Bulletins).

4. **Tenant seed + lifecycle tests.** The brewery crate's
   `tests/protocol_holds_e2e.rs` runs the layer-1 static lint
   over every Workflow in the tenant's seed files — a broken
   Workflow fails `cargo test`. The used-device-shop
   crate's `tests/parity_harness.rs` and `shape_driven_smoke.rs`
   drive the tenant through a 30-day window and assert coverage
   (every authored Workflow fires) and lifecycle (jobs open,
   steps complete, jobs close, the canonical side-effect topics
   fire). The sim does not materialize or drive steps in-process,
   so step volumes, metadata shapes, and side-effect wiring are
   validated end-to-end by the live 365-day regen.

5. **Replay-diff audit.** Wipe projections, replay audit_log
   into a fresh DB via `boss-rebuild-all`, diff against live.
   Any row-level difference is a system bug. The shipped hash chain
   (`verify_chain`) is what turns this from an internal CI step into a
   check an external auditor can run as their first command.

The static lint + the bypass-smell lint described in
[seed-vs-emergent-state.md](seed-vs-emergent-state.md) are the
cheapest first wins; they catch the largest class of mistakes
before they reach the runtime.

## Related docs

- [seed-vs-emergent-state.md](seed-vs-emergent-state.md) — the
  seed-side guidance that falls out of this protocol.
- `crates/modules/boss-ledger/` — the GL projection this
  protocol is most-formalized for; `financial_facts` is the
  ledger's input.
- [human-powered-state-machine.md](human-powered-state-machine.md)
  — the framing this all sits inside.
