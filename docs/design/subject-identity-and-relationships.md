# Subject identity & relationships — the identity and edge contracts

**Status**: living contract (the workstream that established it —
R1–R4 + Q1–Q7, approved 2026-07-15 — completed 2026-07-29; decision
history folded into
[docs/architecture-decisions.md](../architecture-decisions.md)
§Primitives; the audit's residual defect worklist moved to
TODO.md). Every new subject kind and every new cross-kind reference
follows this contract.

## The two invariants

> **Identity has a home.** A Subject exists iff it has a row in
> `subjects (kind, id)`. Identity-first: the row can exist from the
> id alone, before anything else about the subject is known.
>
> **References are declared and enforced.** Every subject-valued
> reference an event carries is a declared `subject_edges` row,
> resolved against `subjects` inside the domain transaction — a
> write referencing a missing subject aborts before it becomes
> state.

Together they close the phantom-reference class behind the
2026-07-13 incident from the referent side, the way the
transactional outbox
([transactional-audit-log.md](transactional-audit-log.md)) closes
it from the provenance side.

## The `subjects` identity table

One thin row per subject: `(kind, id, label, created_at,
retired_at)`, PK `(kind, id)`, kind FK'd to the SubjectKind
registry. Identity only — attributes stay in domain tables and KB
views; this is the minimal durable fact "this subject exists."

**The dual contract (Q1: write-through AND projection).** Domain
services upsert the identity row in the same transaction as their
domain row (`record_subject_in_tx`), AND `rebuild_subjects`
reproduces every row from the log + reference tables — the
`financial_facts` shape; the deep replay-check owns its
correctness. The rebuild has four source families:

1. **TOML identity sources**
   (`boss-subject-kinds/seeds/subject_identity_sources.toml`) — one
   row per identity-bearing event kind (`accounts.account.created`,
   `asset.registered` + `asset.received`, `messages.message.sent`,
   …). Extending coverage is a row append.
2. **The jobs pass** — every `jobs.job.created` subject pair, read
   from the NESTED payload (`payload.subject.subject_kind` /
   `payload.subject.id` — top-level keys never existed; reading
   them matched zero rows and silently dropped the birth-by-job
   kinds, the #140 lesson). Identity-first made literal: a Job
   about a subject proves the subject existed.
3. **Reference tables** for seed-only kinds with no create events
   by design: `locations`, `companies`.
4. **Birth-by-job kinds** (`workflow`, `custom` — registry rows
   with `metadata.birth = "job"`): their identity row is minted in
   the job-create transaction itself; the jobs pass reproduces it.

**The landmine rule.** A kind minted only by a live write-through —
no TOML source, no reference table, no jobs-pass coverage —
VANISHES at the next epoch rollover's truncate-and-reproject. This
class has bitten three times (company, birth-by-job kinds, the
170-asset fleet). A new subject kind lands WITH its rebuild source,
or not at all. Corollary: a projection pass that matches zero rows
raises nothing — parity between domain rows and reproduced subjects
is the check.

**The existence gate** is one indexed lookup against `subjects`,
uniform across every kind including tenant-defined ones, mounted in
the jobs create path (abort-by-default posture: NotFound → 400,
upstream-unavailable → fail closed 503).

## The `subject_edges` relationship registry

A declared edge says: events of `source_kind` carry a subject
reference at `field_path`, and it must resolve in `subjects`.

```sql
subject_edges (
    source_kind      TEXT,  -- event kind
    field_path       TEXT,  -- dotted payload path to the ref id
    target_kind      TEXT,  -- pinned kind …
    target_kind_path TEXT,  -- … XOR dotted path to a payload kind
    on_missing       TEXT   -- 'abort' (everything) | 'warn' (nothing today)
)
```

- **Dotted paths** (`#>>` resolution): `account_id`, `subject.id`,
  `kind.holder_id` are all one mechanism.
- **Dynamic target kinds** (`target_kind_path`) are the typed-pair
  shape: the event names its own target — a Job's
  `subject.subject_kind`, a custody event's `kind.holder_kind`
  ("the brewery installs at locations; the device shop ships to
  accounts" — one rule). A kind-mismatched pair aborts; an id whose
  kind half is absent skips, like an absent ref (identity-first).
- **Enforced where it can abort**: `check_subject_edges()` runs
  BEFORE INSERT on `event_outbox` (inside the domain transaction —
  the write fails before it becomes state) and on `audit_log`
  (belt-and-braces). The bundle-import escape hatch
  (`audit_log.ref_check = 'off'`) covers restore sessions.
- **Swept where it can drift**: conservation invariant Y walks
  every declared edge against the whole log nightly.
- **Q2 posture**: every shipped edge is `abort`. No prod data; a
  loud abort is the point.
- Each module seeds its own edge rows alongside the tables it
  targets, so modules stay independently removable.
  `audit_log_ref_checks` survives only for non-subject residuals
  (raw-material `part_sku` → `inventory_items`).

**Deploy-order rule for new abort edges on a live system:**
backfill `subjects` first (additive INSERT from the log — no
TRUNCATE window on a running demo), then apply the edge. An edge
declared over missing identity rows is an abort storm.

## Custody, company, ownership (the settled shapes)

- **Asset custody is subject-valued** (Q5): the typed
  `(holder_kind, holder_id)` pair on Shipped/Installed, validated
  by a dynamic-kind edge — never an overloaded account-id column.
- **Org-level work is about the company** (Q6): one `company`
  subject per tenant (reproduced from the `companies` reference
  table); payroll, tax filings, AP runs, facility overhead all open
  Jobs about it. The organization being modeled is itself a Subject
  in its own event log.
- **Jobs are owned by humans** (Q7): `jobs.owner` always resolves
  to a person (the owner-resolution module's deterministic
  role-holder spread); steps may be automation-executed. There is
  no automation-identity registry.

## Id minting (R3)

One convention per kind, one minting path: domain services via
their create write-through, table-less kinds via
`POST /api/subjects/{kind}` (idempotent upsert). Seeds, the sim,
and tenant engines route through the same paths — a convention fork
per writer is how phantom references get manufactured.
