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

### The Viable System Model, mapped — which BOSS concept plays which

Beer is the namesake and cybernetics is first among the three
lineages, but **VSM vocabulary does not appear in BOSS code**. It used
to, in exactly two crates — `boss-cybernetics` (whose own header read
"Per-VM Cybernetics coordinator (VSM S2/S3)") and
`boss-observability` — and both retired under design 8382bbb2 on
2026-09-23, each in its own car (below). The
mapping is recorded here so the correspondence outlives the code that
carried the words, because a reader arriving from Beer must be able to
find it and a reader arriving from the code must not have to learn a
second vocabulary to work (backlog 6872efd4). Retiring the only S2
implementation without writing this down is how a founding idea
quietly becomes decoration.

| VSM | BOSS | where it lives |
|---|---|---|
| **S1** — the operational units that do the work | **Actors executing Steps.** Humans and registered agents are the CPUs; nothing moves without one claiming a step and doing it | the claim door; `agents` registry |
| **S2** — coordination, damping oscillation between units | **Stations, and the bounds enforced at the claim.** A station is a data-defined priority queue that holds a packet until there is bandwidth or capability — which is S2's job stated in the network's own words | station rows; `agent_budget.rs` |
| **S3** — internal control, resource allocation, here-and-now | **The dispatcher, plus the registry rows that bound it.** Rules fire on `step.done.<kind>` and threshold events; the numbers they enforce are registry data, not code | `infra/dispatcher/rules/`; `agents.max_concurrent_runs` |
| **S3\*** — the audit channel that bypasses the line | **The audit log and sensor readings.** The log holds what *did* happen; readings are measurements *about* the system rather than work it performed | `audit_log`; `agent_runs`; sensors |

**S4 and S5 are deliberately unmapped.** Designs and the backlog do
S4-ish work and `boss-policy` does S5-ish work, but neither
correspondence is tight enough to assert, and a mapping asserted
loosely is the decoration this section exists to prevent.

**`boss-cybernetics` is retired as SUPERSEDED-BY, not deleted as
dead** (2026-09-23, backlog 467175e7, design 8382bbb2). Its design was
right and was overtaken: every responsibility it stated now has an
owner elsewhere, and the reason it goes is **coherence and one owner
per rule** — two implementations of one budget gate, one dispatch door
and one queue are two places for the same rule to drift. It is **not**
a line-count win, and recording it as one would misrepresent why it
went. What superseded what, so the record answers the question
directly:

| `boss-cybernetics` responsibility | superseded by |
|---|---|
| budget caps before dispatch | `boss-jobs/src/agent_budget.rs` — the claim door reserves the step's budget against the actor's hour, before the CAS |
| one-at-a-time dispatch | `boss dispatch` — one `agent-run` packet per step, bounded by `agents.max_concurrent_runs` at the claim |
| lifecycle telemetry | `agent_runs` plus the run packet's terminals |
| chaining on completion | the `agent-run` workflow's terminals and the dispatcher rules firing off them |
| the per-agent durable inbox | a **station** (923b6571), pulled through the same claim door by `boss dispatch --next --station` |

The inbox was **the real gap, not a translation**, and the ordering
this decision set was held: the crate did not leave the tree until the
station inbox (923b6571), this mapping (6872efd4) and the reclaim of
work a dead executor had claimed (a3397b01) had all landed, because
retiring the old mechanism before the new one is live is how a
capability is lost by accident. The inbox's caller is the operator's
session: David decided on 2026-09-23 that running `boss dispatch
--next` by hand counts as the runner having run (the session is the
CPU, 57c108c2), so no headless runner stands in for the old loop. Its
config (`infra/cybernetics/`) and its systemd unit left with it.

**`boss-observability` is retired as SUPERSEDED-BY, not deleted as
dead** (2026-09-23, backlog 467175e7, design 8382bbb2 — the second car,
which closed the item). It was the read side of the same design: a
cross-VM rollup of each `boss-cybernetics` coordinator, fanned out to
browsers. The reason is the same one, **coherence and one owner per
rule** — a second, parallel read of agent state beside the one the
claim door writes is two answers to one question. It is also
**not** a line-count win. What superseded what:

| `boss-observability` responsibility | superseded by |
|---|---|
| the cross-VM view (`/api/snapshot`, `/api/vms/*`) | a region of the IT world map (5082a08b) — the map reads the system's own regions (`/api/yard/regions`) rather than a per-VM rollup |
| per-agent runs, queues and costs (`/api/agents`, `/api/runs`, `/api/costs`) | `/api/agent-runs` and `/api/agent-runs/cost` on the jobs API, the record the claim door writes |
| its health alias (`/api/observability/health`) | each service's own `/api/<service>/health` |
| the `cybernetics.>` SSE fan-out | nothing, deliberately: its only publisher was `boss-cybernetics`, retired above |
| the playground's synthetic agents (`[demo_agents]`, the brewery's `seeds/demo_agents.toml`) | nothing, deliberately: they existed so a dashboard could show what oversight looks like before real agents ran, and real agents now run and are recorded |

Removed with it: the `observability` row (7880) in `boss-ports`, and so
its line in the container launcher and its block in the config
generator; the gateway's three routes and its proxy target; the
brewery's demo roster and its entry in the tenant contract; and its
bare-metal config and setup (`infra/observability/`).

**An unattended cadence will start work; until it lands, the session is
the only supply** (design `9e1de851`, David 2026-09-22; three questions
accepted as proposed, eligibility widened). Measured over 5.5 days and
132 trains: gate launches fell to zero for six hours every night, 07Z to
12Z, and five of the six longest gaps between trains ended between
13:55Z and 14:21Z — when the operator woke, not when a machine
recovered. Of 61 dispatcher rules, four manage an agent run's lifecycle
and none starts one; all 125 runs on record were filed by an operator
session. A skipped car becomes a total stop only when it is the last one
on the dock, which is the overnight condition, so the seven-hour stall of
2026-09-22 was downstream of supply rather than a separate fault.
Decided: a cadence rule dispatching ready steps (1) **up to a
concurrency starting at three**, half the observed session peak, raised
on a reading; (2) **until a per-night USD ceiling declared in the rule
row**, where it stops and files one packet saying so — never a silent
stop, which would look exactly like the failure it fixes; (3) **from any
queue the actor has policy to work** (David: "Any queue that the actor
has policy to work should be available to be worked overnight too");
(4) **with the bound read from the agents registry and enforced server
side**, never a second copy in the rule row (§9a). The claim door
already judges `agents.max_concurrent_runs` (backlog `57c108c2`); the
cadence itself is not built, which is why the paragraph above records
the session as the CPU.

*Algedonic* signals keep their Beer meaning throughout: rules firing
on threshold events, routed past the normal reporting line because
they are urgent.

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

**A person who writes to us is a contact, found by the address they
wrote from** (design `5548d85f`, 2026-09-21; David answered two
questions and delegated two). Measured: `accounts` has no email column,
and `account_facts` is a projection that answers what happened to an
account, never which account an address is — so `receive-a-sponsorship`'s
rule "customer id or email already on an account: use that account"
describes a lookup the schema cannot perform, and every sponsorship so
far has landed on `acct-anonymous-sponsor`. A support inbox makes the
gap load-bearing: an address that finds nobody makes every thread an
orphan and the second email from a customer a stranger. Decided:
(1) **contact first**, which neither offered option was — the PERSON is
primary, handles attach to the contact (`(kind, value) -> contact_id`,
unique on `(kind, value)`), a contact MAY point at an organisation when
known rather than guessed, and `accounts` is untouched and becomes the
organisation half; "contact" because it names the relationship, the word
a CRM reader already knows (David: "contact works"); (2) **handles are
additive and never reassigned by a protocol** — anyone can put any
address in a From header, so letting inbound mail repoint a handle makes
takeover a matter of sending an email; a wrong handle is corrected by a
human act with a record; (3) the anonymous-counterparty rule is
**dissolved, not answered** — mail always carries an address, so a real
message gets a contact keyed on it; (4) **the tables land as their own
car, then `receive-a-sponsorship`'s reconciliation is repointed as its
own small car** right after, because a sponsor is a person who may
belong to an organisation and that repoint re-models a live revenue
path. Identification comes after triage, never at receipt, so spam
provisions nothing. Not yet built.

**The employee Class drawer stays one drawer, told apart by axis; only
the departments move out, onto the tenant's own roster** (design
`3dff7577`, David 2026-09-23, all three questions accepted as proposed;
it amends the four-kind split design `32f18167` signed off on
2026-09-19, carried by backlog `a45ab09d`). Measured live that day: the
drawer holds 22 `employee` Classes on four axes (role 7, department 9,
status 3, employment_type 3), and every row already names its axis in
`member_attribute` — the explicit axis the split was weighing. What was
broken was the readers, which asked only whether `(employee, code)`
existed, so `role=terminated` or `department=platform-admin` passed.
Decided: (1) **role, status and employment_type stay `employee`
Classes** told apart by `member_attribute` — four subject kinds would
register three with no Subjects behind them and contradict
roles-are-Classes-of-employees, and the drift the split was meant to
stop is refused by reading the axis instead; (2) **the department
roster is the tenant's org chart**: the 13 catalog-derived rows of the
`department` subject kind (migration `20260919181324`) become the
product default a tenant starts from, and a tenant adds and retires its
own — Algedonic's engineering, product, hosting and operations exist
today only in the drawer, with two live actors on them — which needs a
tenant write door and a publish path before the validators move to
department Subjects and the drawer's department Classes retire, moved
rather than copied, with the two actors reassigned through the API so
the log records it; (3) **a role Class's `metadata.department`
retires with them** — a role does not belong to a department, the
person holding it does. Landed: the axis readers — boss-people's
employee checks (`a45ab09d`), then the agents batch door, account-team
roles and `GET /api/classes?member_attribute=` (`ab1e6ff8`). Not built:
the department write door, the publish path and the move; the
`departments` table still has no write door.

**The People roster holds people** (design `7aa2d1c5`, David
2026-09-23; answers backlog `6a123f1f`, gap 12 of the `/ux/people`
page audit). Measured: the page's "active employees" headcount was 2 —
the founder, and `emp-audit`, the "System Audit Account" the operator
baseline (`boss-people/src/operator_baseline.rs`) seeds as an employee
row — while every registered agent, which claims steps and is
capability-checked at the claim, was absent. The proposal was to show
agents on the roster as their own kind with a split headcount; David
answered instead: "People should just be people. Let's actually move
the system-audit account to the same locale as agent and system
accounts." So the employee roster is humans only, agents stay in the
agents registry rather than joining the headcount, and `emp-audit`
leaves the employee table for wherever agents and system accounts are
held — an actor being a CPU in the same machine does not make it a
person on the org chart. Not built: the audit account is still seeded
as an employee.

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
pin to the version they opened under unless an actor explicitly moves
them (the re-pin door, below); creation is blocked against
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

**The step PUT will refuse a metadata body that drops a stored key; it
will not refuse every metadata body** (design `baf738b7`, David
2026-09-23, accepted as proposed; answers backlog `e39a9d2a`). The item
asked the PUT to refuse any `metadata` and send every caller to the
merge door (`PATCH …/steps/{id}/metadata`). Measured before any code:
about 100 writers send metadata with the status flip, because the
checks that run at done read it, so that rule was five cars and made
completion two writes that are not atomic. Decided: **refuse only a
body that omits a key the stored step holds** — clearing by omission is
the actual data loss, and a stale read that would drop a concurrently
added key is caught with it — route that caller to the merge door with
an explicit `null`, and delete the three keys `update_step` carries by
hand (`authority_role`, `human_only`, `agent_run`); the
refuse-every-body plan stays unbuilt unless a two-write completion is
wanted for its own sake. **The premise under the one-car estimate was
false, and the rule stands with a new order** (correction recorded on
`e39a9d2a` the same afternoon): the registry materializes
`metadata_defaults` and the audience keys into every step, so ANY PUT
built without a prior read omits stored keys — the gate runner's
verdict report and `boss car open` / park among them — and landing the
refusal first would have stopped every gate and every park. The order
is now (1) `boss hold` / `boss release` through the merge door —
landed (#586); (2..n) every fresh-metadata writer moved to
read-merge-write or the merge door, the gate runner and `car.rs` first,
each converged on its host before the next; (last) the server refusal,
which closes the item. Until it lands, the PATCH semantics above hold,
wipe of unmentioned keys included.

**A Job carries the instant it was admitted, not only the day**
(design `f2cdff23`, David 2026-09-20, all three questions accepted as
proposed; landed as backlog `6c2eba00`). `opened_on` is a date, and
every surface that asks how long something has waited — the
ops-runner's `oldest_wait_s`, dock wait and gate duration on the
region map, the overdue alarms, the silence sweep that settles a dead
agent run — needs the instant, which was `metadata.opened_at`: a
convention the doors followed and a raw `POST` did not, and a missing
stamp read as decades old rather than unknown. Decided: (1)
**`opened_at` is a column**, server-stamped at admission and kept out
of every UPDATE the way `partition` is; (2) **the back-fill is a
projection of the log** — each packet's own `jobs.job.created` event,
the derivation the rebuilder also applies — and a packet whose create
event the log does not hold keeps `NULL`, because an instant nobody
observed is worse than none; (3) **admission grows no create-time
check** for declared-required fields as part of this — validators
still run at done, and a server-stamped field needs none. The metadata
stamp is still written for the readers already on it.

**A packet stays on its admission version unless an actor moves it, and
the move is on the record** (design `7cf202a9`, David 2026-09-23, all
five questions accepted as proposed; answers backlog `4347a1af`). The
item said the re-pin machinery had no door; measured, it had one —
`POST /api/jobs/{id}/convert`, landed in train #150 on 2026-08-30,
gated on the ordinary job-write permission, answering 409 with the
obstacles when `convertibility_for_packet` is not automatic and
otherwise updating `jobs.workflow_version` under a plain `job.updated`.
No CLI verb calls it, no test covers it, and no open packet carries a
re-pin's signature. It also moves less than it reports: a step's
`procedure` is projected into the step row at materialisation, so
converting page-audit `c0d2caf0` from v1 to v3 would answer
`converted: true` while its `measure` and `file` steps kept v1's text —
the only thing that changed between those versions — and a target that
inserts a step (backlog-item v1 → v7 adds `draft-design`) would walk the
packet to a step no row holds. 112 of 296 open packets were pinned
behind their kind's active version that evening. Decided: (1) **the
guarantee, restated so the code can hold it** — a packet stays on its
admission version unless an actor explicitly moves it; a move is
refused when it would retroactively demand evidence or strand a step;
the move is on the record — with `/convert` the one door and a CLI twin,
`boss job convert <packet> [--to vN] [--dry-run]`, whose dry run
returns the verdict without writing; (2) **a re-pin re-projects the
target's step defaults, procedure included, onto every step not yet
completed, materialises any step the target inserts, and leaves
completed steps with the text they ran under** — and until that lands
the door refuses any move whose target changes a pending step's
procedure or inserts a step; (3) **a `job.repinned` event** (from, to,
actor, steps re-projected, steps inserted) plus a `repins` list appended
to the packet's metadata, so admitted-at-v3 and moved-to-v3 read
differently without diffing `job.updated` payloads; (4) **authority
narrows to `platform-admin`**, the role that owns the Workflow registry,
from any job writer; (5) **never automatic, never on publish** — a
cohort move (the 47 page-audits on v1) is the per-packet door run in a
loop by an operator after a dry run, each packet its own event,
revisited once moves have been counted. Not yet built: on main the
route still admits any job writer, refuses no changed procedure and
emits a plain `job.updated`, so the guarantee above is decided and the
door does not yet hold it.

The brewery's `wholesale-keg-order` is the worked example of
agent-gated fulfillment: an `availability-gate` reads finished-goods
for the order's lines and forks fulfill|backorder — an order the cooler
can't cover exits to a terminal `backordered` outcome instead of
marching through pick → ship → bill against stock that isn't there —
then a human `pull-and-stage` pick precedes delivery and billing. The
gate is the release valve that bounds open WIP the way human
stock-judgment does in a real brewery; finished-goods / COGS draw on the
billing line items.

**Support's `receive-a-message` is the first protocol of a department
that is not IT** (design `bffc0aba`, David 2026-09-21, all four
questions accepted as proposed; the first of the six first protocols in
the order `3613f0af` set). Measured that day: 116 of 126 open
backlog-items named no department, and about five were genuinely non-IT.
The loop adds two ends to standing machinery — the sensor cadence, one
packet per reading, `sensor_unreadable` filing an urgent deduped packet
for a source that refuses — namely a mail source adapter idempotent by
`Message-ID` and the protocol row. The mailbox is a dedicated Agent B
account on Proton Mail Business, `support@algedonic.dev` routed to it
with no forward to a human, and Proton has no plain IMAP, so inbound
needs Proton Bridge. Decided: (1) **Bridge runs on boss-gcp**, installed
by its converge with the keyring login a one-time human act — and before
building, establish whether a logged-out Bridge can answer politely with
an EMPTY mailbox; if it can, the adapter asserts reachability separately
from message count, or a dead daemon reads as a quiet inbox; (2) **the
body lives on the packet**, and the inbox message carries a subject and
a pointer until replies land; (3) v1's terminal was **answered in BOSS**,
superseded the same day by the send loop below; (4) **a spam terminal
reached by a disposition at the first step**, and no automatic
classifier on day one, because a wrong auto-close loses a real customer.
Landed: a sensor may carry a `selector` naming one stream within its
source, so two inboxes on one mailbox open two protocols
(`20260921034443-a-sensor-selects-within-its-source.sql`). The adapter,
the Bridge host and the protocol row are not built; the tenant's
`support` module is on (design `1054c099`).

**Agent B sends nothing without the founder's passkey on the words**
(design `3a89774d`, David 2026-09-21: "I do not want any outbound email
sent by this protocol, or more broadly Agent B, without my sign off by
pressing a UI button on a gating step"). No new control is needed:
`assurance_required = "presence"` on a `sign-off` is enforced with no
bypass, and the stamp is re-checked against the step's CURRENT shape, so
an edit after approving voids the approval. The protocol is `received`
→ `triage` (real or spam) → `draft` (agent) → `approve` (sign-off,
presence; the founder reads and edits HERE, so what he signs is what he
ended with, and there is no revise loop) → `sent` (agent; apart from
`approve` so a failed send never reads as a withheld approval) →
`answered` | `spam`. Decided: (1) **the transport does not refuse a
send on its own** — the requirement is scoped to this protocol and the
ones Agent B will help run, the way the conductor runs the yard, so a
code path that never enters a step is not closed by it, knowingly;
(2) **presence on every outbound** for this protocol ("passkey entry is
as fast as touching my fingerprint"); (3) **the agent drafts**, fetching
from the record to support the draft rather than writing words alone;
(4) **a failed send leaves `sent` open and loud**, the sensor's shape,
and the approval stays valid because the words did not change — the
packet must never reach `answered` when nothing left the building. Not
yet built; the send needs an SMTP submission transport behind
`MailTransport`, beside the log and HTTP-API ones.

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

**A relationship between packets is a declared fact, not a trigger's
precondition** (design `c0d2787a`, David 2026-09-21, all five questions
accepted as proposed). Measured that afternoon: six packets filed in one
session recorded how they related four ways — `prerequisite_for`,
`prerequisite`, `prerequisite_of`, `related` — none declared in
`job_edges`, so none resolved, rendered in a Links panel, was ref-checked
or could be queried; meanwhile the one declared spelling, `answers`, was
REFUSED by `boss design --answers` because its target was a build
disposition with no `design-review` to complete. The system permitted the
weak spelling of the fact and forbade the strong one; the verb's guard was
doing model work. Decided: (1) **a small typed set of relation edges**,
declared like the others and grown on evidence, because the four
spellings meant different things; (2) **a decision edge records and says
what it causes** — "completes nothing" is printed, never a refusal,
since declining a true fact loses more than a side effect that does not
fire; (3) **stored once on the source, resolved both ways by readers** —
a reverse copy is §9a drift; (4) **a job id in an undeclared metadata
field is counted**, a warning first, as the measure of whether the habit
changed; (5) **declarations stay in migrations**, because an edge changes
what the write path refuses, which is schema. Landed: three
behaviourless `'*'` relations — `occasioned_by`, `duplicate_of`,
`supersedes` (`20260921162511-a-packet-relation-is-declared.sql`),
`on_missing = abort`; the proposed prerequisite relation is deliberately
absent, because `waiting_on` already is one and the dispatcher wakes its
waiter when the blocker closes — two of the four freeform spellings meant
exactly that, and would never have woken anyone. `boss design` records
the edge and prints what it will not cause, and `boss census` reports
undeclared job-id fields without raising.

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

**A platform station is declared in `infra/platform/stations/`, a
step plugin's row in `infra/platform/step-plugins/`, a cadence rule in
`infra/platform/cadence/`, the delivery policy in
`infra/platform/delivery-policy/`, and a migration newer than
20260918112134 declares schema only** (2026-09-18, backlog 393d3234,
consolidation H4, four registries in four cars). Measured on that day,
seven migrations were the only place a platform station existed, seven
the only place a step plugin's row did, nine the only place a cadence
rule did (six of the nine re-versioning one integer, the boarding
threshold, and one of those a silent no-op), and two the only place the
delivery policy did (the second a policy edit that rode the pipeline it
was re-tuning), and a
migration is the wrong home for a registry row: it runs once, a fresh
instance cannot re-declare the row without replaying history, nothing
drift-checks it against the live row, and every edit is a contended
timestamped file. The dispatcher rules above and the Workflow bundle
before them already answered this; stations and step plugins take the
same shape: one `<name>.toml` per row carrying every column, published
insert-if-missing by (name, version) at every start by the same seed
the Workflow bundle rides (each directory is found beside
`--seed-path`, so no launcher changed), through one decision table
(`boss_jobs::bundle_seed`) rather than one per registry. A bundle row
that differs from the live active row of the same (name, version) is a
refusal naming the field — a version bump is the edit path — and
`infra/lint/migrations-declare-schema-only.sh` holds the cutover stamp
once, with the registry tables listed beside it. The historical
inserts stay as history; each bundle is pinned equal to the rows they
produce, and proven able to rebuild an emptied table alone. A step
plugin's row lives under `infra/platform/` rather than beside its JS in
`infra/step-plugins/` because that is what the launchers already read:
the image copies `infra/platform` whole and the JS reaches the cluster
as a ConfigMap built from `*.js`, so a row beside the JS would have
needed a Dockerfile COPY, a seed flag and an init.sh edit to be found
at all. `infra/lint/step-plugin-bundle-exists.sh` holds the row to its
JS across the two directories. The cadence bundle differs in one
posture the table's design demands: `cadence_rules` is live-editable
protocol data (measure, experiment, update — without a deploy), so the
bundle is the declared BASELINE, not a lock — a rule an operator
re-versions live is reported as ahead of its file and left alone, a
rule the operator retired stays retired, and only the one refusal
every bundle has (a file edited at the same (name, version) the live
row holds) stops a boot. The conductor's loop reads the live table
over `/api/cadence/rules` and did not change. The bundle carries three
of the four rules the migrations seed: `protocol-retro-daily` is
retired by decision the same day (the weekly department retro opens
IT's retro through a dispatcher clock rule), and the equality pin
compares the bundle to the migrations' active rows minus a
`RETIRED_BY_DECISION` list that names it and why — a bundle must not
carry a row the operator is retiring. The delivery policy is the
`workflows` shape at one row: a train pins the version it departed
under, so a bump in the bundle changes the NEXT boarding's rules and
never a train in flight. Its compiled fallback in `boss-cli` stays —
the conductor runs outside the cluster and must board when the
registry is dark — and is now held equal to the bundle with no
database (`the_bundle_equals_the_compiled_fallback`), closing a
triangle the migration-side pin had left open on one edge.

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

**The experiments program starts at a named threshold, not now**
(design `d8771dec`, David 2026-09-23, accepted as proposed; answers
backlog `8af3aed1`). Measured against the system of record:
`kind=protocol-experiment` answers 0 — the "iterated live" history in
the workflow's header, and the experiment it cites, belong to the
retired second stack; opening one needs no code, but a malformed split
fails silently, so an experiment can sit open splitting nothing; and a
before-and-after reading cannot credit a protocol change —
`backlog-item` went v1→v7 in seven days with the same ten steps while
its median cycle days swung 13×, so the period drives the number and
only a concurrent split can tell. Decided: **experimentation begins at
the first draft workflow version, on a kind with 20+ terminals a day,
that adds, removes or reorders a step**; its author opens it as a Tier
2 split (`boss-jobs/src/experiments.rs`) instead of publishing, and
that experiment's first car is a verb that refuses a malformed split;
the workflow header's second-stack history is corrected. None of it is
built: no experiment has run on this instance, the refusing verb does
not exist, and `infra/platform/workflows/protocol-experiment.toml`
still says real experiments ran through it.

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

**The founder's watch list is his own assigned queue, aged, on the
surface he already reads** (design `5877860d`, David 2026-09-21, all
four questions accepted as proposed; built as backlog `3bc896be`).
Measured: David routed a drive purchase (`b2d5b546`) to design review
because `/it/design` was the one surface that showed what was waiting
on HIM; it sat there with no `design_id`, at a step nothing could
complete, while the work it described happened and was recorded on it —
visible and inert, which looks handled. Decided: (1) **a per-actor
queue** — what `boss orient`'s MY WORK already is for an agent — rather
than a `watched_by` flag (one more thing to remember at the worst
moment) or a derived "needs him" list (the receiving queue under another
name, and the right SECOND step once measured); (2) **age is not
optional**: every row carries how long it has waited, oldest first, and
crossing a declared threshold changes how the row reads, because an
accurate signal that never changes stops being read (learning
`84f8f9b9`: the garage read TROUBLED identically for nine hours);
(3) **no new page** — it renders where he already looks; (4) **assigned
steps first, measured second** — what he routes to design without a
design is the evidence for any wider category. Landed on My Day (`/`):
"Yours to decide" orders actionable verdicts oldest `opened_on` first
and bands each row by the receiving yard's own age thresholds (past 3
days aging, past 14 stale) — one definition of old, not a second copy.
The derived list waits on that measurement.

**Stuck is a packet past its own place's bound whose next move is ours,
counted per third and never summed** (design `cf820810`, David
2026-09-23, all seven questions accepted as proposed; answers backlog
`4142d821`, two of the figures for the HUD frame `c1253e50`). Not a
composite score: each number keeps its name and its denominator and
clicks back to the region that owns it. Measured on `/api/yard/regions`
that evening: the delivery half already existed — every region carries
a `trend`, this window against the previous with sample counts, and
arrivals read 30 a day against 29 — and the shed already told stuck
from waiting (`PROOF_STALE_HOURS = 24`; `NOT_YET_STARVED_HOURS = 72`,
measured over 1,812 ops-requests; `waits_on` with `car::starved`;
"troubled means ours", `3881f5c9`). The gap was the incident's
population: three cars could not board for 9.5 hours while the dock
said the boarding depth was met, because only the conductor reads the
declared `boards_after` edge (`boards_after_outcome` in
`boss-cli/src/train/boarding.rs`) and the dock region counts parked
cars against `threshold_met` without asking whether they can board.
Decided: (1) **stuck is past the declared bound AND the next move is
ours**; past the bound but waiting on a declared outside owner — the
world, an event, a named human's act — is **waiting**, shown beside
stuck and never summed into it, the shed's rule extended to every
region; (2) **the population is the union of five that already have
owners**, with no new measure: receiving past the 3-day triage band,
stations not draining or over WIP, hand-held cars and held greens
(garage), parked cars whose `boards_after` predecessor has not landed
(dock), and landed cars past 24 hours whose move is ours (shed), each
count clicking through to its region; (3) **counted per third** — queue
management, actors building, delivery — never across them, since one
total would read 271 receiving packets plus one shed car and hide which
half moved; (4) **the paired delivery number is arrivals per day
against the previous window**, read from the arrivals trend, while dock
wait and time at CI stay on their own regions; (5) **unknown is not
zero** — a station whose flow cube is blind contributes `?` and its
third reads `≥ n + ?` (one station, 170 standing, read that way); (6)
**one `stuck` block on `GET /api/yard/regions`** — per third: stuck,
waiting, oldest age, owning regions — read by the HUD and `boss orient`
and recomputed by no client; (7) **the dock is repaired first, as its
own car**: `boards_after_outcome` moves into `boss_jobs::car` so the
dock and the conductor share one reading, and the dock says "N parked,
M cannot board" and stays busy rather than claiming a train is due.
Built (7), 2026-09-24: `boss_jobs::car::boards_after_outcome` is the
one judgement, the regions read fetches each parked car's declared
predecessor by id, and the dock says "N parked, M cannot board (waiting
behind X)" — busy, or troubled when the edge can never clear. Not yet
built: the regions read carries no `stuck` block (6), and the HUD and
`boss orient` do not read one.

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
control of the rows. **One exception to append-only, stated once
(backlog b5f21e82, 2026-09-18): seed residue is deleted, not kept.** The
thirty-one pre-collapse migrations are applied history and still insert
sixty-four names on every fresh database; thirty-five of them no product
file authors any longer (the brewery's reactors moved to the tenant's
`seeds/rules.toml`, the design-doc sweep's retired with it), so the boot
seed retired them on every new instance and a tenant then had to take
each name over at v(n+1), reading `registry ahead, left alone` on every
publish after. Append-only protects decisions — a version someone
published, a rule someone switched off. A row a migration wrote and a
seed retired minutes later, under a name no file ever authored again, is
not a decision anyone made; `20260918022108-seed-residue-is-not-a-retirement.sql`
removes those rows (`source IS NULL`, no tenant row under the name — a
tenant that already took a name over keeps the history it was judged
against), and the list is pinned to its derivation by a test. Live
retirement is still a status flip, and `no-migration-writes-a-dispatcher-rule`
still refuses a migration that inserts or updates a rule; it passes a
DELETE, which cannot open a second home.
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

**A product rule is never born in the SPA** (design `ff1c3615`, David
2026-09-23, option (b) as proposed; answers backlog `7d9df2fe`, found
by the `/it/registry/rules` page audit). The page's **+ New rule**
created a rule with no `source` — a product rule — which the boot seed
above retires because no file names it, so it fired until the next
restart and then vanished with only a name in a boot log; on the live
instance 62 of 65 active rules matched the 62 files exactly and the
other three were `tenant:algedonic`. A warning at the button, option
(a), was declined because it keeps a control whose product is temporary
by design — the answer-instead-of-error shape. Landed (#586): the
button opens a page that creates nothing and names the two durable
paths, a file under `infra/dispatcher/rules/` carried by a car or a
tenant's `seeds/rules.toml` published into the instance
(`NewRuleGuide.svelte`); and the draft door refuses a product draft
under a name no authored file declares, reading the names with the
seed's own parser so the door and the seed cannot disagree, and
refusing every product draft when the authored directory cannot be
read. A new version of a rule a file does name is still accepted,
since the seed never walks a live version back — so "live authoring is
untouched" above now holds for rules that exist, not for new ones.

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

**A tenant cannot yet register a handler, and the direction to test is
out-of-process** (design `153d49f7`, David 2026-09-23, accepted as
proposed; answers backlog `ec40e269`). Measured (builder run
`950597e7`, on `642c0171`): of 51 handlers, 29 are invoked by product
rules, 21 only by the example tenants' seeds (13 brewery-only, 8 shared
with the used-device shop) and one, `jobs.complete_step_matching`, only
by Algedonic's own tenant rule. All of them live in
`boss-dispatcher-handlers` and ONE binary registers them, so a
tenant's published rules — and `boss tenant check` — resolve against a
roster compiled into every deployment: the brewery's keg-deposit,
excise-accrual and packaging handlers ship everywhere, and a hosted
tenant can use only what the platform compiled in. Where tenant code
runs is the hosting safety levels by crate tier (data-only / tenant /
modules / full, 2026-09-18). Of three sketches — (a) a tenant links its
own dispatcher binary, in-process but a binary per tenant, which a
data-only tenant cannot ship; (b) out-of-process handlers, a rule
naming one the tenant's engine serves over HTTP (`POST
<engine>/handlers/<name>` with the event, returning its writes); (c)
declarative handlers as data, a templated PUT/POST body, which cannot
cover real logic such as packaging allocation — **(b) is spiked
first**, since it runs tenant code in its own process under its own
identity and policy, with idempotence and provenance to be held across
the hop, and the same spike measures how many of the 21 reduce to (c).
It is a direction to test, not a plan: no spike has run. Landed the
same day: the cascade map of what each handler emits moved out of core
into `boss-dispatcher-handlers/src/cascade.rs`, beside the code it
describes, with core keeping only the type; the handlers themselves
have nowhere else to be registered yet.

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

**A correction names what it corrects, and every reader is handed it**
(design `4105b020`, David 2026-09-23, all six questions accepted as
proposed; answers backlog `56727f95`). A completed step is frozen, and
correctly; a damaged sentence on one could only be corrected BESIDE
it — backlog-item `f3e091f0`'s triage evidence still reads "Ordering
trap confirmed:  is required of every rule", its fix under a job
metadata key, `evidence_correction`, that nothing reads. Measured across
all 15,039 jobs: about 132 corrections under 25 key names — one batch
key on 107 jobs, 24 hand-invented keys on 25 more, and one correction of
a correction joined to it only by its name (gate-run `9a2576fb`,
`verdict_correction` then `verdict_correction_withdrawn`). The server
taught the habit: the terminal-freeze 409 in `http/steps.rs` sends a
correcting author to "the parent job's metadata" and names no key.
`reproof` and `regate_receipt` were already the working shape — a
reserved key plus a reader obliged to look — without the generality.
Decided: (1) **first-class, not a convention readers learn to find**,
because 25 key names are the measurement of a convention authors do
not keep; (2) **one reserved append-only list, `corrections`, in job
metadata**, entries `{step, field, reads, should_read, why, by, at}`,
written only through `POST /api/jobs/{id}/steps/{step_id}/corrections`
and a `boss correct` verb with `-file` twins; the door refuses an open
step, a field the step does not hold, and a `reads` excerpt absent from
the stored text, emits `job.step.corrected`, and the generic metadata
PATCH refuses the key — the step and its audit event are never touched;
(3) **the reader is handed it, not asked to search**: the job GET
attaches each step's entries as `step.corrections`, one shared web
component marks the field, `boss brief` prints each correction under
the field it corrects, and the original is never replaced in the
render — both are shown; (4) **a correction is withdrawn only by
appending** an entry carrying `withdraws: <index>`; (5) **separate from,
and lighter than, `correct-the-record`**, which is for a published
claim whose premise changed decisions, holds 0 packets (control:
`kind=backlog-item` answered 692 on the same connection), and whose
`applied` step will use this door when the false claim sits in a
completed step — a damaged sentence needs a signature, not a review;
(6) **no backfill**, since rewriting the ~132 would mean guessing a step
and field for each; the 409 hint names the new door instead, and two
weeks after it ships the scan is re-run with a target of zero new
ad-hoc keys. `reproof` and `regate_receipt` stay as they are: they
supersede by precedence, and each has its reader. Not yet built: the
409 still sends an author to free-form job metadata.

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

**The chart of accounts is tenant data; the starter chart is the
brewery's.** (Backlog `41af5195`; design `18cf4272`, David 2026-09-17;
amended by `718ac982` the same day.) 40-ledger.sql seeds the brewery's
chart, and since `718ac982` that chart is declared in
`examples/brewery/seeds/chart_of_accounts.toml` and treated as example
residue everywhere else: a fresh company instance evicts it at first
boot and an existing one through `retire-example-reference-rows`
(§OSS posture), so a company's books start empty and hold only what
its tenant declares. A tenant
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

**Algedonic's books read the ledger, and its bills get a protocol**
(design `72ccb3b2`, David 2026-09-23, both questions accepted as
proposed; from the `/ux/finance` page audit `3f964c57`). Finance is on
for Algedonic (module decision `1054c099`). Measured: the ledger held
one entry, a $1.00 sponsorship still in Cash in Transit awaiting its
payout; revenue reaches the ledger only through packets
(`receive-a-sponsorship`, `receive-a-payout`), yet the finance page's
headline, AR aging, gross margin and Invoices tab read commerce
invoices, which nothing writes, so the page could never agree with the
ledger's income statement; and payables had no protocol — the ledger's
bill endpoints held 0 bills, while PO Approvals and AP aging read
inventory, which the company does not run. Decided: (1) **payables are
a finance protocol, `receive-a-bill`**, on the ledger's own bill
endpoints — one packet per invoice (cloud, domains, Cloudflare,
software subscriptions, Stripe fees) posting to AP and then to cash on
payment — and the inventory-backed PO Approvals and AP aging come off
the page for a tenant without the warehouse module; (2) **the headline
reads the ledger**, where packets already post revenue, because one
source cannot disagree with itself; commerce invoices stay for tenants
that sell goods, and hosting revenue (`8db1d6a6`) posts to the ledger
the way sponsorships do. Not built: `receive-a-bill` exists in neither
this tree nor the tenant's, and the page still reads commerce and
inventory.

**The tax regime is tenant data; the migration's kinds and rates are
the brewery's.** (Backlog `7f163e58`; design `e187198f`, 2026-09-18.)
40-ledger.sql seeds `tax_kinds` (five filing kinds, each FK-pinned to a
GL account: 2150, 2300, 2310, 2320, 6500) and `sales_tax_rate_by_state`
(27 states) on every instance, and nothing else could declare a kind —
the accrual door refuses an unregistered one with "register it in
tax_kinds first". Measured 2026-09-17: because a tax kind naming an
account keeps it, the eviction above had to leave those five accounts
under the brewery's names on every real instance, for ever. Both
tables are now `seeds/tax.toml` in the tenant contract — `[[tax_kind]]`
(kind, liability_account, expense_account?, derive_basis?) and
`[[sales_tax_rate]]` (state, jurisdiction, rate_bps) — published
through `POST /api/ledger/tax/batch` after the chart, insert-if-absent
by kind and by state, one `ledger.tax_kind.declared` /
`ledger.sales_tax_rate.declared` fact per inserted row, a held row that
differs named in the publish line and never overwritten. The
migration's rows are the brewery's seed (`examples/brewery/seeds/tax.toml`)
and example residue everywhere else: the derivation reads the kinds
and states as candidates, a kind is kept only when a `tax_filings` row
names it, and **a kind that is itself leaving keeps nothing** — so the
five accounts are candidates like the rest of the brewery's chart, and
on a bare schema every account goes. `boss tenant check` holds the
FK in the directory: a kind naming an account the tenant's
`seeds/chart_of_accounts.toml` does not declare is INVALID naming the
kind and the code. An empty file is valid and is what `init` writes:
Algedonic has nothing to file until its first sellable SKU (design
`18cf4272`), and until then the accrual door refuses every kind on its
instance, which is the truth. The used-device shop declares no chart
and so no tax; a tenant that files nothing declares nothing.

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
differs. **The projection runs live as well as at rebuild** (backlog
5621d166, 2026-09-17): `boss-ledger-api` binds a durable consumer
(`ledger-facts`) on the platform event stream over the subject families
the rule set names, re-reads the registry every 60 s, and for each
delivered event runs the SAME `project_event` and `post_fact_in_tx` the
rebuild and the domain writers run — one definition of the fact, one of
the entry. Idempotence makes the two halves one system: the fact's id
is UUIDv5 over `(kind, source_table, source_id)` and the entry is unique
on `(fact_id, rule_version_id)`, so a redelivery writes nothing twice
and a rebuild after a live projection reproduces the same rows; the
live path posts only the fact it inserted, because a pre-existing fact
was handled by whoever wrote it — including an operator's supersede,
which drops the entry that a re-post would resurrect. A
projection that fails deterministically (a missing field, a locked
period, no posting rule) is logged with its event id and acknowledged;
only a storage failure asks for redelivery. The nightly
`maintenance-ledger-replay` chore — `boss-ledger-replay-check --deep`,
a read-only replay comparison — remains the reconciliation that names
what the live path missed. Events on families the stream does not
ingest (`products.*`, whose facts boss-products writes in-tx) are
named at start rather than silently dead air.

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

**A run reported as a bare total is priced at a declared blend, and
says so** (design `91a9bfe7`, David 2026-09-20: "The blended rate for
the cost of runs sounds fine"; all three questions accepted as
proposed). Measured: 2 of 85 recorded runs carried any cost, because
only an input/output split was priced and the harness that runs a
dispatched builder reports one total — so both budget desks enforced
their caps against about a fiftieth of the spend. Decided: (1) **the
assumed input share is a per-model column on `agent_rate_card`**
(`blended_input_share_ppm`), declared rather than hardcoded, NULL
meaning undeclared and therefore unpriced; one row carries it,
`opus-5[1m]` at 875,000 ppm, the only model with measured splits behind
it; (2) **every figure names its basis** — `split` or `blended`,
derived from the run's own token shape rather than stored — carried
into every roll-up, where a bucket holding one blended run reports
itself blended, the same rule that already makes a bucket with an
unreported run answer `None`; a measured split always wins; (3) **the
83 runs recorded before it stay unpriced** — marking them blended would
record an assurance nobody has, so the series starts where the pricing
does.

**A run is priced from what it consumed, and a budget is a reading,
not a limit** (backlog `e6b2066f`, David 2026-09-23: "the goal is the
PLUMBING to understand costs … we do NOT want budgets to limit
building"). Measured: the bare total the blend above priced was the
harness's `subagent_tokens` — the run's FINAL context size, within 1%
of its last turn on 62 of 68 runs — while its summed per-turn tokens
were a median 48x larger, 96.8% cache reads; real spend was a median
4.9x the recorded figure. Decided: (1) `boss dispatch --report` meters
a run from its subagent transcript — the four counts (uncached input,
cache write, cache read, output) of every turn's `message.usage` — and
the typed count rides `detail` beside it; (2) `agent_rate_card` prices
cache reads and writes as data (0.1x and 1.25x input, the 5-minute
write rate, so a 1-hour write is a floor), basis `metered`; (3) the
`opus-5[1m]` blend is retired, so an unmetered total is unpriced rather
than about five times low; (4) an over-cap claim is admitted and
`agents.claim.over_budget` rides the log beside it, and an over-cap run
is recorded with its `budget` reading `deny` — neither refuses.

**A destructive change is approved by the founder's passkey over a
rendered plan and executed by the machine** (design `17835005`, David
2026-09-21, all five questions accepted as proposed). The occasion was
commissioning the forge's new disk: a second NVMe renumbered the devices,
and the drive that looked new by number was carrying the system —
approving "format nvme0n1" would have approved destroying it. So **the
guard matters more than the approval**: bounded verb, machine-checked
preconditions, plan, passkey-bound approval, recorded execution, the
passkey last of the five. Decided: (1) **the signature binds a rendered
plan** — the target resolved by-id, preconditions evaluated live on the
host — never a verb call; (2) **a runner never trusts an approved
field**: the system of record issues a short-lived single-use
capability, host-side verification of the enrolled key being later
hardening, and the weakness stated plainly — this trusts the SoR;
(3) **eligible is a declared verb whose dangerous outcome machine-checked
preconditions exclude**, however destructive; a verb that needs someone
to eyeball which thing is meant is not eligible behind any approval —
the line moves from who acts to who decides, and stays sharp; (4) **an
approval is single-use, minutes long, consumed when the runner begins
and voided by drift**; (5) `commission-a-disk` is the first verb.
Landed: read-only planners `plan-a-disk-commission` and
`plan-a-tenant-merge`, which evaluate exactly the write path's
preconditions and refuse (exit 78) what cannot run; the mutating
`commission-a-disk` and `merge-tenant-main`, each declaring
`requires_approval` and re-checking immediately before writing; and
ops-request's `approve` step, the first presence-assured step in the
system — the plan rides in that step's metadata, so the WebAuthn
assertion over its shape hash signs the plan's bytes with no second hash
to keep in agreement, and an edit after the ceremony voids the stamp.
The capability a runner would verify is not built, so the ops-runner
refuses every `requires_approval` verb: the gate exists before the
power.

**Presence authorises a break-glass enrolment, and the bootstrap token
retires** (design `03451237`, David 2026-09-22, all four questions
accepted as proposed). Independence is a property of the ASSERT path —
hardware key plus PIN opens the door whatever else is down — while
enrolment is an administrative act done while things work: the founder's
software passkey may AUTHORISE administering the emergency credential and
may never BE it, and the refusal of synced and unattested authenticators
stays exactly as strict. Today `enroll_gate` admits a break-glass session
or the bootstrap token while nothing is enrolled, and that
zero-credentials condition, which exists only to bound a shared secret,
forces a repair (two keys bound to a retired relying-party id,
`1c4c100a`) to empty the store first and open a window with no emergency
door at all. Decided: (1) **one authorisation packet per key**, its step
naming label, relying-party id and deployment, stamped with
`assurance_required = "presence"`, the gateway re-computing the shape
hash against the step's current content before it enrols — the
mechanism `17835005` settled, pointed at a second subject; (2) **who may
authorise is a named list of employee ids on the gateway**, starting
with David, not a role — a role is registry data, and would make
enrolment depend on whoever can write a policy row; (3) **single-use by
record**: the gateway writes the credential id and instant onto the step
and refuses a step already carrying one, naming it; (4) **the token
retires**, and with it the zero-credentials window. Not yet built.

**One bootstrap, at the identity layer** (design `af6dfcdb`, David
2026-09-22: "a 1-time password for the admin's initial passkey
enrollment and then real security takes over from there"; all four
questions accepted as proposed). Retiring the break-glass token left a
fresh instance no way in; the answer moves the bootstrap down, from an
emergency credential to an identity, so the system has exactly one.
Measured: `infra/oss-quickstart/init.sh` sets the bootstrap admin's
password from `BOSS_BOOTSTRAP_ADMIN_PASSWORD`, defaulting to the literal
`change-me`, prints it and asks for a rotation nothing forces or
notices — a standing default credential on a public quickstart.
Decided: (1) **the window closes when the admin holds a passkey**, not
by a spent flag — no state to write, nothing left armed by a crash, and a
reader checks it with a question whose answer is obvious; (2) **the
password is generated and printed once**, the variable kept as an
override for scripted installs and the default removed; (3) **once the
passkey is enrolled, the bootstrap admin's local-auth row is removed** —
`credentials.toml` stays for deployments that never enrol a passkey —
with the counter recorded: an adopter whose only passkey dies is locked
out, which may argue for keeping the row until a second factor exists;
(4) **a fresh instance reaches break-glass enrolment only through the
admin passkey**, so a shared secret never authorises an emergency
credential, at the cost of a window where that passkey is the only door.
Not yet built: `init.sh` still defaults to `change-me`.

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

**Every page is audited as a packet, and the march is a protocol**
(design `0e07ce64`, David 2026-09-18, all four questions accepted as
proposed). Measured on #451: 45 routes in the nav catalog; the mocked
Playwright crawl rendered 26 of them and checked only that the shell
painted; behaviour was exercised on ~10; a 35-spec live suite under
`apps/web/tests/smoke` had been run by nothing since 2026-06-18 and
read as coverage; nothing asked whether a page shows what its
department needs. Decided: (1) a platform kind **`page-audit`**
(`infra/platform/workflows/page-audit.toml`), one packet per
catalogued route with `route` and `department` required on the packet,
walking **measure** (the controls inventory beside the department's
in / working / out needs, the gap as a list) → **file** (every gap a
backlog-item carrying route + department; 'none' with a reason) →
**test** (a mocked spec pinning every control — links land on
catalogued routes, buttons answer honestly, back returns, an empty
backend paints an honest empty state, a refused write shows the
refusal — and the reviewer's brief) → **review** (the founder reads
the gap list, the inventory and the page's words; specs and styling
are the agent's) → **styled**, deferred by design as a marker the
machine completes, because the visual redesign lands on pages whose
behaviour is already pinned → **audited**; (2) the opener is a Bun
script, `apps/web/scripts/open-page-audits.ts`, because the ONE
roster is `ROUTE_CATALOG` in `nav-catalog.ts` — TypeScript with no
JSON twin, so a Rust verb could read it only through a second list —
and it opens one packet per route idempotently (open or audited =
skip) in the decided order: the departments whose first protocols
land first (support, sales, product, finance, marketing, hosting),
then the rest, Home's surfaces carried as `it`, IT's own last; (3) a
generic gated interaction crawl over the catalog is the mechanical
half, and the dead live suite is deleted in favour of one nightly
chore crawling the playground whose reds are packets — each its own
car. Tooling stays mocked Playwright — hermetic, gated, already in
the image; "more thorough" is more assertions per page, not a
different tool.

**A department with no protocol is stood down on this instance, not
given pages** (design `36b79159`, David 2026-09-23, accepted as
proposed). The march kept meeting one fact. Maintenance (audit
`015c3935`) had 0 protocols, 0 sensors and 0 rules — its 26
`maintenance-*` rows are IT housekeeping that shares the word, and its
pages sit behind the `equipment` module that `1054c099` turns off for
Algedonic; People (audit `0c0265a3`) had 0 protocols and a roster of
the founder and a system account. Each had a weekly retro with nothing
to retrospect and pages drawing only the demo's shape, and neither is
among the six first protocols (`86f32b7d`). Decided: **such a
department is stood down on this instance** — no weekly retro and no
sidebar pages until a protocol exists (modules-off already covers
Maintenance); People gets a protocol only when a second person joins;
and each remaining audit records "no protocol: stood down" instead of
re-raising it. Not built: the `retro.open` handler behind
`department-retros-weekly` still opens one retro per row of the
`departments` table, and no stood-down marker exists for it or the
sidebar to read.

**Algedonic, LLC keeps its at-scale surfaces, lightly used, to dogfood
them** (design `922f37be`, 2026-09-23; the proposal to turn
`/watchlist` off here was declined). Measured by the `/watchlist` audit
(`08b0c4f8`): no sales protocol produces or reads a churn score, the
accounts directory holds one account (the shared Anonymous Sponsor),
and the nightly batch that scores had fed nothing since the second
stack retired (`9599babc`). David, recorded verbatim by the operator
because the review box was clearing his entry (`fec57f5f`): "I wanted
to keep watchlist and other 'at scale' needs in Algedonic, LLC even
when it is overkill as a means to dogfood our software development. I
want to scale really smoothly, so it is okay investing in protocols
that may be very lightly used until we have real scale. But the
playground instance and simulator are going to be where we really
pressure test modules and behaviors that are relevant for companies
with physical operations and more people than Algedonic, LLC." So
light use is not a reason to take a surface off the company's own
instance; the line is physical operations and headcount, which the
playground and the simulator carry. It differs in kind from the
stand-down above: a department with no protocol at all has nothing to
dogfood. Nothing to build; the batch fix landed on its own (#586).

**The look is one light theme, Transit, and the map's colours are the
first to route through it** (design `dea94998`, David 2026-09-23: "I
like where we are going with option 2, the route map / transit
option"; answers backlog `42f66fb3`). The direction set that day: one
LIGHT-only theme for the BOSS instance and as the company's overall
tone, flowing into www.algedonic.dev and the GitHub presence — themes
compared side by side first, specifics after — and every UI and
styling car waits on it while back-end work does not. The boards were
compared as a claude.ai Artifact linked from the packet, the gap
exhibits (§Design docs, `26a89f11`) exist to close. Landed (#586,
`42f66fb3`): the groundwork, not the look — the world map (`MapPage`,
`WorldMap`, `RegionMap`) reads only a `--map-*` block in
`apps/web/src/styles.css`, with no fallback and every value still
today's colour, pinned by `it/yard/map-palette.test.ts` so a retired
token fails the test rather than repainting in silence. Not built: the
Transit palette itself, and the rest of the SPA and www on it; the SPA
still paints the dark theme.

**One map per floor: the region map draws its own slice of the yard,
and YardPage retires** (design `fe77a1d2`, David 2026-09-23, all three
questions accepted as proposed; answers backlog `c0565f48`, finishing
the consolidation design `d2154293` decided on 2026-09-19 — "let's
have this become THE surface"). Measured: `/it/yard/<region>` already
swapped to the region's own map (`ca37478f`), but for the six floor
regions `MapPage` still mounted `<YardPage focus={region} embedded>`
beneath it, and embedded YardPage still drew `YardMap` — the WHOLE
six-region floor — so `/it/yard/dock` showed the dock's wagons twice, a
plate and then a wagon, with five other regions underneath. Two facts
set the order: the region map's plates cannot be clicked, so every
selection that drives the entity panel and its verbs comes from
`YardMap` or the departure board; and `YardMap`'s drawing already
splits on region lines through the total `STATION_REGION` record in
`region-contents.ts`. The platform regions (receiving, marshalling,
shop floor) were already the end state — a region map, then that
region's working panels. Decided: (1) **each floor region draws only
its own slice**, so a wagon crossing from the dock to the track leaves
one map and appears on the next rather than sliding across one floor;
the crossing stays visible at the world level, where the borders carry
it, and the departure board still lists every wagon — a whole-floor
seventh view is the second page the consolidation removes; (2) **three
cars, and no detail leaves before its replacement is on screen**: split
`YardMap` into one pure layout function per region, keyed through
`STATION_REGION`, with no change on screen and a test that the six
slices' union draws what `YardMap` draws today; then `RegionMap` draws
its slice with `selected` and `onselect`, so every wagon, bay and
locomotive selects into the entity panel, and embedded YardPage stops
drawing `YardMap` in that same car; then the deck (alerts, departure
board, entity panel and verbs, production and signals) moves into a
`FloorDeck` that `MapPage` mounts, the source-reading tests repoint, and
`YardPage.svelte` is deleted, every route unchanged (`/it/yard` still
resolves to the track). The last two both edit `MapPage`, so they ride
one after the other; (3) **built now in the `--map-*` tokens**, not held
for the Transit reskin above, which chooses how the map looks rather
than what it shows and will then restyle one map per region instead of
two. Not yet built: `YardPage.svelte` still draws `YardMap` when
embedded ("STILL DRAWN when embedded, deliberately"), and no
per-region layout function exists.

**What the website says is checked against what the record holds;
whether it works is a reading with a threshold named first** (design
`59a776c5`, 2026-09-20). Correct and effective are two protocols with
two kinds of evidence. **Correct is a check:** a claim is marked in the
page source (`data-claim="<id>"`) and a registry
(`apps/web/src/marketing/claims.ts`) says which row answers it — David:
"Marked claims are fine to start, let's go with that." The limit is
accepted in the open: an UNMARKED claim is invisible to the check, so
the checker's report carries the marked count and a prose fingerprint —
the decidable stand-in for a coverage denominator nothing can know,
since words that moved while the claim set did not are the prompt to
mark what was added — and a claim whose row cannot be read is a
refusal, never a green. The scope is www only, a site being a row so a
second is data. The design decided a daily `check-the-claims` protocol
plus a run on every publish, a disagreement FILING rather than paging —
a price included, until one instance says a louder arm was wanted. The
caller that shipped first is a gate test instead
(`the-landing-page-claims.test.ts`, backlog `e1524f57`), where a
disagreement reds the gate: every source kind today is a fact of the
tree and can only drift through a commit, so the cadence arrives with
the first `registry` source kind, which reads live data and moves
without one. **Effective is `measure-the-page`**, already
decided by the department template (`3613f0af`): visits by page and by
week, and nothing else until that sensor has produced a decision, with
the step requiring the reader to write down what the number would have
to say to change anything BEFORE taking the reading — not yet built.

## OSS posture & tier boundaries

Two install paths: single-VM bare metal (`infra/oss-quickstart/`)
and Docker compose. File-backed auth is for evaluation; HA
topologies return as opt-in blueprints under `infra/blueprints/`.
Crates split into **Tier 1 — core state-machine OS**
(`crates/core/`, 26 crates: the four primitives' services, policy,
gateway, dispatcher, clock, expression DSL, taxonomy registries,
calendar, content, docs, ML stack, testing, ports,
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

**Four languages: Rust for systems, Svelte for the frontend, Python for
scripting, containers for deployment — and a fence around the third**
(design `9b4d8ccd`, David 2026-09-20, all four questions accepted as
proposed). Measured that day: 234 shell files, 51,806 lines, against two
Python files — shell was already the scripting language, and `python3`
was already a declared forge requirement. Decided: (1) **new scripting
is Python where the work is structured data** — the lints first, where a
1,422-line bash lint is the argument and the startup objection measured
0.56 s across all 88; **never for the forge's shell twins**, which
retire by deletion as the `boss` CLI reaches the forge (§Consolidation)
rather than by translation; **rarely for glue** that drives systemd,
`talosctl` and docker, where an interpreter on the recovery path is a
cost when the system is unhealthy; (2) **the fence is four rules**: no
`.py` under `crates/`; **stdlib only**, the load-bearing one — no
`requirements.txt`, no venv, no `pip` in any image, because a script
that needs a package manager is a program; never imported, only
invoked; and never on the runtime execution path of a cluster service
— Python may do a job and exit, never become a component; (3) **no
migration**: existing shell stays until it is touched for another
reason; (4) **enforcement is a lint measured like any other** — the
directory and no-pip rules are near-free greps, the never-imported
rule needs a parse. No such lint exists yet, so today the fence is a
written rule and is unchecked, stated here rather than implied.

**A real instance carries only what its tenant declares plus what the
platform needs; example data reaches an instance only through its
tenant contract; a bounded verb evicts residue.** (Backlog `718ac982`;
design `e2580840` car 3, David 2026-09-17, folding `83a873e8`.)
01-registries.sql and 40-ledger.sql seeded the two worked examples'
reference rows on every instance — the used-device shop's 26 roles and
ten departments, the brewery's location kinds, account types,
equipment categories and two production sites, the brewery-shaped
starter chart (33 accounts) and a `companies` row each — and on
2026-09-17 the company's own instance answered "Brewery Taproom" from
`/api/locations`. Those rows are now the example tenants' seeds
(`examples/brewery/seeds/classes.json`, `locations.toml`, the new
`chart_of_accounts.toml`; `examples/used-device-shop/seeds/classes.toml`),
published through the same contract every tenant uses. The migrations
are not edited — an applied file is history and `migrate.sh` refuses
a changed checksum — so the rows still land in every database the
converge creates, and they leave by two doors that read ONE
derivation, `infra/postgres/example-reference-rows.sh`, whose
candidate set is read from the example seeds and never typed: on a
fresh instance's FIRST start `boss-init` evicts them before any
service starts when the declared tenant (`BOSS_TENANT_DIR`, the same
directory the launcher publishes) is not an example — an example
tenant keeps its own rows, an undeclared tenant keeps them too; and
on an instance that has already booted the forge verb
`retire-example-reference-rows <mode> <namespace>` evicts them with
the record on the ops-request packet, `--dry-run` the plan, `--for-real`
one transaction per table, refused for an image-sourced instance.
**Deletable only when unreferenced**: a row anything points at — an
employee wearing the role, a location wearing the kind, a journal line
on the account, a tax kind naming it, a job about the company — is kept
and named with the reason, so an instance that adopted an example code
keeps it. What the platform keeps is exactly what the platform names,
and a test derives that line rather than listing it: the roles
flagged `is_system_role` (`platform-admin`, `audit-readonly`), the
bootstrap admin's and the operator baseline's row (the `it`
department, `loc-hq`), the kinds the platform's three default
locations wear, the `unspecified` account type the column defaults
to, employment types and statuses, the module-tier vocabularies. (Until
backlog `7f163e58`, 2026-09-18, that line also carried the five accounts
the migration's tax kinds reference; see the tax decision below.)
The starter chart is therefore no longer an OSS default a company
adopts by collision: a fresh company instance has no chart until its
tenant declares one, and the rows below about a colliding code apply
only to an instance that ran the migration before the eviction.

**The instance is the truth; seeds bootstrap; the tenant repo is
bootstrap + export.** (Design `e187198f`, David 2026-09-18: "move away
from seeds mattering, except to help OSS users bootstrap and/or to
support playground; our actual BOSS instance should be flexible to use
data instead of seeds; going all the way back to a seed is its own
issue.") Measured that day against `40868c22`: `boss tenant publish`
runs at EVERY services-container start, and four of its doors
overwrote a live row on each run — business calendars wholesale
(`ON CONFLICT DO UPDATE` + a closed-day DELETE/reinsert), the company
label (`COALESCE(EXCLUDED.label, …)`), an employee's declared fields
(the 409→GET→PUT overlay of `09887242`) and an agent's whole row — so
an operator's edit to any of them lived until the next boot, while four
others (classes, sensors, policy, workflows) kept the live row and said
NOTHING when the file differed, so a repo edit that never landed was
dead text. Decided, all as proposed: every door is **insert-if-absent
by default**, each batch route carrying `?mode=insert-if-absent|take`
where its semantics live; **`boss tenant publish --take
<registry>[,…]`** is the only overwrite, sent per named registry
(`classes`, `calendars`, `company`, `policy`, `employees`, `agents`,
`workflows`) and printing every overwritten row field by field; the
contract's "declaration wins on declared fields" rule (`09887242`,
2026-09-17) is **dropped** — it covered a repo-edited location that had
not landed, the bootstrap case, and on a running instance the same
overlay was the collision; **every registry's publish line names its
kept-but-differing rows** in one shape, `kept: <id> differs on
<fields> (the instance is the truth; --take <registry> overwrites)`,
as the decision surface; the publish runs **once per database** (a
stamp, like `init.sh`'s first-start gates) and thereafter only by
`--take`; and **`boss tenant export`** writes the live registries back
into the contract's file shape by machine on a cadence, so the repo is
never hand-edited on a live instance. The playground publishes the
brewery every boot and, with insert-if-absent as the default, that
publish changes nothing on a running instance — which is the point.
Car 1 (the doors, the flag, the lines, this paragraph, the contract's
section — printed by `boss tenant contract` and pinned) landed first;
the stamp and the export verb follow. The rule is stated for tenants in
`docs/tenant-contract.md` ("The instance is the truth; `--take`
overwrites by decision").

**A module is on only when its tenant lists it `true`, and an instance
says how many at boot** (design `1054c099`, David 2026-09-22). Measured:
the company's own gateway answered `"modules":{}`, and under the contract
the brewery's `tenant.toml` states (a missing key is off, the same as
`false`) all ten module-gated surfaces had been off since the
declaration existed. It went unseen while the nav catalog and the route
switch disagreed about which module gated which route, and surfaced when
car `f9b43965` made them one fact: a missing declaration is not an
error, it is a confident empty answer. The declaration lives in the
tenant repo that `infra/cluster/instances.toml` names, so which modules
the company runs is a business statement, not a car. Decided: Algedonic
runs `finance`, `exec` and `support` (support "as that is our email
inbound for now"); `calendar` only once dated work is modelled as Jobs
with a `release_date`, since an empty surface is worse than a hidden
one; the six physical-operations modules are WRITTEN `false` with the
reason rather than left missing; prod's absent `edit_level` is
confirmed intended. The platform half: **an instance states its module
count at boot** — a tenant running only jobs, people and messages is
legitimate, so a refusal would be wrong, but silence let ten surfaces go
missing. Landed: the services launcher and the gateway each print one
line (`modules on: <n> of <m> declared`, naming both sides), and the live
manifest reads exec, finance and support on and every other declared
module off.

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

**The forge is also the cluster's image source, so its downtime is the
cluster's restart window** (design `ccce1191`, 2026-09-20). Thirteen
manifests pull from the forge's registry, the system of record's
postgres among them — pulled from the forge rather than docker.io
because a database that cannot re-pull its image cannot restart. While
the forge is off, running pods keep running and anything that must
restart waits in `ImagePullBackOff`. So a forge window is quiesced (no
trains, gates, converge or deploy) and never overlaps a cluster node
being down; with etcd quorum at two of three and Longhorn at three
replicas, nodes go one at a time. Where postgres sits during a
control-plane window was not reached, and waits for a window that wants
one. The
window it was written for took the forge alone (David: "Let's do
forge only then"), and the extra disk was left to a judgement at the
box, because the measurement that night was headroom, not a repair.

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

**Retiring a unit will mean masking it, because disabling is not
retiring** (design `7b230ddf`, David 2026-09-23, accepted as proposed;
answers backlog `9599babc`). Measured through `boss ops boss-gcp
journal-tail` (ops-request `5acce6ec`): `boss-ml-api`, stopped and
disabled by `retire-second-stack` on 2026-09-15, had been running on
boss-gcp since 2026-09-20 as one process — the kept
`boss-ml-inference-batch.service` declared `Requires=boss-ml-api.service`,
and systemd starts a required unit whether or not it is enabled, so the
02:30 batch revived it and it served the batch nightly against the
retired stack's database while no prediction reached the system of
record. The class: any kept unit that `Requires=` or `Wants=` a retired
one brings it back. Landed (#586, `9599babc`): the batch reaches the
cluster's ML API through the machine door, its `Requires=` is gone, and
its packet carries the prediction count, a zero night failing loudly.
Decided, not built: after that car converges, `retire-second-stack` is
re-run for `boss-ml-api`, and the verb changes to **mask** what it
retires (`systemctl mask`) and to refuse to retire a unit a kept unit
still `Requires` — still a bounded verb, still David's to file.

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

**The dev door is an Access SSH application** (design `5fc71f03`, David
2026-09-18, all three as proposed; backlog `e4cedb46`). That one
Deployment answered only on a MetalLB VIP on the LAN, so from anywhere
else the way in was a jump through the boss-gcp WireGuard bastion —
which `/it/estate` had to spell out in three forms, because `ssh://`
cannot carry a `ProxyJump` — authorised by one long-lived ed25519 key
an operator had loaded into a Secret by hand. It is now
`dev.algedonic.dev`: a CNAME to the tunnel, a route on the in-cluster
connector to the pod's own ssh Service, and a Cloudflare Access
application in front of it, all four declared in the same files every
other public name uses. What that buys is not convenience but the
credential: **Access issues a certificate for the session**, which the
pod's sshd accepts through `TrustedUserCAKeys`, so nothing long-lived
sits on either side of the door.

Two things fell out of the measurement. The pod ran **Dropbear rather
than OpenSSH for one capability** — sshd's preauth privsep child
chroots, Dropbear does not, and in 2026-08-30 the cheaper door won.
Dropbear cannot verify a certificate against a CA at all, so the swap
back costs exactly `SYS_CHROOT` and the history stays in the manifest
beside the capability list, which is the only place a reader would ask.
And **the Access declaration needed no new vocabulary**: `type` is a
value the handler carries from the declaration to the comparison to the
create body, never matched against a list of known kinds, so `type =
"ssh"` is declared, compared and applied by the same four fields
`self_hosted` uses — pinned now, so the next reach for an enum goes
red. The short-lived-certificate CA is not a field: Cloudflare generates
one per application, and reading its public key into the Secret the
manifest names is a root ceremony, done once.

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

**Upgrading Claude Code is a bounded ops verb that David or the agent
may run** (design `773236fc`, David 2026-09-22, all three questions
accepted as proposed). Sometimes a new model is blocked on the upgrade,
so it is a precondition for working rather than housekeeping. Measured:
`/work/home/.local/bin/claude` is a symlink into self-contained binaries
under `versions/` on the persistent `/work` PVC — an upgrade is one
symlink flip, rollback is pointing it at a NAMED version already on disk
with no network, a running session keeps the binary it exec'd, and a
pod roll loses nothing. Decided: a verb `upgrade-claude-code` under
`infra/ops/verbs/` that resolves the target to a concrete version and
records it (never "latest", which names nothing), records the version it
leaves as the rollback target, defers while a `gate-run` is launching,
proves itself by `claude --version` or re-points to the recorded version
and reports failed, and prunes; (1) **the agent files and runs it** —
bounded, offline-reversible and unable to touch work in flight, so
gating it on David gates a model on his being at his desk; he keeps the
UI entry; (2) **the target is per request**, recorded on the packet,
never pinned in the tree, which would make every upgrade a car and a
converge; (3) **three versions are kept**. Upgrading at pod start is
rejected: it makes the pipeline's tool whatever the registry served at
boot, with no receipt and no named rollback. Not yet built; the dev pod's
manifest only installs the tool when it is absent.

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

**A credential the estate can mint is delivered by the machine, never
carried by a person; the line is derivability, not sensitivity**
(design `835c0c9c`, David 2026-09-20, all three questions accepted as
proposed). Root material — an admin kubeconfig, a
talosconfig, the credential broker's own root tokens — cannot be
minted from anything the estate holds, so placing it stays David's
act, as above. A scoped credential is minted FROM that material, so
its path is the credential broker
(`infra/cluster/manifests/boss-credential-broker.yaml`), which exists
because a hand-placement walkthrough leaked the dev pod's forge token
on 2026-09-02 — the ceremony was the vulnerability. The first case the
broker's Secret-and-mount pattern cannot reach is boss-gcp's scoped
break-glass kubeconfig (`boss-break-glass-operator.yaml`): an
off-cluster host cannot read a Secret without the credential being
delivered. Decided: (1) **push, don't pull** — a bounded ops verb on the
forge, which holds admin and already reaches boss-gcp, mints the
kubeconfig and deposits it over a forced-command key scoped to
`/etc/boss-ops/`, the shape `boss-backup` already uses for the nightly
dump, so the value never enters a packet, a transcript or a person;
(2) **verify by effect in both directions** — the credential CAN read
nodes and CANNOT read secrets — and refuse the deposit when the
negative fails, because a break-glass credential broader than its
declaration is worse than none; (3) **a host declares the credential
set it is expected to hold** (`BOSS_OPS_CREDENTIALS=kubeconfig` on
boss-gcp), since a talosconfig has no scoped form and a full one on the
public edge is the unbounded grant the scoping exists to avoid — so
the converge reports the truth about that host rather than a
permanently red absence. The delivery verb is not yet built.

**Agent B's mailbox: BOSS holds the operational pair, the founder holds
the account** (design `0ec5e1d2`, David 2026-09-21, all three questions
accepted as proposed). The keyvault already exists — the credentials
registry holds knowledge and never values, values live in k8s Secrets
read only to send them, the broker mints, verifies by effect and
revokes — so the question was what goes in it. Agent B has four secrets
and they are not one risk: the Proton account password (the whole
mailbox, including changing its recovery address), the MFA seed, the
Bridge IMAP password (one Bridge install, localhost only) and the SMTP
submission token (send as Agent B, nothing more). Decided: (1) **BOSS
holds the IMAP password and the submission token; David holds the
account password and the MFA seed**, and neither of his ever enters the
registry, the tree or a transcript — an agent holding the account
password can take the mailbox, which no protocol step needs; the same
scoped-not-root split as the break-glass kubeconfig above; (2) **both
BOSS-held secrets are registry-declared and broker-rotated**, the shape
of `stripe-restricted-read`, never hand-placed; (3) **MFA goes on now**,
its cost accepted knowingly: Bridge keeps its session in the host
keyring, so a session that dies at 03:00 stops the mail sensor —
`sensor_unreadable` makes that loud, and recovery needs David. Not yet
built: the credentials registry holds no Agent B row. Whether the
founder-held pair belongs on the one-stick recovery kit (`c1bb822e`) is
that kit's question.

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
by `boss prove --unattended` — until 9f00a805 car 2, run-car-probe.sh — from `infra/forge/sor-ports.env` in its own
checkout), defaulting to jobs, still one argument, still refusing a
URL and an unidentified read, and unchanged when no table is present;
(3) **the gateway service-token door is deferred** to the hardening
programme, as the machine door for OFF-LAN callers — the probe runner
is on the LAN and needs no public door, and a proof instrument that
depends on Cloudflare and a credential rotation is the wrong
dependency to add before hardening starts. Not chosen: the jobs API
proxying read-only paths for the probe tier, which duplicates the
gateway's routing inside a domain service.

## Consolidation toward 1.0.0

**The debt is measured, the order to pay it is decided, and every
consolidation car leaves a measured line smaller** (design `42277636`,
David 2026-09-18; all four questions accepted as proposed). The
framing, David's: "We built up a lot of tech debt in our sprint to
design BOSS, and we need to spend this next period getting ourselves
simple, clean, functional, and adaptive … getting the open source
version into 1.0.0 readiness, but we are going to prove its utility by
running Algedonic, LLC on it." A read-only audit of origin/main
`3465acbb` (train #436) measured the debt — 406k Rust / 49k shell / 93k
TS; 82 lints, two of which could not fail; ~100 pinned pairs, 14 with a
concrete collapse; 154 migrations of which 81 were DML-only seeds; a
2,081-line bare-metal deploy script with ~310 live lines; the
system-of-record address spelled in 47 files — and the report stays on
the packet, not in a file. Decided: (1) **the order** — 1.0.0 blockers
first, being what a stranger hits before anything else (H5 instance data
in the platform schema, H6 `emp-david` literals, H10 the address
literals, H7 the bare-metal path), then what costs cars and diagnoses
weekly (H2 lints that cannot fail, H3 unguarded flake classes, H1 the
contended monoliths, H9 the lint exclusion set in five places, H4
registry rows whose only home is a migration, H8 no `boss` binary on the
forge, H11 nothing prunes the pod), then the MEDIUM and LOW findings as
filler cars when a builder is idle; (2) **the rule** — each item is one
car or a short series, and **every car must leave a measured line
smaller** (files, definitions, baselines): a consolidation car that
cannot name the number it reduced is not one; (3) **the bare-metal
deploy path is deleted, and the container launcher is the one way to
run BOSS** — it was 85% dead, drifted three ways from the container's
config generator, and every real instance is the container path; the
OSS quickstart documents docker/Kubernetes only, and the deleted code
stays in history; (4) **the `boss` CLI is installed on the forge from
the image** (as boss-gcp already is) and the ~3,000 lines of shell and
python twins each pinned by a test retire one verb at a time — one
implementation of each verb, in Rust, tested once, the forge running
the same binary the cluster does; (5) **the stranger lens** — the
by-hand and by-memory steps a fresh operator has no door for (the pod's
hand-installed tooling and door symlinks, the forge's hand-placed
Forgejo config, a printed runbook for the root ceremonies, the
hardening inventory) become items now, and they are the quickstart's
test: a fresh install on a clean machine, by the runbook, with nothing
from memory.

**What has landed, each with its measured line** (the item ids are the
backlog packets the design spawned):

- **H3 — the flake classes have a guard** (train #439, 2026-09-18,
  item `2c257761`). The 47 `… | grep -q` pipelines under `pipefail` in
  `infra/lint/` — a 1-in-3,000 SIGPIPE false red or green — became
  here-strings the converted-lint pin already covers, and the
  `tracing::set_default` shape that redded two trains (the #281 class)
  left `boss-core/src/publisher.rs` for one-test binaries that own their
  process, pinned by `a_log_capturing_test_owns_its_process.rs`.
  Measured: 47 flake sites → 0.
- **H2 — a lint that scans nothing is red** (train #441, 2026-09-18,
  item `cdf2d959`). `sim-boundary-audit.sh` scanned zero declarations
  under mawk's `\s` and exited clean; `seed-bypass-smell.sh` scanned a
  deleted directory. Every scanning lint now prints its scanned count
  through `infra/lint/lib/scanned.sh` and refuses on zero, a test of
  lints (`a_lint_that_scanned_nothing_is_red.rs`) runs each against the
  tree and asserts it, stale allowlist entries are refused by a shared
  helper, and `a-kind-bundle-does-not-tighten.sh` — the weaker twin of
  `steptype-bundle-ratchet` — was deleted. Measured: lints that cannot
  fail 2 → 0, and one duplicate lint gone.
- **H6 — one `platform_owner()`** (train #442, 2026-09-18, item
  `3c23662d`). Fourteen production sites named `emp-david` (one in
  Tier-1 `car.rs`) and `bootstrap.rs` named `emp-cto`; a second
  operator (Algedonic, LLC on BOSS, decided 2026-09-16) would have been
  fourteen edits or every alarm landing on David forever. The owner is
  now read once from the people registry (`boss-people-client::
  platform_owner`, the first hire holding `platform-admin`, with an
  explicit launcher override), the shell alerts read it off the same
  API, and `no-employee-id-literal.sh` refuses a new literal outside
  tests and example seeds. Measured: 15 literals → 0.
- **H7 — the bare-metal deploy path is gone** (train #443, 2026-09-18,
  item `e109bd71`; 102 files, +1,134/−5,508). `deploy-services.sh`
  (2,081 lines), `deploy-web.sh`, `bootstrap-local.sh` (which still
  started the deleted `boss-docs-api`), `bootstrap-vm.sh`,
  `dev-postgres.sh`, the four smoke scripts nothing ran,
  `check-service-drift.sh`, `check-service-write-roundtrip.sh`,
  `push-step-plugins.sh`, the host backup and restore, the
  deploy-confirm dead-man (unit, timer and script) and `files-root.sh`
  were deleted; the ~310 live lines — the `units` and `roster` modes
  boss-gcp's converge runs — became `infra/gcp/install-units.sh`,
  reading its roster from `infra/estate/roles.toml` and nothing else.
  The container launcher (`infra/oss-quickstart/services-launcher.sh`)
  is the one way BOSS runs, and its roster — the unpinned copy the
  whole time — is now the one held equal to `boss-ports`. The quickstart
  and the operator runbook document the container path only.
- **H10 — the address is spelled once** (train #444, 2026-09-18, item
  `5222163e`). The system-of-record address was in 47 files three ways
  and the forge IP in 62. Both now live in `infra/estate/estate.toml`:
  each managed host's converge renders `/etc/boss/sor.env` from it,
  every unit reads that file with `EnvironmentFile=`, every script
  through `infra/lib/sor.sh` (which refuses, naming the file, rather
  than fall back to a literal), the forge verbs source one
  `forge-defaults.sh`, and `the-estate-address-lives-once` refuses
  either address anywhere else, with a per-file allowance table whose
  stale entries are themselves refused. The `boss.algedonic.dev`
  cutover is now an edit to one file. Measured: 47 files → 1.
- **H5 — instance data leaves the platform schema** (train #445,
  2026-09-18, item `ee368d0c`). Nine migrations inserted this LAN's
  seven nodes and eight service instances, five node roles and eight
  credential ids into every fresh database — an adopter's first boot
  declared a cluster it does not run and credentials it does not hold,
  and one re-declaration under `ON CONFLICT DO NOTHING` had never
  landed anywhere. Credentials are now the instance's declaration
  (`seeds/credentials.toml` in the tenant contract, sent by `boss tenant
  publish` through an insert-if-absent batch door); the estate's nodes
  and roles are the tree's (`infra/estate/estate.toml`, published on
  every pod start through `POST /api/estate/nodes/batch`); and migration
  `20260918063829` deletes the seeded rows wherever nothing references
  them. Measured: the instance rows a fresh install boots with (seven
  nodes, eight service instances, their roles, eight credentials) → none.
- **H9 — the lint declares its own consist skip** (train #447,
  2026-09-18, item `6fa15484`). Which lints the consist check leaves
  out lived five times, and nothing held `gate.sh`'s copy equal to the
  one the conductor ran on — which had already produced one false
  refusal. Whether a lint needs more than a bare tree is a fact about
  the lint, so each such lint declares it on one header line
  (`# consist: skip — <why>`), `gate.sh --exclusions` derives the set,
  the conductor asks the assembled tree's `gate.sh`, and migration
  `20260918102236` drops the `consist_excluded_lints` column the
  delivery policy no longer needs. Measured: 5 definitions → 1;
  `delivery_policy.rs` −160 lines.
- **H7 residue — the hops that called the deleted path** (this car,
  item `ed64f852`, 2026-09-18). What #443 left: the conductor's own
  deploy hop in `train.rs` (pulled `/opt/boss`, ran `migrate.sh`,
  `build-release.sh`, `sudo deploy-services.sh prod`, `sudo
  deploy-web.sh`), reachable only through a `BOSS_TRAIN_DEPLOY_TREE`
  the one live conductor had set empty since the 2026-09-04 cutover;
  `boss status`/`restart`/`logs` and `boss deploy` (a hand-typed
  service roster, `systemctl` and `journalctl` on a host, a backup check
  reading a deleted timer); `check-binary-freshness.sh` (the
  `/usr/local/bin` fleet), `dev-refresh.sh` (an ssh to a host that no
  longer exists) and `deploy.env.example` (sourced by the deleted
  script). Every present-tense mention of `deploy-services.sh` became
  history or named its successor. Measured: 4 files and ~2,300 lines
  deleted, 4 CLI verbs gone; the `deployed` step of a train is now
  completed by one arm instead of two, with the same evidence it always
  carried on the cluster. Kept, each with its reason: `build-release.sh`
  (the migration rehearsal `restore-log-copy.sh` names it for the
  release binaries it verifies), `validate-brewery-sim.sh` and
  `reset-to-baseline.sh` (the maintainer correctness gate and the
  drop-and-reseed it calls, named by the README, the runbooks and the
  invariant register — still shaped for a host with systemd, which is a
  retarget, not a deletion). And one thing measured rather than done:
  the `maintenance-deploy-confirm` kind (0 packets ever) was retired on
  the live registry at 12:20Z and re-published by the next boot's
  platform seed at 12:42Z as v3, active — the seed reads "present" as
  "an active row exists", so a retired kind whose bundle file is still
  in the tree is re-inserted on every boot, and the file cannot leave
  the tree while the registry admits the kind. The seed must learn that
  a retired version is an operator decision it does not revert; then
  the retire sticks and the file goes.
- **H4 — registry rows whose only home was a migration** (trains
  #448, #450, #453 and this car, 2026-09-18, item `393d3234`; the
  decision paragraph is under §Stations above). Four registries, four
  cars, one shape each: `infra/platform/stations/`, `step-plugins/`,
  `cadence/`, `delivery-policy/` — one TOML per row carrying every
  column, published insert-if-missing by (name, version) by the seed
  every launcher already runs, through one decision table
  (`boss_jobs::bundle_seed`, written once in car 1 and reused three
  times); `infra/lint/migrations-declare-schema-only.sh` holds the
  cutover stamp `20260918112134` once with the four tables beside it;
  each bundle is held equal to the migrations' active rows by a
  `*_bundle_is_the_migrations_pg.rs` pin that also rebuilds an emptied
  table from the bundle alone. Measured: 25 migrations (7 + 7 + 9 + 2)
  stopped being the only home of 21 declared rows (5 stations, 12
  step-plugin kinds, 3 cadence rules, 1 delivery policy — the
  directories, counted); no migration newer than the cutover may
  insert one; the seed reads five bundle directories from one
  `--seed-path` and no launcher changed. Kept, with its reason: the
  conductor's compiled delivery-policy fallback (a dark registry must
  still board a train), pinned to the bundle rather than deleted.

**Pending, by item:** H11 the pod prunes worktrees whose branch is gone from the
forge (`1933db9e`); H12 the conservation-invariant sweep never ran
against the system of record (`236529aa`, found by the H7 measurement);
H1 splitting `train.rs` along its test-module seams, and H8 the `boss`
CLI on the forge with the shell twins retired verb by verb, are decided
above and not yet filed as items; the stranger-lens items likewise. The
MEDIUM and LOW findings ride as filler, by what a first outside user
would see.

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

**A proposal whose substance is visual rides inside the packet as an
exhibit, not behind a link** (design `26a89f11`, David 2026-09-23, all
five questions accepted as proposed; from his question that day about
richer renderings "so we don't need to go to the claude.ai links"). A
palette, a layout or a diagram was rendered as a claude.ai Artifact and
linked, which put what was approved outside the record and mutable,
behind a second door, and out of `boss brief`'s sight. **HTML as the
doc's source was rejected**: packet prose is untrusted — any actor can
write step metadata — and the review surface
(`infra/step-plugins/review-design.js`) runs in the reviewer's
authenticated session, which is why it renders prose through web-kit's
escape-first `renderMarkdown`; and the record is text that agents,
search and this fold read. Decided: a design packet keeps its markdown
and questions and may also carry **`exhibits = [{anchor, title, html |
file_ref}]`**. (1) **The bytes** live inline in step metadata up to a
size bound (proposed 256 KB), frozen when the step completes, and in
`file_refs` above it, content-addressed and rebuilt from
`content.file.attached`; the build first measures whether `file_refs`
is on, and ships inline-only if it is not. (2) **An exhibit may run
script and nothing else**: `<iframe sandbox="allow-scripts">` without
`allow-same-origin`, loaded by `srcdoc`, under CSP `default-src 'none'`
with inline style and script only — no network, forms, top navigation
or fetched assets; one that needs live data is a page, not an exhibit.
(3) **Any actor who may author a design packet may attach one**,
agents included — the sandbox is the protection, not the author list —
with the attaching actor recorded. (4) **A question may bind exhibits**
by anchor and renders beside them; validation refuses an anchor the
packet does not carry. (5) **It is proven** when the visual redesign is
reviewed in `/it/design` with no claude.ai link. Authoring is `boss
design --exhibit anchor|title|path.html`, read with no shell between;
where an exhibit cannot render (a terminal, `boss brief`) it is listed
by anchor, title, size and hash, never dropped. Not built, and not yet
proven: the theme board (`dea94998`) was decided from a linked
Artifact.

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

**A feedback or backlog item routed to `design` opens the executor's
`draft-design` task first, and the founder's `design-review` waits on
the design that answers it** (`ready_when = steps.draft-design.done`;
backlog `f90ca046`, 2026-09-18, measured on 54f0ab33 where the review
went ready with no design filed — an empty decision, 4f6019d7 again) —
`boss design --answers <packet>` files the design, writes the review's
question, and completes the draft with the `design_id`, and the publish
rule closes the review exactly as before.

**Design error gets two instruments: the brief carries prior art, and a
packet's proposed FIX is verified, not only its claim** (design
`0d02cda0`, David 2026-09-21, all three questions accepted as
proposed; answers backlog `3c779ca8`). Measured over 2026-09-20/21: the
protocol refused a mechanical error nine times, each refusal naming its
reason, while five design errors went through — one reached David (a
scoped credential designed as a hand-placement, the broker that retires
that ceremony never found) and four were caught only by the operator
searching first, a habit rather than a mechanism. Decided: (1) **`boss
brief` derives prior art** the way it derives invariants — what has
already been decided about the packet's area, matched on its `area` and
title terms — and is **informational first**, printing the match count
and the sources searched even when empty, because a fuzzy match needs a
precision number before it can earn a refusal; (2) **the corpus is**
closed design-doc resolutions, this document's headings,
`infra/dispatcher/rules/` names and CLAUDE.md §Doors — deliberately not
the open backlog, which would match nearly everything; the acceptance
test is that "credential" surfaces the broker; (3) **"verify the
proposed fix, not only the claim" goes in both** CLAUDE.md's startup
step 4 and `builder-rules.md`, since three packets that night were
right that something was wrong and wrong about the remedy, and a
builder reads a packet's fix shape as an instruction. Not yet built.
Design error in genuinely new territory, with no prior art and no
proposal to check, is out of reach of both by construction.

## Agent memory and the record

**A memory corpus is a second copy of the record that cannot be
audited.** Decided in design `a5368918` (2026-09-19), on the
measurement that opened it: the agent memory index loaded with
`MEMORY.md is 24.5KB (limit: 24.4KB) — 1 of 152 lines were cut off`,
and the line it dropped was David's world-map direction for `/it`, one
day old and load-bearing for three cars then on the dock. Nothing
failed loudly — the session read 151 lines and had no way to know the
152nd existed. A store that sheds content by growing is the one
forbidden failure mode, and the file has none of the properties the
correctness protocol exists to guarantee: unversioned, outside the
event log, mutable in place, no provenance per claim.

**The rule: agent memory shrinks from a corpus to a kernel of
USER-scoped facts, and the test for a survivor is "would a new repo
need it?"** Everything project-scoped moves to where it can be
audited, by class:

- **A door is never remembered.** Each doors-class entry becomes a
  CLAUDE.md line, a `boss brief` derivation from the file that decides
  it, or a refusal in the verb itself. A remembered door can drift
  from the door it describes, and unlike the §9a pairs no equality
  test is possible against it.
- **A decision lives in this document and nowhere else.** The memory
  line is a lossy second copy of a decision the record holds in full.
  If a decision is not discoverable from the record without a memory
  prompt, that is a defect in the record SURFACE, not a reason to keep
  the copy.
- **The session-state class is deleted outright.** `boss orient`
  computes trains, gates, the dock, stranded greens and MY WORK from
  the record; a hand-written READ FIRST snapshot is a stale duplicate
  of a live read. Each thing orient cannot yet compute is a car on
  orient, not a memory line.
- **Incident residue belongs to the packet that fixed it**, and to the
  comment in the code that cites it by date and packet id. The rule
  outlives the memory of it.

Two consequences carry forward as work rather than prose. **The budget
must FAIL rather than truncate** — we do not control the harness
loader, so the refusal has to be ours: a check that reads MEMORY.md
and refuses over budget naming the offending entries. Where that check
runs is the one sub-question left open, because the file lives outside
the repo at `/work/home/.claude/`. And **user-scoped data is a tenant
Subject row**: David is an `employee` Subject with a Class, so
timezone, company email and working hours are Subject and Class data
read through the same door as everything else — which makes the kernel
genuinely empty and deletes the concept rather than shrinking it.

What was done on the day is a pin, not the destination: 131 over-long
index hooks were trimmed (25,675 → 21,388 bytes, no entries deleted),
purely to stop the live truncation.

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
