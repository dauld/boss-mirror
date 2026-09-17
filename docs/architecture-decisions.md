# BOSS — Baseline Architecture Decisions

This is the **consolidated decision record** for BOSS: one thematic
walk through every load-bearing choice in the running system,
written as current truth. It absorbs the v0.1 pre-release record
(~180 decisions), the v1.1 ADR catalog (the step-UX plugin model,
the dispatcher-as-event-router and Workflow-v2 decision sets, step
types as property bundles, the Intangible subject root), and the
design documents whose work has shipped. There is no separate
history to cross-reference: what this document says is what the
code does.

**How decisions evolve.** A design doc **is a packet**, not a file.
Its prose and its open questions ride on a `design-doc` Job, so it is
reviewable the moment it exists rather than after it ships, and the
answers are recorded on the packet — which is the record. A revision
is a **new packet carrying `translated_from`**, never a mutation, so a
doc's life is a chain and the chain is its decision history, with an
actor and a timestamp on every answer. **This document is the fold**:
current truth per topic, assembled from those packets, with the
history beside it rather than inlined. It stays hand-maintained
because merging prose has nuance a generator cannot judge — but the
`fold` step of the `design-doc` workflow makes updating it an
obligation the protocol enforces rather than an intention somebody
holds. Files under `docs/design/` are the legacy corpus, being
translated into packets; a generated tree at release keeps `git grep`
and the OSS install honest, and because it is generated nobody
hand-edits it and it cannot drift.

---

## Thesis & positioning

BOSS is a **technical proof of a simple thesis**: model the
operating system of a company directly as a state machine, and the
abstraction layers traditional ERPs/workflow platforms accumulated
fall out as scaffolding around a missing primitive rather than as
load-bearing structure. The codebase is small on purpose — small
enough that a human reviewer can audit the entire production
output in a sitting, which makes it a substrate that pairs well
with modern AI authoring tools. The running system stays plain
Rust + Postgres + SPA with no model in the request path; AI
mediates at authoring time, not at runtime. The forward direction
is **modeling UX and experimentation**, not new domain modules.
The correctness goal is **TLA+ provability** — every state-machine
transition, projection, and invariant small and clean enough to
specify formally if pushed (`docs/formal/` carries the first two
specs: Step lifecycle, ledger period locking).

Three intellectual lineages anchor the design (CLAUDE.md
§Founding ideas): **Stafford Beer** (a company is a viable system
describable in feedback loops; the dispatcher is the feedback
layer; *algedonic* signals are rules firing on threshold events),
**Rich Hickey** (information is simple; data is primary; the audit
log is the system of record and projections are pure functions of
it), and **George Orwell** (language anchored to reality; the log
holds what *did* happen so the words operators use stay anchored
to facts — and the repo's own vocabulary is held to the same bar:
one word per concept, enforced by rename passes and lints).

**The public repo carries no inherited git history** — every
release is a fresh rooted commit cut from the working tree. The
working tree is the canonical record; docs are self-contained; no
"see commit X" references survive into the public repo. Public
demo tenants are **Algedonic Ales** (the brewery) and the
**used-device-shop**, both instantiations of company-management on
the same state-machine abstraction.

## Primitives & information architecture

Four primitives model everything: **Subjects** (identity-bearing
things work is about), **Jobs** (bounded units of coordinated
work), **Steps** (typed transitions inside a Job), **Events** (the
immutable record — the system of record). Three supporting
concepts hang off them: the **Class registry** (every taxonomy as
data), **StepPlugins** (step UX as data), and **Policy**
(row-level privilege rules).

**Subject is a trait, not an enum.** Each kind implements it with
its own KB view; the wire shape is a flattened
`{ subject_kind, id }` pair (the old per-kind tagged-enum payload
keys are gone). `subject_kind` is an open string validated against
the SubjectKind registry; the platform ships its kinds as registry
rows, and tenants add kinds without touching core. The expression
language reads `subject.kind` / `subject.id` — that DSL surface is
stable independent of wire serde.

**Five roots** seed the SubjectKind taxonomy: the four noun axes
**Person** (`boss-people`), **Place** (`boss-locations`), **Thing**
(`boss-assets` for tracked units, `boss-catalog` for the model
registry), and **Intangible** (identity-bearing things with no
physical embodiment — agreements, campaigns, workflow documents
like purchase orders; the home for future contract/SLA/lease
kinds), plus **Calendar**, the time-coordination primitive. A
`NULL parent_kind` on a platform row means "TBD", never "special".
The `custom` kind stays deliberately outside the taxonomy as the
escape hatch. The tracked physical unit is an **asset** at every
layer — crate, routes, `asset.*` event kinds, tables, types,
subject kind — and the word "system" means exactly one thing in
this repo: the organization being modeled.

**Subject creation is identity-first.** A Subject can exist from its
stable id alone, before everything about it is known, and accrete
data incrementally — the Subject-level form of the Step rule
"required-at-done, not required-at-create." The asset is the worked
example: an asset is born by a `Registered` event carrying only its
id (`phase = registered`, no `sku`), and its catalog model, custody
(`Received`), and location arrive later as enrichment events — sku is
nullable, identity is not. A registered-but-unidentified asset
honestly has no model-derived attributes (no depreciation basis, no
Equipment-KB model view) until an `Identified` event sets the model.
The general principle: the only hard constraint on creating a Subject
is its identity; any further "required at create" constraint is data,
not a baked-in NOT NULL or event field.

**Identity has a home: the `subjects` table** (R1, approved
2026-07-15; live contract:
`docs/design/subject-identity-and-relationships.md`). One thin
`(kind, id, label)` row per subject — identity only, no attributes —
that every domain mint upserts in the same transaction as its domain
row AND the rebuilder reproduces from `*.created` events (the
dual write-through/projection contract, Q1; the deep replay-check
owns its correctness). The uniform existence gate is one indexed
lookup against it, for every kind including tenant-defined ones.
Kinds with no rebuild source are **rollover landmines**: any subject
minted only at prepare time vanishes when the epoch rollover
reprojects — the class bit three times (company → the `companies`
reference-table pass, birth-by-job kinds → the nested-payload jobs
pass, assets → the TOML identity sources), so a new kind lands with
its rebuild source or not at all.

**Relationships are one registry: `subject_edges`** (R2, shipped in
two passes 2026-07-17/29). A declared edge — `source_kind` event,
dotted `field_path`, target kind either pinned (`target_kind`) or
read from the payload (`target_kind_path`, the typed-pair shape:
job.subject's own subject_kind, the asset custody holder_kind) — is
enforced by the `check_subject_edges()` trigger on `event_outbox` +
`audit_log`, aborting inside the domain transaction (Q2:
abort-everywhere, no warn-mode legacy), and swept nightly by
conservation invariant Y. It superseded the three partial registries
(audit_log_ref_checks survives only for non-subject residuals like
part_sku). Absent refs skip — identity-first — and an id whose
dynamic kind half is absent skips with it; a kind-mismatched pair
aborts. Custody is subject-valued: the asset holder is a typed
(holder_kind, holder_id) pair (Q5), not an account id. Org-level
work is about the **company** Subject (Q6) — one row per tenant,
reproduced from the `companies` reference table. Jobs are owned by
humans, never automations (Q7) — steps may be automation-executed,
but the Job's owner resolves to a person (the owner-resolution
module's role-holder spread). Deploy-order rule for new abort
edges on a live system: backfill `subjects` first, then apply the
edge.

A **Class** is not a Subject: Classes are typed reference data
keyed `(subject_kind, code)` that each Subject kind owns — roles,
account types, asset models, departments all land in the one
`classes` table (living reference:
`docs/design/class-registry.md`). Parts are Subjects in their own
right; the Composite primitive is heterogeneous and laws-checked
via proptest at the trait boundary.

The system is laid out on a **three-axis information
architecture**: *Knowledge Bases* (durable queryable state),
*Surfaces* (operator UI), and *Work* (Jobs + Steps that change
state). Every KB-exposing domain implements the shared `KB` trait
from `boss-core`; facts live in domain tables, not a global facts
table; aggregations rebuild on-demand + periodically.

## Jobs, Workflows, Steps

A **Job** is a bounded unit of coordinated work: stable identity,
owner, subject, status, and a structured list of Steps. The
**Workflow registry** is append-only and versioned; in-flight Jobs
pin to the version they opened under; creation is blocked against
`draft` and `retired` kinds. Adding a new workflow means adding a
Workflow row — never a `match` branch in core code.

**The DAG is implicit in predicates.** Each step declares
`ready_when` — a pure expression over
`(subject, job, prior step states)` — and an edge A → B exists iff
B's predicate references A. `blocked_by` is a derived,
denormalized edge list for rendering, recovered from the
predicates. Predicates are **pure over immutable inputs**:
external state (clock, inventory, balances) is out of bounds —
reactions to external state belong in dispatcher rules, so replay
is deterministic across evaluator versions. Materialization is
**eager with status**: every step exists from Job creation
(Pending → Ready → Active → Completed, plus Skipped), the
re-evaluator is the readiness authority, and structural transition
events emit on every status change. **No loops at the workflow
layer** — iteration lives inside a step or in a sub-Job.
**Terminals are explicit** (a `terminal` flag on the step spec;
multiple per kind), and the **viability lint** proves structural
invariants, reachability, and fork coverage at publish time; fork
coverage over open-ended fields requires a wildcard fallback. A
predicate dependency index is built at publish; runtime
re-evaluation is incremental. The predicate DSL is a tiny custom
language shared verbatim with the dispatcher's `when` clauses and
handler args.

**Sub-Jobs are a typed contract** (`delegate-subjob` — the one
spelling): the parent step's completion *is* the child Job's
close, parents must handle every possible child outcome, and the
dispatcher performs the close → resolve write-back. Required
metadata is checked **at done, not at create**;
`PUT /api/jobs/{id}/steps/{step_id}` has PATCH semantics
(top-level fields replace wholesale; clients merge metadata keys).
The Jobs list takes exactly one subject filter — `?subject_id=` —
and the Job's subject column is `subject_id`.

The brewery's `wholesale-keg-order` is the worked example of
agent-gated fulfillment: an `availability-gate` reads finished-goods
for the order's lines and forks fulfill|backorder — an order the cooler
can't cover exits to a terminal `backordered` outcome instead of
marching through pick → ship → bill against stock that isn't there —
then a human `pull-and-stage` pick precedes delivery and billing. The
gate is the release valve that bounds open WIP the way human
stock-judgment does in a real brewery; finished-goods / COGS draw on the
billing line items.

**Workflows bootstrap through Jobs.** The system-owned
`workflow-design` kind authors new Workflows inside a Job (draft
edits live in the authoring Job; the terminal `workflow-publish`
step writes the registry row), so the platform's own catalog is
published with full audit provenance — the system models its own
development. Platform kinds ship in code (`platform_kinds()`);
tenant kinds load from `examples/<tenant>/seeds/workflows.toml`
(governance rule: `docs/design/platform-vs-tenant-jobkinds.md`).

**Authoring is graphical and author-gated.** The `workflow-design`
surface is an interactive trigger→outcome canvas (Svelte Flow + dagre,
code-split onto the editor route): steps are nodes (trigger / terminal
/ fork / work), an edge A→B *is* `steps.A.done` in B's `ready_when`, and
a structured predicate builder emits the boss-expr behind a
live-validated raw "advanced" escape hatch. A non-persisting dry-run
(`POST /api/workflows/_validate`) runs the publish-path lint against
the same in-process `StepRegistry::v1()`, so editor-green publishes by
construction; the SPA persists drafts as `metadata.workflow_spec`
PATCHes on the design Job and never calls the direct `/api/workflows`
create/update/publish handlers (kept only for bootstrap + tests). The
design **approve** step requires a `workflow-approver` capability —
authoring a work-type is operational leadership's call, not the deploy
operator's alone (core policy grants it to `platform-admin`; tenants
grant it to their leaders; `design-doc-review` stays `platform-admin`).

## The network substrate — packets, stations, routes

The reading frame is `docs/design/the-three-layers.md`: *"The network
is the substrate, the fat protocols dictate the current operating
model, the actors run it."* That doc is the one place the statement
is made; this section is what it resolves to in the running system.

**A Job is a packet.** The envelope is the Job row — identity,
subject, headers, protocol set; the payload is the accretion of
writes the log holds for that job_id. The projection rows mutate;
the log accretes. Two columns were asked to justify themselves and
both survived. **`owner_id` stays** as the accountable human of
record but stops pretending to be routing: stage 1 is that demotion,
stage 2 re-keys the `Self_`/`Team` policy scopes onto queue-derived
ownership and leaves `owner_id` a derived accountability lens ("who
owns the queue this sits in" plus "who has written to this packet").
The 2026-07-15 rule that a Job with no resolvable human owner is
refused is **relocated, not repealed** — the protocol names an
accountable requirement owner. **`Job.status` stays** as a
materialized cache of what `compute_job_status(steps)` derives,
recomputed on every step write; the manual status PUT dies except
for `released` and `cancelled`, the two imperative states that
resist derivation and become explicit packet writes.

**The protocol set is fixed at creation.** A packet declares a set
of compatible protocols composed exactly once, at admission —
requirements conjoin, obligations union, the viability lint runs
over the composed set — and the envelope never mutates after;
layering was rejected for v1. A packet needing different governance
mid-life is **translated**: a new packet under the new set, admitted
through the same edge, carrying a `translated_from` edge back and
leaving a `translated` terminal on the source. Translation is also
how a packet crosses fabrics (instances) — one mechanism, not two.

**Headers are declared data; undeclared metadata stays payload.**
`job_edges` is the shipped half: which metadata field of which Job
kind references another Job, enforced on the write path with
`subject_edges`' `on_missing` dial and prefix-aware resolution for
the folklore it inherited — seeded `warn` while the values were
dirty, turned to `abort` once the three real edges (`backlog_item`,
`train`, `boarded_jobs`) were cleaned and the machine writers
audited; `spec`, `branch` and `merge_ref` point outside the Job
graph and are deliberately out of scope. It generalizes into a
**header registry** carrying name, value shape, edge-ness
(resolution + `on_missing`), and which protocol reads the header;
`authority_role`'s triple duty gets named and split as it lands.

**Stations are the network's nodes, and everything about one is
registry data** (living reference: `docs/design/stations.md`). A
station is an abstract priority queue that routes or holds packet
traffic until there is bandwidth or capability to handle it —
**queuing separated from dispatching**: a station holds and orders,
the router moves. The registry lives in `boss-jobs` beside the
workflow registry, same append-only versioned posture, one active
row per name. Four kinds share one row shape: **actor** (every
executor has one), **group** (departments and teams), **constraint**
(membership by capability predicate, not an enumerated roster), and
**batch** (the bundling points — loading dock, review queue, board
windows — where packets accumulate for periodic, higher-bandwidth
handling). **Membership is derived and motion is evented**: the row
carries a predicate evaluated over open packets at read time, so no
mutable current-station field exists to drift from `steps`, and the
router emits arrival/departure markers so the map and flow metrics
read motion without one. Per-actor stations need no per-actor rows —
the predicate carries a literal `"@me"` that the evaluator binds to
the requesting actor once, before any packet is compared, and both
failure modes fail closed (an unbindable placeholder answers with an
empty queue; an unbound one matches nothing, so it can never hand
one packet to everybody). **Ordering is data**: a `discipline` array
on the row, default `priority, then age`, ties broken on job id, and
shown in the lens header so an operator never wonders why a queue is
in this order. **Capability gates at the claim** — checked against
the station the claim *names*, before the compare-and-set decides
anything — and **`wip_limit` is advisory first**: a lens warning and
telemetry, enforcing later only if the data says it matters.
`terminal_window_days` sits on the row rather than inside the
predicate because it is retention, not membership (and keeps
predicate evaluation clockless): a watchlist read by the person who
filed the packet is empty at exactly the moment it matters if
departed packets vanish at closure. Stations ship read-only and
barely seeded — two platform `batch` rows, no authoring API — so
"every executor has one" is the design, not today's data.

**Priority becomes Class-registry data.** The `CHECK` constraint,
the closed Rust enum and the TS union retire together in favour of
Classes of `job`-kind Subjects — §Registries-over-code one level
down — and only then can a station's discipline reference priority
meaningfully. Escalation stays a hop between stations, never a
discipline.

**A protocol gets a page.** Each workflow kind grows the network
vocabulary — name, version, purpose, demands, and usage read from
the `jobs_kind_version` index — so a protocol is presented as a
protocol rather than only as a DAG, and the canvas's route ghost
becomes a per-protocol tint. The system diagram redraws on the same
vocabulary and becomes the one diagram the README and the canvas
legend both cite (resolved 2026-08-12; the redraw itself is not yet
executed). The envelope model and the target shape of the parts not
yet built are carried by `docs/design/job-packet-network.md`.

**A step declares its audience once, and every surface derives its
selector from that** (design `f5ebd2e1`, David 2026-09-11; three
questions accepted as proposed, the fourth a constraint). The
measurement: a backlog item routed to `design` produced a decision step
at slug `design-review` that no station predicate claimed — invisible
on `/it/design` by construction — while My Day's endpoint listed it
among eleven rows for `assignee_id=emp-david&roles=platform-admin`, and
the operator found neither. Read from the code, placement is three
mechanisms keyed on three different things: an individual on
`step.assignee_id`, a role on `metadata.authority_role` (with the
unclaimed-or-active rider), a queue on a `StationPredicate` over
`job.kind` + `step.slug`/`step.kind`; My Day unions the first two, a
station page reads only the third, and nothing asserts that a ready
step is claimed by any of them. Decided: (1) **one required audience
field** on a step's declaration — a closed set of shapes: an
individual, a role, a department, a named station — from which all
three selectors derive, because a closed set makes an audience nobody
reads *impossible* rather than unlikely; (2) **department is Class
registry data** on workflows and/or stations, never a match in core
code (CLAUDE.md §9), which also answers "every human-only step in the
department regardless of assignee" without a bespoke query; (3) **no
orphan steps**: a check, computable from registry data alone, that
every ready step in an open packet is selected by at least one station
predicate or carries an assignee — the mechanism that would have caught
this the day it was introduced; whether it warns or refuses at publish
is open until the station set is complete. The fourth answer is a
constraint on any build, not a choice between them: **surfaces never
show inconsistent representations of the same step.** Not yet built;
until it lands, a decision that must reach a person is assigned to
them by id, which is the one selector every surface honours today
(2026-09-15: six operator decisions assigned that way, each with the
ask written on the step as `context_md`).

## Step types are property bundles; the alphabet is the mechanisms

A step *type* enforces rules, and each rule is an orthogonal,
data-expressible property. **What stays code is the closed
mechanism set**: the completion authorities, the validator engine,
the lint protocols, the expression DSL, the surface host, and the
side-effect handler verbs. Every named StepType is a **property
bundle over those mechanisms** — an append-only, versioned,
tenant-authorable registry row carrying:

- **Completion contract** — a `fields` schema (required-at-done +
  per-type value checks). Steps may also author `fields` inline in
  the Workflow; validation is the union, so single-use vocabulary
  needs no registry row at all.
- **Completion authority** — one enum: `human` (an operator
  holding `authority_role`; default), `agent` (a computed
  decision; with an `outcome` enum field it is a gate resolved by
  the dispatcher's gate handler), `child-job` (the delegate
  contract), `external` (a bound counterparty event completes the
  step — **binding**: the jobs API rejects manual completion; the
  policy-gated operator override is its own audited action; the
  source is named by dispatcher rule), and `auto-on-materialize`
  (the `trigger` special case — a trigger describes job-creation
  conditions and has no completion logic of its own, so it is
  resolved at materialization: the firing trigger is born
  `Completed`, its alternatives `Skipped`. Which one fired is read
  from the Job's `metadata.trigger_name` — stamped by the
  `jobs.spawn` rule that opened the Job — so a Job authored with
  several triggers records only the one that actually fired, never
  all of them. Downstream steps fan in with `steps.a.done OR
  steps.b.done`).
- **Sign-off requirements** — see below.
- **Render surface** — a surface id into the surface table
  (platform-shipped components and tenant StepPlugins are the two
  suppliers); plus layout (`ux`), category, and a duration model
  (typical hours + jitter) the simulator reads.

**No core code may match on a step-kind name** — enforced from day
one by `infra/lint/no-step-kind-match.sh` (ratchet allow-list:
exactly the two platform-pinned rows, `workflow-publish` and
`review-design`). The registry ships 43 bundles; identical-property
bundles merge on sight (`approval` folded into `sign-off`,
`generic` into `task`, `sub-job` into `delegate-subjob`), and row
count is editorial — rows are cheap shared vocabulary, code seats
are what the lint forbids.

**Sign-off is a completion property, not a kind.** A sign-off is
the stamping of a step, *in its current shape*, by an
authenticated authority — policy-enforced — so steps can require
that multiple authorities agree before completion. Requirements
are a role list (`sign_offs_required`, requirement-object shaped
so k-of-n can land without a wire break); stamps are
`(authority_id, role, stamped_at, shape_hash)` where `shape_hash`
binds the stamp to the title + canonically-serialized metadata it
attested. Stamping is its own act (`POST …/sign-offs`), authorized
against the role-scoped policy resource `step-signoff:<role>`,
emitting `jobs.step.signed_off`, idempotent per (role, shape).
Completion requires every required role to hold a current-shape
stamp; editing a stamped step emits `jobs.step.stamps_invalidated`
(loud) and stale stamps stay recorded as provenance. Two storage
invariants back this: **stamps are append-only at the row**
(`sign_offs || stamp`; no generic write path carries stamp
fields), and **terminal statuses are immutable at the row** (a
write merged against a stale pre-completion fetch cannot demote
Completed/Skipped) — both proven necessary by race forensics
against the live dispatcher.

The v1 step-type catalog derives from the traditional software
stack BOSS replaces (CRM/ITSM/ERP/HR/comms); the canonical source
is data (`crates/core/boss-jobs/seeds/step_types.toml`), loaded by
`StepRegistry::v1()`, with `core_v1()` as the company-free tier.

## Dispatcher — the event router

Side effects are data: **steps emit events; rules in the
dispatcher's registry watch for those emissions and invoke
handlers.** Rules are rows in the append-only versioned **`dispatcher_rules`
registry** (`on_event`, `when`, `do`, over the shared expression
DSL) — the step_plugins-style draft → active → retired lifecycle,
authored in-app at `/system/dispatcher/rules`. **`infra/dispatcher/rules/`
— one `<rule-name>.toml` per rule — is the registry's DEFINITION, and
the table is derived from it:** the dispatcher publishes every authored
rule the table lacks at boot and retires every enforced rule no file
names (`rules::seed`, 2026-09-11, backlog 41ba00cd). Adding a rule is
dropping a file in; changing one is bumping its `version`; retiring one
is deleting the file; **no migration writes rules**. Until then a rule
was declared twice — a file here and an `INSERT INTO dispatcher_rules`
in a migration, compared by a test and derived from nothing — the worst
shape of CLAUDE.md §9a, since one copy lived in the production database
where no test could reach it. The direction was decided on measurement:
nothing reconciles `dispatcher_rules` at boot (so the hazard that forced
the Workflow move did not apply), a reviewed `why` cannot live in a row,
and only the tree can supply a fresh database. Live authoring is
untouched — the seed is insert-if-absent and never walks a version back,
so `POST /api/dispatcher/rules` + publish still changes a rule with no
deploy, and the tree owns which rules exist rather than moment-to-moment
control of the rows.
The reactive wiring is visualized as a cascade — trigger event →
rule → handler → emitted event → re-triggered rule, feedback cycles
highlighted, filterable by trigger event — at `/system/dispatcher`. The
dispatcher is reactive, not a catalog of everything the system can
produce. Each rule is an
**actor**: every side effect it fires is attributed
`automation:rule:<name>`, so "why did this Job spawn?" is a query
over data. Sim and prod run the **same dispatcher binary**;
operator-initiated Jobs bypass the dispatcher (it routes
reactions, not commands). Clock interaction is an ephemeral stream
plus one-off queries. The handler vocabulary (`po.place`,
`invoice.issue`, `jobs.spawn`, `gate.resolve`, `webhook.notify`,
…) is the adapter edge — the verbs that touch the world stay code;
which verb fires when is data.

Where the mechanisms live now: step side-effects are rules keyed
`step.done.<kind>` (the old `StepType.side_effects` field and the
step-effects runner are gone); inventory auto-restock is a rule
whose open-PO predicate is the idempotency check; sim Job rates
ride `clock.tick.daily` rules; and the **CounterpartyEngine stays
in the simulator deliberately** — its probabilistic choices model
external actors, the dispatcher is deterministic, and the
`webhook.notify` handler forwards triggering events to the
engine's callback server, which replies over the public API only.
The sim/system boundary **is** the HTTP API: one set of surfaces
serves real actors, the simulator, and side-effect handlers
identically; the simulator presents as the role-matched humans it
assigns, with no exemptions anywhere in policy or validation.

**The forward direction inverts this contract: reaction becomes
admission.** The network gets one admission edge — **Protocol**
evaluates the write against the packet's pinned protocol set,
**Policy** checks the actor may perform it, **Publish** stages the
consequences in the same transaction as the write — and reaction
survives only where admission cannot see: wall-clock timers,
external ingress, and cross-protocol reactors. Two-thirds already
exist (the `PolicyClient` gate, `ready_when` evaluated inside the
write transaction, the transactional outbox); the missing third is
**protocol-declared consequences**, which live in the WorkflowSpec
as `on` blocks per step transition — notify / spawn / assign /
obligation rows with an optional `when` — versioned with the
workflow and proven resolvable at authoring time by
`workflow_lint`. `on_complete_create` is the shipped precedent. The
edge is **promoted in place**, not extracted to a service: a
`boss-admission` crate boundary inside `boss-jobs`, because a
network hop inside the write transaction buys nothing until a real
second writer exists. Consequences are **decided synchronously and
delivered asynchronously** — jobs-internal ones apply in the same
transaction (exactly-once by construction, replacing the JetStream
consumer's at-least-once), cross-domain ones become **obligation
events** that the existing handler machinery drains instead of
interpreting rules, keeping the decision in the log beside its cause
while delivery stays at-least-once behind the handlers' own
idempotency guards. The queue-placement half publishes what already
exists: admission resolves the next station, records
requirement-addressed pools as events so replay can reproduce who
could have taken the work, and emits the ready/assigned markers —
no new placement state. The census of 2026-08-12 classed 38 rules as
7 jobs-internal consequences, ~22 domain effects, and 9 external-glue
reactions that never migrate; `infra/lint/dispatcher-rules-ratchet.sh`
makes the roster **shrink-only** from that baseline, and every
surviving rule owes a sentence naming why it cannot be a protocol
consequence. Scope is **packet admission only** in v1 — other domains
keep their write paths, with no quiet annexation of other front
doors. Landing the workflow registry's own draft/publish/bootstrap
writes on the outbox is a prerequisite: under this model a protocol
edit *is* a network configuration change, and a protocol the log
cannot witness is not yet data. The build plan is
`docs/design/protocol-policy-publish.md`.

## Correctness protocol & the audit log

The five-property protocol — **provenance, conservation, closure,
idempotence, determinism** — is a first-class invariant (living
reference: `docs/design/correctness-protocol.md`). The audit log
is the system of record; projections are pure functions of it;
rebuilders reproduce truth from it
(`docs/design/projection-rebuilders.md` is the living contract);
the system contributes zero error of its own.

**Sim time is retired from the record** (David, 2026-08-22, packet
a7a4cae5 — superseding the earlier "every projection row
representing a sim-time event stamps the sim-day the engine
emitted" rule). `Event.timestamp` / `audit_log.timestamp` is
wall-clock, minted by `EventStamp` at emit, whatever mode the
deploy's clock-api runs in — one incident reads as one timeline
(the mixed-clock log had a 19:25 arrival stamped ~03:00 because
clock-routed writers stamped sim while allowlisted writers stamped
wall). The sim timeline survives where it is data, not clock:
**business dates in payloads** (`happened_on`, `issued_on`,
`completed_on`, `{day}` tokens) still read the sim-aware clock
port, and the sim's own scheduling (day cursor, warp, epoch,
`clock.day` rules) is untouched. If a record ever needs a
sim-timeline annotation again, it returns as a protocol field —
explicitly deferred.

**Events become durable atomically with the state they describe**
(the transactional-outbox decision, arc completed 2026-07-29;
living contract:
`docs/design/transactional-audit-log.md`). Writers stage events on
`event_outbox` inside the domain transaction
(`record_event_in_tx`); the single `boss-event-relay` daemon drains
them into `audit_log` + NATS in order. This replaced the original
best-effort post-commit publisher after the 2026-07-13
replay-divergence incident proved the swallowed-write class real
(260+ committed state changes with no event). The decision chain
that got here: relay as a separate binary in the deploy list from
day one; explicit `record_event_in_tx(&mut tx, …)` threading at
call sites rather than publisher magic; epoch trim TRUNCATEs the
outbox before the audit DELETE; ref-checks moved up to the outbox
trigger so phantom-subject writes abort the operation. The interim
"rebuild-consumed kinds only" classification was ultimately
superseded — **every** emit migrated (three recipes: per-kind
stamps on port mutations, jobs' events-ride-the-write
`events: &[Event]` parameter, and the `EventRecorder` port for
row-less telemetry), idempotency guards double as event gates
(replays record nothing), and
`infra/lint/outbox-migration-ratchet.sh` enforces a flat CI ban on
post-commit publishing workspace-wide. Two full-year from-empty
regens (484K and 1.84M outbox events, zero undelivered) are the
acceptance record.

The log itself is tamper-evident in three layers: append-only
enforcement (BEFORE-triggers reject UPDATE/DELETE), a **hash
chain** (each row stores its predecessor's hash and its own,
computed by a trigger that assigns ids post-advisory-lock so the
verifier's id-walk matches commit order — uncontended now that the
relay is the log's single writer; the chain columns ship in
the schema, chained from the genesis row), and a **daily
checkpoint** that emits the chain head outside the database for
auditor comparison. `boss-audit-integrity-check` walks the chain
on a timer; the release's validation gate (`validate-brewery-sim.sh`)
hard-fails unless the full replay (every rebuilder, from the log
alone) and the integrity check both pass. Every event names its actor —
**there is no anonymous "system" actor**; the four deliberate
spellings (`ActorId` type, `actor` publisher param, `_actor`
payload key, `actor_id` boundary field) are documented in
`boss-core::actor` and must not be flattened. Origin markers on
registry rows use `owning_team = 'platform'`, not 'system'.

**Event kinds are the last vocabulary to become a registry.** The
log's own semantic layer was folklore — 120 distinct kinds across 15
sources on the live box against roughly 19 declared as constants —
while every other vocabulary in the system was already
registry-shaped. `event_kinds` is a **table, not a generated
manifest**, and it is compositional because the kind space is:
static kinds are plain rows, and a **dynamic family** is one pattern
row whose `suffix_domain` names the registry that already owns its
suffixes (`step.done.*` ranges over the StepType registry), so
declaring the family once covers every future step kind without a
migration per step type. A declaration carries a **flat field
inventory** (`payload_fields`), starting empty and filled as
consumers need it — the per-field sensitivity classification the
payload-encryption work wants becomes a column on a row that now
exists. Enforcement is deliberately **warn plus a drift guard**
rather than a write-path abort: an emitted kind no pattern matches
is loud in `boss-audit-integrity-check` and in CI, and the log stays
available under drift, because a registry that could refuse an event
would make the system of record refusable. The seed was **harvested
from the live log** rather than hand-authored, which is why it
matched reality on day one; new kinds are born declared in the same
change that emits them.

**The end state for the bus is the log itself.** The dispatcher's
two durable consumers move off JetStream onto a cursor over
`audit_log` — both already consume `(kind, event_id, payload)`,
which are the log's exact columns, and the cursor pattern ships
twice already. The swap deletes the duplicate durable log, retires
the delivery machinery behind two real incident classes (the
ack_wait/backoff double-fire; the redelivery state-leak that
receive-dedup compensates for), makes the silent-zero-deliveries
filter trap structurally impossible, and removes a stateful service
from the correctness path. Everything else on the bus either wants
at-most-once (the SSE fan-outs) or is not event-log traffic (the
cybernetics message plane), and stays on NATS. Costs accepted with
the decision: side effects trail the write by relay lag plus a poll
interval (human-timescale irrelevant), the retry/dead-letter budget
and the concurrency fan-out must be rebuilt on a cursor, and epoch
trim re-anchors cursors the way it purges the stream today.
Sequenced `dispatcher-rules` first (its `Settle` outcome is already
transport-agnostic), then `dispatcher-steps`, then the stream
shrinks to fan-out-only retention — **staged with the cluster work,
not standalone**. This and the insert-time chaining above were
settled as one decision, because one writer inserting in sequence is
what makes id order ≡ commit order, which is exactly what log-tailing
needs to never miss a row; if sustained demand ever approaches
~1K/sec, both reopen together.

## Finance & ledger

The ledger is a dedicated crate consuming `financial_facts` via a
`FactSink`; the same facts also project from `audit_log` via
data-driven `gl_fact_projection_rules`, so the
rooted-at-audit-log replay check stays viable. RuleSets are
versioned per-RuleSet; rebuild has online and offline modes;
periods are monthly with a fiscal-year close pass; the chart of
accounts is seeded and admin-authored. Financial statements read a
**`gl_account_daily` rollup** (per-account/day debit + credit +
attributed-cash totals) instead of scanning `gl_journal_lines ×
gl_journal_entries` per request; the rollup is incremented live in
`post_fact_in_tx` (same tx as the journal write) and re-derived on
rebuild, so it stays a pure function of the log. Money is an inline
TEXT currency column on every money-bearing row; `Currency` lives
in `boss-core::money`; column prefixes (`amount_`, `price_`,
`cost_`) distinguish kind, not currency.

**The chart of accounts is tenant data; the starter chart is the OSS
default.** (Backlog `41af5195`; design `18cf4272`, David 2026-09-17.)
40-ledger.sql seeds the brewery's chart and that seed stays as the
default every deployment boots with, so the demo tenant and the
posting rules that name its codes keep working untouched. A tenant
declares its own accounts in `seeds/chart_of_accounts.toml` and `boss
tenant publish` sends them through `POST /api/ledger/accounts/batch`
after the classes — insert-if-absent by code, the batch-door shape the
classes, locations and agents doors share, one `ledger.account.declared`
fact per inserted row staged on the outbox in the insert's own
transaction. **A code that collides with a starter row is the SAME
account under the starter's name.** The code is the identity every
posting rule and journal line points at, so two rows cannot share one;
insert-if-absent keeps the registered row and the answer names the
difference (`kept: [{code, differs: [field…]}]`, `1000 (name differs)`
in the publish line), and the tenant then adopts the code as it stands
or chooses another. There is deliberately no upsert and no rename in
place: renaming `1000` would re-label every entry already posted
against it, silently. `boss tenant check` judges the file with the
door's own validation (codes unique, kinds and balances inside the
table's CHECK constraints, a parent declared before its child); the
collision itself is only visible at publish, because only the
deployment knows its chart.

**Counterparty prices are data; our costs emerge.** The vendor's
agreed price (`inventory_items.vendor_price_cents`, seeded per
part) prices the PO **once, at placement** (qty from our
reorder_qty, unit price theirs; an unpriced part refuses placement
loudly). Receiving and bill-approval read the PO's lines — the
purchasing contract — so receipt value, the vendor bill, and the
emergent weighted-average `avg_cost_cents` chain from the same
numbers; `avg_cost` is never an input to purchasing. COGS is
modeled directly from the bill-of-materials × input prices —
margins emerge, never hard-coded. Revenue recognition: hardware at
shipment; `service` defers via `revenue_schedules`; `parts` and
`new-sales` recognize immediately; the recognition scheduler runs
daily and respects locked periods. Sales tax rides
`tax_lines` on the issued-invoice fact and remits per
jurisdiction. The single-shot "DR Cash / CR AR" invoice-paid rule
is deliberately not mapped for tenants whose bank-clearing chain
emits the canonical two-phase pair — double-crediting AR was
observed live and the projection mapping is the cut point.
Finished products are tracked per-location with cost basis
(produce/consume handlers + the products KB); invoices are
line-item based with header rollups checked on write.

**Inventory value is primary; the average is display.** Every
inventory-bearing row carries `value_cents`; per-unit averages are
derived (`value / on_hand`), shown but never an input to a GL amount.
Adds (receive/produce) post exact line totals; drains consume
proportional value with the final unit taking the remainder, so
`on_hand → 0` forces `value → 0` and nothing strands. Conservation —
`balance(1300/1320) == Σ row value`, to the cent, live and rebuilt —
holds by construction because every mutation's GL amount IS the row's
value delta, and it is **gated**, not discovered: a per-account
GL-vs-physical reconciliation runs in the sim validation and the
nightly integrity timers, so the class is never findable by hand
again. **The consume owns COGS** through the products surface — one
writer on the 1320 credit; a module reaching into another module's
projection with direct SQL (the invoice-issue path once UPDATEd
`finished_product_inventory` in place) is the prohibited shape.

**Posting rules are registry data; the code rules are the fallback.**
(Backlog a40541cb on design 18cf4272, 2026-09-17.) `gl_posting_rules`
is an append-only registry keyed `(fact_kind, version)`: `lines` is
`[{account_code, side, amount_path, memo?}]` with each amount an RFC
6901 pointer into the fact payload (integer cents), `basis` is the
tenant's declared accounting basis (cash | accrual — recorded, never a
code path), `source` is `NULL` or `tenant:<id>`. `DataRuleSet` is the
one active RuleSet: it evaluates a fact by the NEWEST registry rule for
its kind and by `BossRuleSet` when there is none, so a tenant's own
fact kinds are rows in `seeds/posting_rules.toml` (the tenant contract)
and the product's kinds keep their tested code rules. A data rule is
admitted only when its debit pointers and its credit pointers are the
same multiset — balanced for EVERY payload, a decidable check at
publish, refused 422 naming the rule — and the balanced-draft check
runs again at evaluation; that language is narrower than the code
rules' (payroll's `gross = net + withheld` stays in code) on purpose.
**A data rule's version is not a `gl_rule_versions` row.** That table
names the interpreter that ran and `gl_journal_entries.rule_version_id`
keeps pointing at it (BOSS RuleSet v1); a tenant's `version` is its
edition of one kind's lines, written into the entry's memo, and a
newer edition re-projects OPEN periods on rebuild exactly as the code
rules do under `OPEN_PERIOD_FACTS_SQL`, never a locked one. The
event→fact half gained `when` — `{"/pointer": value}`, every pointer
equal for the rule to fire — so ONE workflow's step can become a fact
out of the `step.done.task` every workflow emits; identity of a
projection is `(event_kind, when)`, and `step.done.<kind>` carries
`workflow_kind` beside `spec_slug` for it. Both registries are
published through insert-if-absent batch doors that record one
`.declared` fact per landed row and name a kept row whose declaration
differs. The projection runs at the facts rebuild today; a live tail
from `audit_log` into `financial_facts` is the open half.

## Policy & auth

Every write passes `boss-policy` via the `PolicyClient` port.
Rules are row-level grants of `(action, resource)` within a scope;
user overrides take precedence; every decision is auditable. Scope
predicates are named in code (`Self_`/`Team` compile to
**owner_id** predicates — a Job's *owner* is who is responsible;
a Step's *assignee* is who executes; the distinction is
load-bearing and deliberately not flattened). Sign-off authority
is policy: stamping authorizes against `step-signoff:<role>`
resources, uniformly — simulator included. The policy client
**fails closed**; a 60s TTL cache floors correctness with NATS
invalidation as the convenience overlay. (The packet model's
stage-2 re-key of `Self_`/`Team` onto queue-derived ownership —
§The network substrate — is the one decision that would move this
predicate; it is resolved but unscheduled, and until it lands the
owner-keyed compilation above is the truth.) SPA auth is file-backed
credentials managed by the gateway's admin CLI; SSH is
bring-your-own-keys with the SSH-CA flow parked as an opt-in
blueprint.

**The front door for real people is Kanidm** (living contract:
`docs/design/idm-kanidm.md`), on the GCP box rather than in the
cluster — stable public IP, and rebuilding the cluster must not lose
the company's logins. Two invariants make it BOSS-shaped rather than
bolted on. **Kanidm authenticates; it never provisions**: a login
maps to an *existing* employee Subject, joined on email, or it
**fails closed with an audit event** — people enter the company
through the People domain, where hiring is a Workflow with a trail,
never as a side effect of first login (a pending-access Job is a
later nicety, not v1). And **the policy engine never learns Kanidm
exists**: OIDC is another way to authenticate an email, and
everything after the email is the pipeline local login already uses
— roles are read from the employee row, exactly as they are for a
local session. (The design doc proposed an `idp_group_roles`
registry so Kanidm could own membership; the shipped runtime
deliberately does *not* have one, on the ground that it would be a
second source of role truth. The two statements disagree and the
code is the current truth here — see §Open findings.) The
**gateway holds the session**
— OIDC at login only, every downstream service untouched; per-service
bearer validation waits for service-to-service auth to actually need
it. Agents get Kanidm service accounts in **phase 2**: humans first,
agents while the forged-claim header path still works, then that path
dies. Kanidm's own state is the second member of the
outside-git-and-Postgres class (with `credentials.toml`) and its
online backup rides the existing `backup.sh` timer. It terminates its
own TLS at `id.algedonic.dev`, DNS-only, with the gateway's OIDC
callback staying behind the existing front. **Local auth survives as
break-glass** — an IdP outage must not lock operators out of the
system that runs the company.

**The gateway joins the log.** Auth denials were structured warn
lines; with a real front door they are security telemetry, and "who
tried the door" is a company fact. The gateway therefore gains **one
small Postgres pool used only for audit staging**, on the existing
`EventRecorder`/`PgOutboxRecorder` recipe, connecting as a dedicated
role with INSERT-only rights on `event_outbox` — least privilege for
the one internet-facing service. The alternative, an authenticated
ingest endpoint on events-api, was **rejected**: it either reopens
the measured single-writer decision or reintroduces the retired
post-commit-publish shape over an HTTP hop, spending a new
credential class for strictly worse durability. The `tracing::warn`
line stays as the backstop when the bounded queue is full or the
pool is down — **degrade to today's behavior, never to silence, and
never block a login**. Three kinds ship, registered in `event_kinds`
with `source = 'gateway'`: `auth.login.denied` (a closed reason enum,
and deliberately **no subject reference** — no employee matched, and
a reference is exactly what the ref-check trigger would rightly
refuse), `auth.login.succeeded` (carrying `method`), and
`auth.session.guest`. IdP transport failures — discovery, token
exchange, userinfo — stay warn lines: plumbing facts, not
who-tried-the-door facts. **No per-request events**, ratified as a
standing constraint rather than a deferral.

**One actor, one identity: an agent's login resolves to a registered
id the way a human's does** (design `6fda05ae`, David 2026-09-12; all
three questions accepted as proposed; folds backlog `adf025df` and
`7dd9f28c` into one decision). Measured: the same CPU was
`claude@algedonic.dev` on `steps.assignee_id`/`completed_by` and
`claude:opus-5[1m]` on `agent_runs.actor_id`, so the question the
agent-runs module exists to answer — what did this actor build, at what
cost — could not be asked, and `ActorId` carried no address arm at all:
the address was live data the vocabulary did not describe. The human
side already had the answer — `boss-gateway/src/oidc.rs` resolves a
login to an `emp-*` id before any write is signed and fails closed on
no match — so an address surviving to a step meant that resolution
never happened for agents. Decided: (1) **the canonical id is an
emp-style opaque id in an agents registry** (`agent-<slug>`), never the
login address and never the model-qualified colon form — addresses are
logins (aliases), models are facts about a run; (2) **the model lives
on the run** (`agent_runs.model`), the agent row carrying its default
model and its caps, because one registered agent runs different models
over time and cost is priced per run; (3) **an unregistered agent login
is refused, failing closed**, exactly as a human login matching no
employee is — after a one-release migration window in which an alias
table maps the one live address to its agent and a lint counts writes
still arriving under an alias. Budgets (`AgentSpec` caps, a decision
recorded before a run starts) sequence after the run is a recorded
fact. Car 1 (registry + alias + resolution, window open) building
2026-09-15.

## Calendar

Reservations store **UTC**; `strength` defaults `hard` for
subjects that can't double-book, `soft` for advisory holds.
Multi-occupancy resources are distinct subjects, not capacity-N; one
reservation per subject per event (sharing `reason_ref_id`).
**A reservation is on a `Subject`** `{kind, id}` — not a closed
resource enum. Which kinds may be reserved is data: a
`calendar_reservable` flag on the subject_kinds registry (employee,
asset, account at v1), enforced by the calendar on reserve; the GIST
exclusion constraint guarantees one hard reservation per individual
subject per overlapping window.
The `reason_kind` is likewise a **free-form tag**, not a closed enum —
the conventional values BOSS emits (`job-step`, `pto`, `meeting`, …)
live as consts in `boss_core::calendar::reason`; a tenant uses its own
reason without a core change.
Cancellation is synchronous with the step update; PTO lives in HR
and the calendar sees only approved PTO; the jobs↔calendar hook
reserves before persistence so a hard conflict can 409 without
half-writing the step.

## Locations

Locations are a Subject kind with a parent hierarchy (no hard
depth cap; warn at 8). Address is free text at v1. Location-Part
singleton enforcement is a write-path helper; movement history is
event-log only.

## Simulator

One **shape-driven engine** drives both tenants; per-tenant flow
is data (`workflows.toml` step graphs; `tenant.toml` rates, ramps,
anomalies, shocks, counterparties, periodic and batch cycles). The
workforce executor claims and completes **assigned** steps through
the public API as the role-matched employees, filling
required-at-done fields (bundle + step-authored) and collecting
sign-off stamps before completing — metadata first, stamps
attesting the final shape, then the status flip. Gates are
agent-executed by the dispatcher reading real stock — the
workforce never sees them. Brewery production is **demand-pull, not
open-loop**: each `morning-brew*` kind is a `deterministic` daily
review (one brew *reviewed* per working day — the rate is the
brewhouse's per-beer slot capacity, never a Poisson draw that could
silently emit nothing), and the gate is in-flight-aware, crediting the
pipeline (`effective_on_hand = real + open_sibling_jobs × batch_yield`)
before deciding brew|oversupply so the daily review doesn't double-brew
through the multi-day brew lag. Batch engines (payroll, taxes) are
generic over Population + Rule traits. Warp is honest: the sim
runs at the throughput the serial write path sustains, and the
canonical 365-day world must pass hard-fail (any non-2xx aborts),
queue drain, full rebuild parity, and chain integrity for the
validation gate to go green. The scratch stack mirrors prod at +1000 ports
for experiments. The daemon is a **cursor-gated auto-tick loop**:
clock-authoritative time, each sim-day processed exactly once
(`days_to_run`) — which fixed the cold-start over-firing (periodics +
rate engines re-firing on overlapping day windows) without the
heap-scheduler refactor that was prototyped (`boss-sim/scheduler.rs`)
but deliberately not adopted, the simpler cursor gate being sufficient.

## ML platform

`boss-ml` + `boss-ml-api` (gateway-proxied under `/api/ml/*`);
inference plugins live in `boss-ml-plugins` and register via
constructor wiring — no dynamic loading. Models bootstrap from
embedded TOML seeds; predictions store as JSONB; scheduling is
systemd cron. Next-action rules and risk scoring are declarative
rule models with plain string-template substitution — no embedded
scripting.

## Content, files, knowledge

Bulletins and the company manual are separate tables in
`boss-content`, Markdown-authored, searched via the shared FTS;
the manual writes a history row per edit; bulletin audiences are
JSONB predicates evaluated in Rust. **File attachments are
first-class auditable artifacts**: a two-port design (metadata
rows + content storage) with upload/GC lifecycle, served through
the gateway, rebuild-deterministic like every projection. Each
domain's KB documents hang off the `Document` type; the Equipment
KB keeps typed columns for stable queried fields and a
schema-validated `extras` blob for tenant-specific evolution; the
event stream remains the source of truth for asset state.

## Search

One core endpoint (`boss-search`) queries `subjects`, `jobs` and
`audit_log` in a single round trip; each app contributes its own
scoped search for domain detail — the global box answers "what and
where", the app answers "which one". **Search reads its own
projections, rebuilt from the log** — not the live domain tables and
not the log directly — so a Subject absent from a domain projection is
still findable and the index reproduces rather than drifts. Results
group by kind in a hard order (Subjects → Jobs → Events, recency
within a kind); there is deliberately no cross-kind relevance score,
because unexplainable ranking is how search boxes lose trust. Policy
scoping is server-side in the same `PolicyClient` path as every other
read — a result the caller could not open must not appear, and
client-side filtering of a wider set is prohibited. The chrome
dropdown is the current app's scoped preview; the full cross-app
results page lives in Home. v1 shipped the unified claim whole (one
query returning a Subject with its Jobs with their events) rather
than name-lookup-first — the join on system-issued identity is the
point, not a later feature.

## Step UX & frontend

Step surfaces ship as **data**: the registry row names a
`surface` id; the SPA loads the step-type registry once and mounts
tenant StepPlugin → the platform surface the registry names →
the generic fields/notes card. Plugins are JS bundles in rows
(`step_plugins`, append-only versioned; steps pin the plugin
version at creation), served by the gateway at `/plugins/<path>`,
mounted framework-free with declarative validation — new step UX
never requires a core SPA change. The frontend is **Svelte 5 with
Runes** (no stores), one Bun bundler, in-app router, CSS grid
layout. Live views poll at 60s with SSE push where the policy doc
says push pays (`docs/design/sse-policy.md`); the System Diagram
complements the HQ map; account detail composes KB panels
(devices, invoices, shipments, agreements, notes) over the
domain APIs. The ports table (`boss-ports`) is the single source
of truth for service names/ports — the SPA's generated copy is
lint-checked against the Rust registry.

**The activity surface draws the network, not the route map.** A
workflow DAG is a route map, and stacking N kinds as N sections
could never show that two packet classes share the same operator —
so the **canvas is `/system/flow`'s hero**, per-kind decorated DAGs
demote to the route ghost and the Fleet inspector, and `/system/os-map`
retires as a page while its LAG-pairing SQL survives as the traffic
layer, re-keyed from department to station. **Rails are merged** into
one faint overlay of all declared routes with per-kind tint on
hover: stations serve many kinds by construction, and per-kind rails
would re-partition exactly what stations-as-queues unified.
**Packets move on the marker topics** — `step.ready`,
`step.assigned`, `step.done`, plus `jobs.job.closed` as the
departure — while `jobs.step.updated` metadata chatter stays
ticker-only; transport is hybrid per the SSE policy, dots riding the
existing push stream and depth piles and edge thickness riding the
polled aggregate. **Personal-queue stations materialize when
non-empty or recently active** (the os-map's nodes-from-edges rule
restated for queues, honest at the measured occupancy), the claim
hop renders in the one transfer grammar with the push-vs-self-claim
split as a tint rather than a second gesture, and the claim
compare-and-set was a hard prerequisite — without it the canvas
could animate two actors winning the same packet. **No company-level
canvas in v1**, but every layer endpoint takes a scope parameter
from its first version so the recursion is a query change rather
than a rewrite, pinned by a test that a department-filtered canvas
is self-consistent. Simulated traffic is always counted separately.

**The personal unit is a View, not a gadget.** A View is a saved
composition — a query plus a layout — holding no authoritative state
of its own; it is a pure function of the log, so it rebuilds, cannot
drift, and two people running the same View see the same numbers.
This is the deliberate inversion of the private-durable-state
micro-app (Cloudflare OS's gadget), which is the federation problem
returning one user at a time. Local state is allowed **while it stays
local**: inside a Step until the Step completes, scratch on a View
until it flows into a Job, Step or Event — the test is whether
anything outside depends on it, not whether it exists. Views are
declarative compositions (reviewable, diffable, deterministic);
agent-authored full-code apps are a later phase gated on
safe-user-code infrastructure. The promotion ladder: personal View →
shared (frictionless, the individual curates their own shareables) →
inclusion in a department's views (a submitted Job) — ceremony lands
only where something becomes the company's. **IT is the department
app and System Model lives inside it**: modeling the company is work
the IT department does, so dispatcher rules, step plugins and
experiments need no IT-vs-model line. Department Apps are the decided
workflows (registry-governed, the same for everyone in the role);
Home is where an individual explores what has not been decided yet.

**Aborting a job asks for the reason** (design `c6f9fb3e`, David
2026-09-15, all three questions accepted as proposed; from his feedback
`33324fe9`: "I need to be able to Abort a job in the UI, but make me
provide a reason"). Every workflow already declares its abort as a
terminal step with `outcome_kind: aborted` and a `reason` field, so an
abort is a step completion, evented, on the record; what did not exist
was one control that finds it. Decided: (1) the job page shows
**Abort…** whenever the job is open and its workflow declares a step
with `outcome_kind = aborted` — two such steps and the control lists
them and asks which; none and there is no control, because the
protocol says the job cannot be aborted and the *row* is what changes,
never the page; (2) the reason is **required, free text, at least a
sentence** — not a code from a list, because the list is what we would
maintain and the sentence is what the next reader needs — recorded on
the step as `reason` with the session's actor as `completed_by`, and
nothing else is asked; (3) **whoever the step's `authority_role`
admits may abort**, the ordinary claim rule and no separate permission;
a viewer without it sees the control disabled with the role named.
Built as `7a98040e`.

## OSS posture & tier boundaries

Two install paths: single-VM bare metal (`infra/oss-quickstart/`)
and Docker compose. File-backed auth is for evaluation; HA
topologies return as opt-in blueprints under `infra/blueprints/`.
Crates split into **Tier 1 — core state-machine OS**
(`crates/core/`, 27 crates: the four primitives' services, policy,
gateway, dispatcher, clock, expression DSL, taxonomy registries,
calendar, content, docs, ML stack, cybernetics, testing, ports,
plus `*-client` crates) and **Tier 2 — company-modeling layer**
(`crates/modules/`, 16 crates: people, accounts, commerce,
inventory, shipping, ledger, products, messages, catalog, assets,
clients, ML plugins). A non-company tenant deploys Tier 1 alone.
**Orchestrators** (`crates/orchestrators/`: `boss-rebuild`,
`boss-cli`, `boss-sim`, `boss-ml-api`, `boss-simulator`) fan out
across tiers by design; **tenants** (`crates/tenants/`: brewery
engine, used-device-shop engine) carry tenant binaries.
`infra/lint/tier-import-audit.sh` enforces
Tier-1-never-imports-Tier-2 for libraries. Seeds never write
emergent state — if a seed wants to `INSERT INTO invoices`, the
answer is a Workflow (`docs/design/seed-vs-emergent-state.md`,
enforced by `seed-bypass-smell.sh`); the canonical demo world is
**built live, not migrated**: the install starts the sim and it
generates 365 simulated days of events against the live API.

## Deployment, the forge, and the cluster

**Deployment is modeled on how networks patch** (living reference:
`docs/design/deployment-as-network.md`, which the deploy scripts and
unit files cite by question number), which means distinguishing what
kind of thing is changing. **Traffic** —
requests in flight, Jobs mid-step, the log — is never rolled back; a
delivered response is history. **Derived state** — binaries,
projections, the served SPA — is *reconverged*, not restored:
rebuilt from intent, freely replaceable, no snapshot nostalgia.
**Intent** — the repo at a commit plus the registries and config —
is the only versioned layer, and "rollback" exists there only as
rolling *forward* to a prior intent. Two prerequisites were already
policy, which is why the rest is plumbing: expand/contract
migrations are exactly the N-1 compatibility that lets a reverted
binary run on today's schema, and the SPA's content-hashed dist is
make-before-break natively.

**Generations make installs make-before-break.** A release lands in
`releases/<sha>/` carrying `bin/`, `web-dist/`, `step-plugins/` and
the source-fingerprint stamp, with `current` and `previous`
symlinks that unit `ExecStart` lines go through; deploy is install
beside, flip, restart, and revert is re-point and restart — seconds,
not a rebuild. Three generations are kept, with an explicit prune
step that prints sizes, because this box has had its disk-full day.
The web dist joins the generation, so `rsync --delete` retires
cleanly and the SPA's content-hashed naming finally has a revert
path.

**The flip is commit-confirmed — a dead-man switch.** After a flip
the deploy is UNCONFIRMED, and the confirm is every deployed unit's
health probe returning 200 (reusing the deploy roster itself, so the
confirm cannot drift from the deploy list), plus dispatcher readiness,
plus one write round-trip through the HTTP API. It is read at **+2
and +8 minutes** — the delayed second reading is what catches the
dispatcher silent-death class — and an unconfirmed deploy
auto-reverts at +10. The evaluator is its **own systemd unit armed
at flip**, never an in-process wait inside the deployer: a 45-minute
build timeout once killed the deployer mid-run, and a dead-man
switch that dies with the process it guards reverts nothing.
Auto-revert covers binaries, web dist and step plugins; **schema,
registries, the log and the data stay** — roll-forward only. Two
riders follow from that: emitted config bodies snapshot into the
generation and restore with it, and events written during the
unconfirmed window stay in the log forever, so projections and
rebuilders must tolerate unknown event kinds — closure doing
revert-safety work. The conductor completes the train Job's
deploy step only on the confirm marker, and an auto-revert reopens
it.

**Scratch is wave 1, named honestly.** Per-environment `current`
symlinks are what make a wave seam possible at all; scratch's
confirm covers only the paired services it runs, so it reduces
prod's exposure and never replaces prod's own confirm and dead-man.
Real per-node waves and true drain-patch-undrain arrive with the
cluster; on one box, restart order plus health gates approximate
them, and the approximation is named so nobody mistakes it for the
thing.

**Git and CI come inside, on Forgejo.** Internalizing is what turns
the merge wall into policy — the ship-a-change `review` step gains a
required operator sign-off, and the conductor, on seeing a
signed-off review, calls the forge adapter's merge verb and stamps
the merged marker exactly as before: the observe-then-mark shape
survives, the observer becomes the executor. CI is **Forgejo
Actions**, chosen de facto once GitHub-Actions compatibility held
with two container-job deltas, with the CI image's Dockerfile in the
repo and the gate script pinned on both sides so a second gate
definition cannot drift into the workflow file. The GitHub mirror
becomes a **push-mirror on every main update** — superseding the
earlier daily cadence, because a disaster-recovery copy of the
system of record that is a day stale is a day of lost commits, and
the GitHub-native checks only audit what the mirror shows them.
Inbound stays deliberate-pull through GitHub PRs, keeping external
code off internal runners. **Forge events land on the outbox** via a
small ingress that validates the webhook secret and stages with
`record_event_in_tx` — never post-commit publish — with
`forge.push`, `forge.check.completed` and `forge.merge` born
declared in the event-kind registry.

**Maintenance stops being invisible work.** The department's
recurring labor — backup, audit integrity, ledger replay checks,
views catchup, GC, purges — ran as systemd timers outside the Job
model, in a system whose thesis is that work is visible. Each chore
is now a **maintenance Workflow kind**: success completes the run
step, failure completes nothing and leaves the Job open and loud,
and recovery closes the standing Job, so a failed backup is an
algedonic signal instead of a quiet journal line. The spawner is
deliberately the **systemd timer's wrapper on wall-clock time**, not
the dispatcher's schedule runner — an amendment to the original
proposal, ratified: sim-day rules fire every couple of wall-minutes
at warp, and maintenance is wall-clock work. The cadence loop
retired the *train's* timers, not systemd itself; the remaining
timers are a rollout list, not a claim.

**The cluster reaches the hub over bare WireGuard.** The GCP box is
the hub — stable public IP, overlay `<overlay>/24` — and cluster
nodes are spokes that dial *out*, so no inbound hole is opened in
the home router and a keepalive holds the NAT mapping; node-to-node
traffic inside the cluster stays on its own mesh. Kanidm and the
log-copy migration both ride that wire. The cluster is a *client* of
identity and a consumer of intent, never the host of either: moving
the company is copying its log and its rules, and everything else
regenerates.

**A workspace declares what it guarantees; that is the half that has
shipped.** Allocation was decided first (`2d43cbcb`, 2026-08-16): a dev
node is a **`service-instance` Subject**, the pool a StatefulSet whose
ordinals give stable identity and a per-replica PVC, **the checkout Job
the lease** — its terminal releases it — with a read-only credential
and a sweep for leaks. Review `775f0b35` (2026-08-29) then settled
what a workspace *guarantees*, separately from who gets one: the
capability declaration belongs on the Subject, not on the checkout
packet (Postgres is there whether or not anyone has leased it; on the
lease it would be copied onto every checkout and drift per-lease);
**evidence before enforcement** — record which workspace the work
happened in before refusing work from one that lacked a capability,
because nothing yet says what a change *needs* and inferring it from
touched paths would refuse correct work; and **a laptop is a workspace
like any other, with its capabilities declared false** — a car that
passed 285 local tests failed the gate on a roster test that cannot run
without Postgres, 118 database-backed targets unrun on the workstation.
What the code does today is the last of these: the pre-flight
**opens** by reporting what the workspace cannot cover
(`workspace-declares-what-it-runs.sh`, pinned first in `gate.sh`),
which is that guarantee made legible at the point of work. The pool,
the lease, the `service-instance` kind and the `build` step's record
of its workspace are decided and **not built**: the dev session is one
Deployment on the build node, allocated by hand.

**The train is tested where the seed is** (design `128b5496`, David
2026-09-12, all three questions accepted as proposed; cars landed
2026-09-13). Measured off the forge's task list on 2026-09-12: CI's jobs
ran *serially* on the runner's one slot, about seventeen minutes a run,
of which `test` — `infra/gate.sh`, cold, in a fresh container — was
eleven to thirteen; and every train ran twice, once on the PR head and
again on the squash commit, for a result nobody read. The cluster gate
runs the *same script* warm on the reflink seed in three to five
minutes, and the forge's disk is ext4 (no reflink) and chronically
under its floor — so the seed does not move to the check; **the check
moves to the seed.** (1) **The launcher is the conductor**: when it
opens a train's PR it files a gate-run for the *train branch* — the
assembled tree — and creates its Job the way `boss gate` does, under a
`boss-conductor` ServiceAccount bound to the gates Role the dev pod
already uses, with kubectl and the runner manifest in the cluster
image; the gate-run carries `train_gate`/`train` marks and a `hold`, so
the stranded-green sweep reads it as HELD and auto-park leaves it alone
(`boss-cli/src/train_gate.rs`). A standing launcher that turns *any*
requested gate-run into a Job — a gate filed from the forge, a Mac, or
boss-gcp — is the general door, deferred. (2) **The verdict is both
halves read together**, off the system of record and never posted back
to the forge (the forge stays a one-way peer): a green CI does not merge
a train whose gate has not spoken; a red gate strikes the cars like a
red CI; a refused or lost gate is filed again up to three times and then
the train reads *aborted*, cars released unstruck. While CI still ran
the Rust checks, a gate the conductor could not *file* fell back to CI
alone after three passes, stamped `train_gate_fallback`; car 3 sets
`BOSS_TRAIN_GATE_REQUIRED=1` in the same change that drops CI's `fast`
and `test`, and from then on a train waits. (3) **The per-train image
stays**, contrary to the proposal as filed: `locomotive.sh` reds when
the image's baked stamp disagrees with the tree, and a floating tag was
once served stale from the runner's cache (`6aa603ef`) — so
`build-image` keeps pushing `boss-ci:<sha>` (about a minute, layers
cached) and the disk cost it carries is the bigger forge disk's to
absorb. The double run went first (`fix/a-train-is-tested-once`,
2026-09-12: `pull_request` alone); the two hours of unconverged trains
on 2026-09-13 taught that a tag in the mirror *list* is a declaration
and the registry holding it is the fact, which the converge now closes
before it builds.

**A car lands where its change goes live** (design `c6bd173e`, David
2026-09-15, all three questions accepted as proposed; from his feedback
`61366e5a` on the yard). Every car already carries a
`delivery_channel` — data | config | software | infra — stamped at gate
time from the paths it changed, the heaviest winning
(`boss-cli/src/channels.rs`); the yard read none of it, and "landed"
meant one thing, the image converged, whatever the car changed. Now:
(1) **the arrivals yard is four sidings**, one per channel, and a car's
wagon lands on its siding when *its channel's* live evidence exists —
data when the live-protocols / live-rules equality holds for its files
(read off the system of record), config when the converge packet
reports the manifest applied or the unit installed, software when the
train's `converged` step completes, infra when the host converge
packet reports — with the car's probe the only *proof* in all four.
(2) **Three terminal tracks and only three**: arrivals by channel, the
inspection shed, and a cancelled siding for a withdrawn car; struck and
left-behind are states of a car ON THE DOCK, drawn there with a badge,
never a track. (3) **The train has a channel too**, the max of its
cars: a data- or config-only train's `converged` is satisfied by the
registry read / manifest apply, not the image roll, so those trains
arrive in minutes. Built in three cars: the sidings by the stamped
channel first (`953aaf30`), the train's channel second, per-channel
landing evidence third.

**A builder is a pod, not a process in the operator's shell** (design
`90a14acc`, David 2026-09-15, all three questions accepted as
proposed). Measured 2026-09-14: two builder agents and the operator's
session in one 8-CPU / 16 GiB cgroup put memory at the ceiling and
throttled the pod 2,177 periods in an afternoon; builder throughput
was bought with operator latency, and a builder was invisible to the
record while it worked. Decided: `boss builder <packet>` renders a Job
the way `boss gate` does — the brief handed in, a cargo target
reflink-seeded from the gate seed volume on w-1 (the pin to w-1 is
accepted for now; a registry image of the target is the alternative),
the branch gated with the same park intent a human builder types —
and (1) **its forge credential is a short-lived, branch-scoped token
minted per run by the credential broker**, placed as a Secret the
launcher creates and deletes with the Job; never the operator's
identity, never one shared long-lived token; (2) **the pod ends when
its gate is launched**, with a 90-minute ceiling after which the
observer settles it as a dead runner; (3) the seed is the gate seed
volume by reflink. **Until the broker mints** (the maiden rotation,
`25dc5358`) builders stay in the dev pod: a bigger pod (`5ee0ff2a`,
held for a David-timed restart) and 4-wide niced cargo for `agent-*`
worktrees (`infra/dev/wt-cargo`), which took the throttling from
2,177 periods to ~350 over the next ten hours.

**Cluster management runs on an internal host; the workstation is a
terminal** (design `1bc4b4ed`, David 2026-09-12: "I would rather one of
the internalized systems be where commands run"). Measured: every
`talosctl` and admin `kubectl` ran from `~/talos-homelab/v2` on a
laptop — the cluster's root credential and machine configs on a host
that is not managed, converged, observed or reachable — and the gate
seed's kubelet mount took nine hours to diagnose because three of the
reads that settled it had to be typed by a human and pasted back.
Decided: (1) **the forge declares a `cluster-operator` role** (a Class
row on the node, `infra/estate/roles.toml` saying what the role
installs) — "start with forge; a GCP VM may make more sense later", so
w-2 off Talos and a GCP VM are recorded as future options for
separating the credential from git+registry; the role brings the
interactive door first (`talosctl`/`kubectl` on the forge, credentials
root-only under `/etc/boss-ops`, installed once by David — the
estate's converge never writes a credential — and the Talos configs
move there as the canonical copy), then (2) **mechanical reads as ops
verbs** — `node-status`, `pod-describe`, `pod-events`, `talos-ls`,
`talos-logs`, `talos-get`, literal argv, reads only — and (3) **one
bounded act**, `node-converge <node>`: `apply-config --dry-run` against
the per-node file declared in the repo (non-secret entries only; PKI
stays in `/etc/boss-ops`), the diff recorded on the packet, then apply.
**Reboot, upgrade, reset and etcd membership never become verbs.** The
question of *how the operator's own terminal should carry the
credentials* is not decided: David's answer asked what professionals
do and whether a small standalone management tool should ship for
terminals like the Mac, and that stays open on the packet.

**The knobs outside the tree become declared settings** (design
`16115a17`, David 2026-09-12; all four questions accepted as proposed).
Measured 2026-09-11: three of the five levers behind a 22-minute outage
of the system of record and a publish PR closing itself two minutes
after it opened were settings that live in no file here — a Forgejo
push mirror found only through its API, the Talos kubelet image-GC
thresholds, Longhorn's node-down pod-deletion policy — each an
imperative act with no packet, no declared value to converge from, and
no observer comparing it to anything; and retention had the same
absence, three reclaim scripts each carrying its own constant for the
lifetime of an image, a target dir, a `publish/<date>` branch. Decided:
(1) **scope** is the four that bit — Talos kubelet extraConfig, Longhorn
settings, Forgejo repository settings, and the retention rules — with
GitHub token/repo facts recorded but not compared (no read without the
token); (2) **one table, `declared_settings`, with a `kind` column**
(`setting` | `retention`) — retention *is* a declared setting of an
artifact kind, and one observer and one alarm path is the point (two
tables would drift the way the three scripts did); (3) **apply is an
ops verb per system where a runner can reach it** (Longhorn via the
conductor's kubectl, Forgejo's API from the forge) and **a named human
step for Talos** (`talosctl` is David's door), and **verify is always
the observer's next comparison, never the applier's exit code**; (4)
**drift raises through `estate.alarm`** as a hard finding carrying live
and declared values, never auto-applied, and a setting nobody declared
but the observer can see is reported **UNDECLARED** — the third state
beside declared-and-matching and drift. Not yet built; the Talos
entries the gate depends on are being declared first (`08430090`).

**A maintenance chore states its coverage, its numbers, and its
suppression** (design `14c135f5`, David 2026-09-11; all three
questions accepted as proposed). Nine maintenance mechanisms were found
failing in one session on 2026-09-10 and they were one shape, not nine
bugs: a chore is not obliged to report honestly about itself. Seven
daily sweeps dead nine days each behind its own undrained packet; the
mirror check dead nineteen days while the mirror drifted 238 commits;
a branch sweep reading the newest fifty trains and calling it total;
139 disk-floor packets recording `result: ok` and no number while
computing the very number that decided it; a `disk_tight` alarm that
could never fire for a cluster node because the observer records
capacity, never headroom. **Coverage** — what population was examined
and whether that was all of it; **numbers** — what was measured, not a
word for how it went (CLAUDE.md's rule about reducing a record before
storing it, applied to successes); **suppression** — "did not run
because a packet is open" distinguishable from "ran and found nothing".
Decided: (1) **the contract lives in the Workflow registry**: the
`measured`/`coverage` fields are REQUIRED on maintenance chore kinds via
`metadata_schema`, so the protocol refuses a chore packet that cannot
say what it examined — the packet is the durable record, and a lint
over scripts would catch the author but not the packet; (2) **a
suppressed firing refreshes one open record, never twins** — the estate
alarm's existing idiom (bounded dedup read, refresh not twin, a human
close suppresses for a week, self-clear stamped) — because a packet per
suppressed tick would file hundreds and no packet is the invisible
state that cost nineteen days; (3) **a chore that cannot state its
coverage FAILS**, not warns: the audit-integrity alarm's history is the
proof that permanently-red and green-with-warnings decay the same way.
First instance landed 2026-09-15: the protocol-drift chore records
`measured` (kinds compared, fields, head) on every run.

**The LAN machine door carries every read surface of the instance, not
only the jobs API** (design `28d2bed9`, David 2026-09-17; both
questions resolved as proposed). Measured: `boss-jobs-internal`
(10.20.0.34, MetalLB, LAN-only) exposed one port, and
`boss-sor-read` — the one way a car's recorded probe reads the system
of record — was `curl $BOSS_JOBS_URL$path`; so the batch-door events
car (`056f7bd8`) sat UNPROVEN because `/api/events/tail` lives on
boss-events-api and answered 404 on the jobs port, the locations door
(`1ec8312a`) was proved by tree shape instead, and every future car
about people, classes, locations or the audit tail had the same
blindness. Decided: (1) **the door widens, the trust class does not** —
the events, people, classes and locations ports join the same Service
on the same IP, header-trusted and LAN-only exactly as the jobs API
already is there WITH its writes (the conductor writes through it), so
a read service on that IP exposes nothing to anyone who could not
already write the record; the manifest's ports are boss-ports' prod
ports under boss-ports' names, pinned by one equality test
(`the_machine_door_carries_every_read_surface.rs`, CLAUDE.md §9a); (2)
**the reader routes by path** from a `name=port` table rendered from
boss-ports into the ops-runner's environment (`BOSS_SOR_PORTS`, read
by run-car-probe.sh from `infra/forge/sor-ports.env` in its own
checkout), defaulting to jobs, still one argument, still refusing a
URL and an unidentified read, and unchanged when no table is present;
(3) **the gateway service-token door is deferred** to the hardening
programme, as the machine door for OFF-LAN callers — the probe runner
is on the LAN and needs no public door, and a proof instrument that
depends on Cloudflare and a credential rotation is the wrong
dependency to add before hardening starts. Not chosen: the jobs API
proxying read-only paths for the probe tier, which duplicates the
gateway's routing inside a domain service.

## Design docs and the decision record

The markdown corpus stopped being the source of truth and kept the
title, and everything that broke around design review followed from
that. **The packet is the doc.** Decided in review `87f5bc84`
(2026-08-29), on evidence: 146 recorded answers had never reached a
file (11 queued, 135 in failed flush jobs); a terminal step named
*"Settled — carry it to a file"* completed twice and wrote nothing;
and a doc's status was a hand-written line nothing updated, so 20 docs
claimed live discussion while having no open questions. None of those
were independent bugs — they were the cost of keeping an authored file
and a projection of it in agreement when the decisions already lived
somewhere else.

**Generation runs from the packet outward.** Previously a human
authored `docs/design/x.md`, an indexer parsed it into rows, decisions
were recorded against those rows, and a flush job tried to write them
back — the file was the source and the database the projection. Now
the packet is authored and the file, where one exists, is a generated
artifact. This is the rule the rest of the system already lives by;
design docs were the one place it was inverted.

**The legacy corpus is translated once, not indexed forever.** All 52
markdown docs become packets and the directory stops being read;
leaving it as a parallel source recreates the two-sources problem the
change removes. The 49 legacy `design-doc-review` packets are archived
as they are, and their stranded decisions are **not** flushed — the
answers are already on the packets, which is where the fold reads
them, so writing them into files is work in the direction being
abandoned.

What this deletes, and the deletion is the point: the pending-decision
table and the whole flush pipeline (including a service that ran `git
commit` and `git push`), `boss docs flush-pending` and `boss docs
reindex`, the prose parser and its conventions (`**Status**:` in the
first thirty lines, `## Open questions`, `### Qn:`, `(resolved)`), the
corpus lint that enforced them, and the `stale-statuses` and
`rejections` reports — with them the concept of a "drifted" doc, since
a packet's status *is* its status.

**The write-back half was deleted on 2026-09-10** (backlog `f5da586c`,
filed because this record had described the deletion in the present
tense for twelve days while the pipeline kept running). Gone: the flush
pipeline end to end — `design_flush_jobs`, the four `/api/design/
flush-jobs` routes, `boss docs flush-pending`, the markdown surgery it
applied, and the `git commit`/`git push` it ran; `POST`/`DELETE
/api/design/pending-decisions` and the step plugin's mirror into them;
the `stale-statuses` and `rejections` routes, their two SPA panels, and
`design_docs.pending_count`; and the two dispatcher rules that existed
only to serve that half (`design-decision-flush-queue`, and
`maintenance-sweep-doc-status-daily`, whose entire content was the
drifted-status report).

Part 1 stopped at the read half and left two findings, both now
settled. First, the pending-decision rows were **load-bearing for the
read half**: `upsert_doc` read them on every reindex and force-resolved
their anchors, because reindex rebuilt the question set from the parse
and an answer known only outside the file was otherwise erased on every
boot. Measured then: dropping both tables would have re-opened 25
questions across 6 docs and handed back six review packets for questions
already answered. So the table survived one day, renamed
`design_recorded_decisions` and backfilled from the stranded flush
payloads — a closed ledger with no writers. Second, `boss docs reindex`
and the corpus index stayed, because the daily
`design-review-level-sweep` read them and retiring a working rule was
not a call to make inside a deletion.

**The read half was deleted on 2026-09-10, the same day** — the
sequencing had to run that way round, because the ledger exists only to
stop a reindex from re-opening an answered question, and with no
reindexer there is nothing for it to stop. Gone: the whole `boss-docs`
crate and the `boss-docs-api` service (the parser, the reindexer, the
port and both adapters, the remaining `/api/design/docs` routes and the
gateway's proxy to them), the three read-cache tables
(`design_docs`, `design_questions`, `design_doc_rejections`) and the
ledger with them, `boss docs reindex`, the `docs.design.sweep` handler
and the two dispatcher rules it served (`design-review-spawn`,
`design-review-level-sweep`) plus the one-kind
`open_review_exists` helper the first of them used, the `corpus` panel
on `/it/design`, and the corpus lint (`docs_corpus_presents.rs`) that
enforced the file conventions. **No history is lost**: `audit_log` is
the system of record and every `docs.design.indexed` /
`docs.design.decision_recorded` event stays in it; these were
projections and read-caches.

Three things that deletion established, worth keeping:

- **The `### Qn:` / `## Open questions` / `**Status**:` conventions were
  FILE-WORLD ONLY**, and the world they described is gone. A
  `design-doc` packet's questions are registry-enforced structured
  metadata (`design-doc.toml`, `item_keys = ["anchor", "title",
  "proposal"]`); `review-design.js` returns before any docs-API call
  when the packet carries its own, and a mocked test routes
  `**/api/design/**` to 500 and asserts zero calls. So nothing anywhere
  parses a markdown heading for a question any more, and CLAUDE.md's
  §"Design docs" was corrected from "author your questions like this" to
  what is actually true.
- **Docs under `docs/design/` survive as human-read living references.**
  Nothing indexes them, no lint enforces their shape, and a docs-only
  change now implies no crate to compile (`infra/gate.sh`'s path map).
  The file is the residue of a settled discussion, not its venue.
- **`/it/design` became only what it claimed to be.** The page
  advertised itself as a lens over the `design-review` station's queue
  while rendering a table of files; its one use of the queue was a join
  on `subject.id` = doc path, which no `design-doc` packet can satisfy
  (its subject is the literal `boss-platform`), so the station's real
  packets rendered nowhere. It renders the queue now.

## Open findings — where two live decisions disagree

Flattening surfaced three places where a settled decision conflicts
with another that is also still in force. None is resolved here; a
fold is not the place to pick a winner. Each names what the code
does today, because that is what this record is obliged to state.

- **Group→role mapping for the IdP.** `docs/design/idm-kanidm.md`
  carries it as an invariant — Kanidm owns membership, the join to
  BOSS roles is registry data. The shipped OIDC runtime deliberately
  has no such registry and reads roles from the employee row,
  reasoning that a mapping table would be a second source of role
  truth. Both are defensible; only one can be the design. **Today
  the employee row wins**, and the doc's invariant is stale as
  written.
- **A queue: lens or node?** `docs/design/queue-visibility.md`
  states that a queue is a `WHERE` clause and *never* a reified
  structure, with "no queue tables" as an explicit non-goal. The
  station registry shipped as rows. The distinction it was reaching
  for survives one level down — the station row is real, its
  membership is a predicate — but the doc as written argues against
  the substrate's own nodes, and two of its five open questions have
  been answered by shipped code (the claim primitive, the
  assignments lens) while still presenting as live.
- **What `Self_` and `Team` compile to.** §Policy & auth records
  the owner-keyed scope predicates as load-bearing and deliberately
  not flattened; the packet model's stage 2 re-keys them onto
  queue-derived ownership. This one is a **scheduled** divergence
  rather than a disagreement — stage 2 is resolved and unscheduled —
  but until it lands, two sections of this document describe
  different futures for the same predicate, and that should be
  closed by a decision rather than by drift.
