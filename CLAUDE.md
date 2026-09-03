# BOSS — Coding Guidelines

## Project Overview

BOSS is event-sourced **software for modeling systems as state
machines**. The acronym expands to **Beer Open Source Software —
for System Modeling**, named after **Stafford Beer** — the
British cybernetician whose life's work was modeling companies in
software (the Viable System Model, Cyberstride / Project Cybersyn,
*Algedonic* feedback signals). BOSS sits in that lineage: an
event-sourced, state-machine-shaped OS for describing real-world
organizations directly. Real-world systems get described as
Subjects (identity-bearing things), Jobs (bounded units of
coordinated work), Steps (typed transitions), and an immutable
event log over the state changes those transitions produce.
Executors are humans and agents — the "human-powered state machine
OS" framing is the executor model on top of the abstraction.

The public demo tenant **Algedonic Ales** (the brewery the repo
ships with) is the worked example, not the namesake — its name
is a tip of the hat to Beer's *algedonic* terminology, and the
brewery context lands the "model what your company does directly"
pitch through a literal beer-on-software pun.

When writing prose for users (READMEs, design docs, runbooks, UI
strings), use the all-caps form **BOSS** — it is the canonical
brand spelling. Lowercase `boss-` survives only as the prefix on
crate paths, binary names, env vars, and systemd units (`boss-core`,
`boss-jobs-api`, `BOSS_POSTGRES_URL`, `boss-brewery-sim.service`)
where uppercase would break tooling.

The concrete use case this repo ships is **managing a company**:
Jobs encode work-in-flight, Subjects identify what the work is
about (an account, an asset, an employee), Steps gate transitions
behind explicit ownership and sign-off, the event log gives
operators a complete audit trail. Two worked-example tenants:

- **BOSS Brewery** — public OSS demo tenant; an industrial-scale
  brewer. Data-first seeds under `examples/brewery/`.
- **Used-device-shop** — second worked example, a business that
  sells, services, and resells used physical devices needing
  sophisticated diagnostics and repair. Tenant-specific flows live
  in the `boss-commerce`, `boss-inventory`, and `boss-shipping`
  crates; this tenant builds on top of the platform-supplied
  Equipment KB (`boss-catalog`, `boss-assets`) and people domain
  (`boss-people`).

Both tenants are instantiations of the company-management use case
running on the underlying state-machine abstraction. BOSS itself
is neither a brewery product nor a device-refurb product. Treat
the example tenants as plugins built on top of the core, and the
core itself as a generic state-machine modeling toolkit that
happens to be tuned for human + agent executors.

The four foundational primitives are **Subjects** (identity-bearing
things the work is about — an asset, an account, an employee), **Jobs**
(bounded units of coordinated work), **Steps** (typed transitions inside
a Job), and **Events** (the immutable record of state changes — the
system of record). The Class registry (reference data on Subjects),
StepPlugins (UX extensions on Steps), and Policy (the privilege model)
are supporting concepts that hang off the four. We start small, stay
incremental, and optimize for adaptability over everything else.

### Founding ideas — what every load-bearing decision is measured against

Three intellectual lineages anchor the design. When you're choosing
between alternatives, weigh the candidate against these:

- **Stafford Beer — cybernetics as the operating model.** BOSS's
  namesake. A company is a viable system describable in feedback loops,
  *algedonic* signals, and recursive structure. Subjects, Jobs, and the
  event log are the software-shaped form of that claim.
- **Rich Hickey — "information is simple."** Data is primary; functions
  of data are easy; hiding data behind a mutating object trades
  simplicity for convenience and pays forever. The audit log is the
  system of record; projections are pure functions of it; rebuilders
  reproduce truth from the log.
- **George Orwell — *Politics and the English Language*.** Communication
  decays when language drifts from reality; vague, abstract, or
  euphemistic prose lets sloppy thinking pass for rigor. The audit log
  + the five-property correctness protocol (provenance, conservation,
  closure, idempotence, determinism) are BOSS's defense: the system
  holds what *did* happen, immutably, so the words operators use stay
  anchored to the facts. This is why correctness is a first-class
  invariant, not a quality bar.

All three converge: the company *is* its event log + its current state
+ the rules connecting them. Design choices that respect that
convergence land cleanly; ones that don't accumulate fragility.

### Reading frame: the three layers

**The network is the substrate. The fat protocols dictate the current
operating model. The actors run it.** (David, 2026-08-13.) Three layers,
each replaceable without disturbing the others — which is what lets a
company change how it works without rebuilding what it works *on*:

- **The network is the substrate.** Packets (a Job is an immutable
  envelope + a protocol set fixed at admission), stations (data-defined
  priority queues that route or hold packets until there is bandwidth or
  capability), routes, the log, and the one admission edge. This layer is
  physics — it has no opinion about what the work *means*.
- **The fat protocols dictate the current operating model.** Workflows
  are protocols, and the meaning lives in the protocol row, not in the
  endpoints: steps, the predicates that order them, the evidence each
  requires, the terminals, the obligations. That is why protocols are
  **registry data** — versioned, append-only, in-flight packets pinned to
  the version they were admitted under. "The **current** operating model"
  is load-bearing: a protocol that cannot be replaced without a deploy
  has leaked into the substrate, and that leak is the defect to hunt.
- **The actors run it.** Humans and registered agents are the CPUs —
  nothing moves without an actor claiming a step and doing it. Capability
  is enforced at the claim; bandwidth is finite, which is why stations
  hold rather than drop. Actors are not users of the system; they are the
  part of it that executes.

Canonical statement:
[docs/design/the-three-layers.md](docs/design/the-three-layers.md).

**The "human-powered state machine" reading is the execution lens over
this, not the foundation.** It answers "how does one packet get
executed": the event log + projections are the machine's memory; the
StepType registry is the alphabet of legal transitions; Workflow is the
program written in that alphabet; a Step's `status` is the program
counter; My Day / assignments is an actor's station rendered (not to be
confused with the `boss-dispatcher` core service, which runs step
side-effect rules off `step.done.<kind>` topics); policy is the privilege
model on CPUs. Its invariants still hold — human and agent executors are
CPUs in the same machine, not a separate system, and new work types are
registry rows, never bespoke core code paths. Read a new workflow, page,
or abstraction against both frames; when they disagree, **the network
framing wins**, because it is the one that survives changing the
operating model. The lens and its invariants:
[docs/design/human-powered-state-machine.md](docs/design/human-powered-state-machine.md).

The five-property correctness protocol (provenance, conservation,
closure, idempotence, determinism) named in §Founding ideas above is
detailed at [docs/design/correctness-protocol.md](docs/design/correctness-protocol.md);
the seed-side corollary ("if you're writing `INSERT INTO invoices` in
a seed file, the answer is to fix the Workflow, not the seed") is at
[docs/design/seed-vs-emergent-state.md](docs/design/seed-vs-emergent-state.md).

For the example domain, see [examples/used-device-shop/DOMAIN.md](examples/used-device-shop/DOMAIN.md).
For architecture and the full service map, see [README.md](README.md).

**Stack:** Rust (backend/CLI/agents) · TypeScript/Svelte 5 (frontend)
**Repo strategy:** Monorepo. BOSS core + example tenant share one tree
for now; BOSS may extract to its own repo once interfaces stabilize.

---

## Core Design Principles

### 1. Adaptability First
Every design decision is evaluated by: "How easy is it to change this later?" Prefer small, replaceable modules over large, optimized-but-rigid ones. Delete code freely. Avoid sunk-cost reasoning.

### 2. Simplicity & Maintainability
If it's hard to explain, it's too complex. Fewer abstractions > more abstractions. No premature generalization. Build for what we need now, not what we might need.

### 3. Hexagonal Architecture (Ports & Adapters)
- **Ports** = traits (Rust) / interfaces (TypeScript) that define what the domain needs
- **Adapters** = implementations that plug into ports (DB, API, messaging, etc.)
- Domain logic NEVER imports infrastructure. Infrastructure implements domain traits.
- This is how we stay adaptable: swap adapters, domain stays untouched.

### 4. Immutable Data by Default
- Rust: prefer owned types, `Clone` over `&mut`, `Arc<T>` over shared mutability
- TypeScript: `readonly`, `Readonly<T>`, `as const`, spread-to-update. No mutation of function args.
- State changes produce new values, not mutated old ones.

### 5. Stateless Services
- Services receive everything they need via arguments or events
- No hidden state, no singletons, no module-level mutable globals
- If something must persist, it goes through an explicit persistence port

### 6. Functional Programming Paradigms
- Pure functions as the default unit of work
- Side effects pushed to the edges (adapters)
- Favor `map`, `filter`, `fold` / `reduce` over imperative loops
- Use `Result<T, E>` and `Option<T>` (Rust) — never panic in library code
- TypeScript: discriminated unions over exceptions for expected failure cases

### 7. Async by Default
- Rust: `async fn` with `tokio` runtime. All I/O is async.
- TypeScript: `async/await` everywhere. No sync I/O in server code.
- Design for concurrency: if two things don't depend on each other, they run concurrently.

### 8. Event-Driven Architecture
- Services communicate through events, not direct calls
- Events are immutable facts about things that happened
- Every event has: `id`, `timestamp`, `source`, `kind`, `payload`
- Commands (requests to do something) and Events (facts that happened) are distinct types

### 9. Registries Over Hardcoded Paths
New work types, new step UX, new posting rules — they land as **data in append-only versioned registries**, not as new branches in core code. This is how the system stays adaptable as operational needs evolve. If you find yourself adding a `match kind { "refurb-used" => ..., "sale" => ... }` in core code, there's a registry you should be using instead.

The same principle applies one level down to **taxonomies**. Closed Rust enums for roles, departments, account types, asset models, etc. force every tenant to fork core to add a value. The BOSS answer is the **Class registry** ([docs/design/class-registry.md](docs/design/class-registry.md)) — one `classes` table keyed `(subject_kind, code)` with rows for every taxonomy in the system. Roles are Classes of `employee`-kind Subjects; AccountTypes are Classes of `account` Subjects; catalog asset models are Classes of `asset` Subjects. When you reach for a closed enum to model a tenant-extensible category, reach for the Class registry instead. Move things to data that can be data.

### 9a. A fact that lives twice gets an equality test

Some facts genuinely cannot live in one place: a Rust registry and the
shell script that reads it, a schema manifest and the `include_str!`
list a test harness compiles in. When that happens, **collapse it if
you can, and pin it with a test if you cannot.** A comment asking the
next person to keep two lists in sync is not a mechanism.

This is not theoretical. Three such pairs drifted and each caused a
real failure:

| pair | what broke | now |
|---|---|---|
| `boss-ports` ↔ `deploy-services.sh` fallback arrays | two services silently absent from a deploy | pinned by a test — the fallback must stand alone when the binary is unbuilt |
| `manifest.txt` ↔ `boss-testing::SCHEMA_FILES` | every DB-backed test ran without two tables | **collapsed twice** — `build.rs` generated the list from the manifest, then the manifest itself was deleted and the schema directory became the definition |
| `MODEL_ROUTES` ↔ `MODEL_KINDS` | pages rendered under the wrong tab, silently | **collapsed** — one `nav-catalog.ts` answers both questions |

All three are now either collapsed to one definition or pinned by a
test that names the offending entry when it drifts. Prefer collapsing:
`VENDOR_COUNT` was two hardcoded `13`s under a sync comment and is now
one `pub const`, because one constant cannot drift from itself.

**A pin is a holding action, not a destination.** The equality test is
what you write when you cannot collapse *today*; it stops the drift
but keeps the duplication, and duplication has a running cost the test
does not pay off. `manifest.txt` ↔ `SCHEMA_FILES` is the worked
example, and it took two passes. The pin held for months, then every
new migration had to edit the tail line of both files, and on
2026-08-13 four cars in one day collided there — each collision
costing a re-rail. That bought the first collapse: `build.rs` generates
the Rust list from the manifest, so it cannot drift, and
`include_str!` needing compile-time literals is what a build script is
for.

**That was half a fix, and the half it left was still expensive.** One
file still had a contended tail line, so any two cars carrying a
migration still conflicted on merge — two more were stranded on
2026-08-14 ("left for the next train"). The second collapse deleted
`manifest.txt` outright: the ordered list is `schema/*.sql` sorted by
the `NNN-` prefix, which every reader derives independently. Adding a
migration is now dropping a file in a directory, touching no shared
line at all. The lesson to carry: when a collapse leaves one
authoritative copy that everyone still has to *edit*, ask whether that
copy holds any information its source does not. This one held none.

### 10. Core vs. Example Tenant
The core state-machine OS lives under `crates/core/` (
among them `boss-core`, `boss-events`, `boss-jobs`, `boss-policy`,
`boss-gateway`, `boss-observability`, `boss-cybernetics`,
`boss-docs`, `boss-ml`, `boss-content`, `boss-testing`,
`boss-dispatcher`, `boss-clock`, `boss-expr`, `boss-locations`, the
two taxonomy registries (`boss-classes`, `boss-subject-kinds`),
`boss-calendar`, `boss-nats`, `boss-ports`,
plus matching `*-client` crates). Company-modeling adds the
`crates/modules/` tier (`boss-people`, `boss-messages`,
`boss-ledger`, `boss-commerce`, `boss-inventory`, etc.). The
`crates/tenants/` tier is where each tenant binary lives — today
the brewery (`boss-brewery-engine`) and used-device-shop
(`boss-used-device-shop-engine`). Don't push tenant-specific
assumptions (device refurb, service-account specifics, regulator
quirks) into the core; if a
feature only makes sense for the example tenant, it lives in those
crates.

---

## Primitives

Four interlocking primitives model everything. Stay close to them — new work types and new UX ship as data in registries, not as new core code paths.

### Subjects
The identity-bearing things work is *about*. BOSS treats Subject as a trait: each kind (Asset, Account, Employee, PurchaseOrder, Campaign, Vendor, Custom) implements it with its own KB view. Adding a new Subject kind means a new crate (or a new module in the example tenant) that implements the trait — not a switch in core code.

### Jobs
A **Job** is a bounded unit of coordinated work — a sale, a service visit, a hiring pipeline, an onboarding, a vendor-payment chase. Jobs give every piece of work a stable identity, an owner, a subject, a status, and a structured list of Steps.

The **Workflow registry** (`boss-jobs`, backed by the `workflows` table) is append-only and versioned. Each kind declares:
- `subject_kinds` — what the Job can be about
- `steps` — a flat set of Steps; the DAG is implicit in each step's `ready_when` predicate (an edge A → B exists iff B's `ready_when` references A), not an author-drawn graph
- `metadata_schema` + `entitlements` — typed fields and policy hooks on the Job itself

**Adding a new workflow means adding a Workflow row**, not touching core code. New versions supersede old ones; in-flight Jobs stay pinned to the version they were opened under. Authoring lives at `/system/workflows`.

### Steps
A **Step** is the typed unit of work inside a Job. Each step has a `kind` (from the StepType registry), `status` (pending → ready → active → completed (+ skipped)), optional assignee, `blocked_by` (a predicate-derived denormalized edge list for DAG rendering — recovered from the step's `ready_when` references, not an author-specified gate), optional sign-off, and free-form `metadata`.

The **StepType registry** (`boss-jobs/src/step_registry.rs`) is the alphabet of legal transitions — `scheduling`, `sign-off`, `handoff`, `outreach`, `checklist`, `acknowledgment`, and the rest. Each kind declares a `fields` schema describing its metadata shape.

Two rules shape the contract:
- **Required-at-done, not required-at-create.** A `scheduling` step can exist with no `scheduled_at`; that field is required only when the step flips to `status=completed`. Metadata validators run on completion, not on create.
- **PATCH semantics on PUT.** `PUT /api/jobs/{id}/steps/{step_id}` fetches the current step, overlays the body, then saves. Callers can send `{"status":"completed"}` and keep every other field intact. Clients providing new `metadata` must merge with the existing keys — top-level fields are replaced wholesale, so partial metadata wipes unmentioned keys.

### Events
Every state change emits an immutable fact through NATS (`boss-nats`) and lands in `audit_log` (`boss-events`). **The log is the system of record.** Projections rebuild from it; rebuilders reproduce truth from it; the five-property correctness protocol (provenance, conservation, closure, idempotence, determinism) guarantees the system contributes zero error of its own. Every state-changing operation publishes an event; nothing else.

## Supporting concepts

These three hang off the four primitives. They are load-bearing infrastructure, not foundational vocabulary.

- **Class registry** — typed reference data each Subject kind owns. One `classes` table keyed `(subject_kind, code)` carries every taxonomy in the system: roles (Classes of `employee` Subjects), AccountTypes (Classes of `account` Subjects), asset models, departments, account tiers. See [docs/design/class-registry.md](docs/design/class-registry.md).
- **StepPlugins** — UX extensions on Steps. A plugin is a small JS bundle served by the gateway at `/plugins/<path>` that renders a custom surface for a step kind. Plugins ship as data (a row in `step_plugins`) + a static JS asset; authoring at `/system/step-plugins`. **New step surfaces do not require a core code change in `apps/web`.** Decision record: [docs/architecture-decisions.md](docs/architecture-decisions.md) §Step UX & frontend.
- **Policy** — every write passes through `boss-policy` (via the `PolicyClient` port). Rules are row-level: a rule grants an `(action, resource)` within a `scope`; user-specific overrides take precedence; `policy_rule_audit` tracks every decision.

### How to add a new thing
The shape of an "add a new thing" change follows the primitives:

1. **New work type** → add a Workflow row (authoring at `/system/workflows`), declaring its `steps` (the DAG implicit in their `ready_when` predicates). Usually no Rust code change.
2. **New step behavior** → add a StepType entry (if the schema is new) + a StepPlugin row + a JS bundle. No core frontend change.
3. **New domain entity** → a new crate following hexagonal structure: domain types + traits in `boss-core` or a `*-client` port crate, implementation in the service crate, HTTP surface in the service binary.
4. **New cross-service contract** → extend `boss-core` events or add a `*-client` port shared between consumers.

---

## Rust Conventions

### Structure
Crate organization (the canonical roster lives in
`Cargo.toml` workspace members; the visual map is at
[docs/architecture-diagram.md](docs/architecture-diagram.md)):

Crates live under `crates/<tier>/<name>/` so the tier is visible
in the file tree.
Four tier directories, each with a hard rule about what the tier
can depend on. **Every PR is judged against the audit bar of
the tier it touches.**

- **`crates/core/` — Tier 1: Core state-machine OS**.
  The generic state-machine modeling toolkit. Every BOSS
  deployment ships these regardless of what the tenant models.
  A non-company tenant (research lab, robot fleet, city
  government) exercises these the same way the brewery does.
  **Tightest review bar: the five-property correctness protocol
  is non-negotiable here.** Crates include `boss-core`,
  `boss-events`, `boss-jobs`, `boss-policy`, `boss-gateway`,
  `boss-dispatcher`, `boss-clock`, `boss-expr`, `boss-locations`,
  the two taxonomy registries (`boss-classes`, `boss-subject-kinds`),
  `boss-calendar`,
  `boss-content`, the ML stack, `boss-cybernetics`,
  `boss-testing`, `boss-ports`, `boss-docs`, plus matching
  `*-client` crates.

- **`crates/modules/` — Tier 2: Company-modeling layer**.
  Useful for modeling a company on top of the
  core. A non-company tenant can deploy without these. Same
  hexagonal shape as Tier 1 (domain types + port + HTTP surface
  + projection rebuilder) but the *concepts* are business-shaped,
  not state-machine-shaped. Audit bar inherits the correctness
  contracts of the core but domain-surface changes move at the
  speed of the business. Crates: `boss-people`, `boss-accounts`,
  `boss-commerce`, `boss-inventory`, `boss-shipping`,
  `boss-ledger`, `boss-products`, `boss-messages`, `boss-catalog`,
  `boss-assets`, plus matching `*-client` HTTP-contract crates and
  `boss-ml-plugins`.

- **`crates/orchestrators/` — Cross-tier orchestrators**.
  Binaries that fan out across both tiers by design. The
  Tier-1-must-not-depend-on-Tier-2 rule applies to **libraries**,
  not orchestrators. An orchestrator's purpose IS to wire core
  + module crates together. Same audit-bar status as Tier 1
  (ships with every deployment) but inherently fan-out.
  Crates: `boss-rebuild` (calls every domain rebuilder),
  `boss-cli` (operator commands across domains),
  `boss-sim` (synthetic event generator with tight domain-type
  coupling), `boss-ml-api` (ML HTTP surface that loads
  inference plugins from both tiers), `boss-simulator` (the
  standalone `/simulator` UX — SPA bundle + `/simulator/api/*`
  control/status surface). Step side-effects are
  owned by the core `boss-dispatcher` crate, which subscribes to
  `step.done.<kind>` topics and runs data-driven rules.

- **`crates/tenants/` — Tier 3: Tenants**. Tenant-specific
  binaries. Crates: `boss-brewery-engine` (Algedonic Ales public
  demo) and `boss-used-device-shop-engine` (used-device-shop).
  Data-side seeds + Workflows for the brewery tenant live under
  `examples/brewery/`.

The `infra/lint/tier-import-audit.sh` script enforces the
Tier-1-can't-depend-on-Tier-2 rule (orchestrators excluded);
runs cleanly today (0 violations across 29 core crates).

Each domain crate has a matching `*-client` for cross-service
HTTP calls + a `Pg*` adapter behind the `postgres` feature.

### Style
- `cargo fmt` — no exceptions
- `cargo clippy -- -D warnings` — treat all warnings as errors
- Edition 2024
- Error handling: `thiserror` for library errors, `anyhow` for application/CLI errors
- Serialization: `serde` + JSON for events and API boundaries
- Async runtime: `tokio`
- No `unwrap()` or `expect()` in library code — `Result` propagation only
- `unwrap()` acceptable only in tests and CLI `main()`

### Naming
- Types: `PascalCase`
- Functions/methods: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`
- Crate names: `boss-{name}` (kebab-case)
- Module files: `snake_case.rs`

### Patterns
- Traits define ports: `trait EventStore`, `trait NotificationPort`
- Adapters implement traits: `struct PostgresEventStore`
- Constructor pattern: `Type::new(deps) -> Result<Self>`
- Builder pattern for complex construction
- Newtypes for domain IDs: `struct OrderId(Uuid)`

---

## TypeScript Conventions

### Structure
```
apps/
  web/            # Svelte 5 frontend (Bun + bun-plugin-svelte)
  simulator/      # Simulator UX SPA (served by boss-simulator; shares @boss/web-kit)
```

Type definitions that need to be shared between services live in
the Rust crates and are translated to TypeScript at the HTTP
boundary. There is no `libs/shared-types/` — every domain owns
its own TS types under `apps/web/src/{domain}/types.ts`,
and deserialization happens once at the fetch call site.

### Style
- Strict TypeScript — `strict: true`, no `any`
- svelte-check on the client, `cargo fmt` + clippy on the Rust side
- Svelte 5 with Runes (`$state`, `$derived`, `$effect`) — no stores
- Prefer `type` over `interface` unless extending

### Patterns
- Discriminated unions for state: `type State = { kind: 'loading' } | { kind: 'ready', data: T }`
- Props are `Readonly<{...}>`
- No direct mutation — spread, `map`, `filter` to produce new state
- Custom hooks extract logic from components
- Co-locate: component + hook + types + test in same directory

---

## Testing — Test-Driven Development

We practice TDD. Write the test first, watch it fail, then write the minimal code to make it pass.

The full multi-layer strategy (compile-time → static lints → unit
→ integration → replay-rebuild → continuous integrity in prod →
formal proofs) is documented at
[docs/design/testing-strategy.md](docs/design/testing-strategy.md).
Read that to know which layer a new test belongs in.

### TDD Workflow
1. **Red** — Write a failing test that describes the behavior you want
2. **Green** — Write the simplest code that makes the test pass
3. **Refactor** — Clean up while keeping tests green

### Rules
- No production code without a failing test first
- Tests are first-class code — they deserve the same care as production code
- Unit tests live next to the code (`#[cfg(test)] mod tests` in Rust, `*.test.ts` co-located)
- Integration tests in `tests/` directories
- Test the domain through ports — never through adapters directly
- No mocks unless unavoidable. Prefer in-memory adapter implementations.
- Property-based testing for domain logic where applicable
- Every bug fix starts with a test that reproduces the bug
- If you can't write a test for it, reconsider the design

---

## Design docs

When writing or editing a file under `docs/design/*.md`, follow the in-repo convention: open questions must be authored as `### Qn: <title>` subheadings (not numbered lists), **inside a `## Open questions` section** — the tracker ingests questions from that section only, and the reindex rejects a doc with a live `Qn:` heading anywhere else (resolved ones may live in a Decision-history section). If you use numbered lists instead, the review workflow silently falls back to positional ids and the open questions you wrote don't show up in the UI.

Resolutions flush into the source doc's Decision-history section via the tracker. Each release, settled material folds into [docs/architecture-decisions.md](docs/architecture-decisions.md) — the Baseline Architecture Decisions, the one current-truth decision record — and the flattened source doc is deleted. Docs that survive under `docs/design/` are living references (reading frames, contracts, governance rules), not decision archives.

---

## Git & Workflow

- `main` is always deployable
- Feature branches: `feat/{short-description}`
- Fix branches: `fix/{short-description}`
- Small PRs. If it's hard to review, it's too big.
- Commit messages: imperative mood, concise. "Add order event schema" not "Added order event schema"

---

## Engineering Session Startup — orient before you build

A new session starts blind, and the queue does not un-blind it. **This
protocol exists because the durable pod session — the one meant to make
work smoother — was clunky for hours on 2026-09-01 for want of it:** it
built from a local `main` seven commits behind `origin/main` and so
rebuilt a fix that had already landed; it closed five "open" packets
whose fixes were already on main; and it duplicated a branch that was
sitting green-gated and unparked, because it never read the approach.
Every one of those was visible at startup. Run these first, before
picking up any work:

1. **Vantage.** cwd `/work/boss` = the cluster dev pod (builder,
   gatekeeper, publisher — see §Doors for what that can and cannot do).

2. **Sync to the source of truth.** `git fetch origin`. Branch, edit,
   and diff against **`origin/main`** — never a stale local `main`. A
   fix built on an old base rebuilds landed work, and once parked it
   reverts the trains: a branch on an old base merges clean and wrong.

3. **Read the approach — the whole car pipeline, not just the queue.**
   The Train Yard shows only the last third of a car's life; read the
   rest from the API:
   - In-flight trains — `boss-api GET /api/jobs?kind=pr-train&status=open`
     — and where each sits (CI / merge / deploy).
   - Gate-runs INCLUDING closed ones (`kind=gate-run&limit=40`; a
     gate-run closes on its verdict, so green/red ones are *closed* and a
     `status=open` query misses them). A **green gate-run whose branch
     has no car is stranded** — rescue it (rebase onto current main +
     re-gate, since its base has likely moved) or note it, but never
     rebuild it blind. A **red** may be a superseded earlier attempt, not
     a live failure.
   - Parked cars — `GET /api/stations/loading-dock/queue`.

4. **Before building any packet's fix, verify the claim still holds on
   `origin/main`.** The queue's "open" count is inflated by
   landed-but-unclosed residue. Ask: is this already fixed on main? Is
   there already a branch — stranded green, or in flight — for it? If so,
   close the packet `stale`/`duplicate`; do not rebuild.

5. **The queue is a worklist, not the truth.** Measure current reality,
   not the reality a packet was filed against — the discipline the
   correctness protocol asks of the system, asked of the operator.

**`boss orient` runs steps 2–4 for you** — trains in transit, gates
running, stranded greens with rescue guidance, the dock, and the task
queue, one read (needs `BOSS_JOBS_URL`, like every SoR verb). Run it
first; this section is the checklist behind it, and the reason each
line exists. On its first live run it named three stranded greens —
one of which was rescued onto the next train instead of rebuilt blind.
Residue auto-detection (the L3 half) is still design work on acedf981.

## Diagnosis — what a stopped pipeline owes you

On 2026-09-02 delivery stopped for most of a day, and much of that time
went to questions the system already held the answers to. The lessons
below are not about the individual faults (a bad boot, a full disk);
they are about **how much re-derivation a failure is allowed to cost.**
Each is now a defect class with a fix, and each is worth checking any
new surface against.

- **A verdict must name what failed.** A red train recorded
  `?:SUCCESS, ?:SUCCESS, ?:FAILURE`. Learning that the failing job was
  `test`, and that `test` had died on a disk floor rather than on any
  code, took three calls to the forge API — the adapter had been
  dropping the check's name one layer below the code that wanted it.
  A verdict someone must go re-derive is not a verdict.

- **An infrastructure refusal is not a consist failure.** The gate's
  disk floor refuses *before any check runs* — a correct refusal that
  says nothing about the branch. Recorded as a plain CI failure it
  strikes every car aboard, and two strikes hold a car out of the
  queue until a human looks. The same thing happened on 2026-08-22 and
  cost four clean cars five departures.

- **A troubled packet must look troubled.** A train wedged four hours,
  with an urgent overdue alarm already filed, still rendered in the
  yard exactly like a healthy two-minute transit; two more sat at a
  step they could never complete. If a state has crossed its own alarm
  threshold, the surface showing it must say so — an alarm packet
  existing elsewhere is not the same as the thing looking wrong.

- **An alarm that reports through its subject dies with it.** The
  estate chain observes, compares and files, all through the jobs API.
  During the outage it was silent about an outage. A monitor needs a
  path to a reader that does not depend on what it watches, or at
  minimum must retain and replay so the gap is visible afterwards.

- **A check nobody reads is a check that is not running.** The audit
  integrity alarm exited nonzero nightly over benign sequence gaps
  until it was correctly demoted to a warning. Then a real finding — an
  emitted-but-undeclared event kind — rode inside a *passing* run for
  days, unread. Permanently-red and green-with-warnings fail the same
  way: decide where a warning is read, or do not emit it.

- **"Roll back" is a target, not a verb.** `rollout undo` moved between
  two revisions carrying the *same* broken image, so the first rollback
  was a no-op that read as a rollback. Roll to the last known-good
  artifact, named and verified — never to "the previous one".

- **Mechanical operations belong to the machine.** Five times in one day
  an operator was the transport for a command whose output the system
  could have read itself. The foothold already exists — the cadence loop
  runs supervised on each host and takes its schedule from registry
  data — and the only gap is that its verb vocabulary is closed
  (`ee8ec68a`). Reads and bounded reclaims are mechanical;
  destructive-by-policy actions are not, and keeping that line sharp is
  what makes handing over the first kind safe.

**What held, and is worth protecting:** the seed's baseline guard
refused to stamp over a failed prepare and saved the tenant model; the
trains refused to claim convergence they could not evidence and filed
loud packets instead; the disk floor refused rather than wedging a
host. Each is a component choosing to stop rather than guess. The
failures above are the same shape in reverse — a component that
answered instead of erroring, or waited instead of speaking.

## Doors — the supported way in

A door is a path already made safe: correct target, correct actor,
pre-approved so it does not prompt. **Every expensive mistake this
pipeline has made was building a path by hand when a door existed**, so
the doors are listed here, in the one document every session reads.

This list is load-bearing for how much gets delegated. David,
2026-08-28: *"As we get more confident that the protocol prevents big
mistakes, we can let you handle more steps. The protocol is our
constraint."* An agent is trusted with a step because the protocol makes
that step hard to get wrong — not because it promised to be careful. So
a door that stops being true is a defect worth a car.

- **The jobs API — `boss-api METHOD /api/path [body.json]`**
  (`/Users/david/bin/boss-api`). Pinned to the system of record, signs
  as the session's own actor, allowlisted so it never prompts. Invoke it
  **bare**: `boss-api GET … > file` stays inside the allowlist, `boss-api
  GET … | python3` falls out of it and gets adjudicated. Speaks
  GET/POST/PUT/PATCH/DELETE. **Annotate a packet with `PATCH
  /api/jobs/{id}/metadata`** — it MERGES top-level keys, and a key set to
  `null` is deleted (both verified against a live packet, 2026-08-28).
  Prefer it over a full job PUT: the PUT is safe, but it REPLACES, so
  every annotation depends on reconstructing the whole job body
  correctly. The step API refuses metadata writes to a completed step and
  its 409 names this endpoint as the way to annotate instead — which is
  how it was found, in the conductor's journal, failing every ten minutes
  for weeks (`f402a681`). On 2026-08-27 a session made ~28 hand-built
  calls before finding this door at all, ~14 of them writes carrying a
  forged `emp-david` actor, so the audit log credits a human with an
  agent's work.

- **Which deployment.** The system of record is
  **`http://10.20.0.34:7900`**. boss-gcp's `127.0.0.1:7900` is a
  *second, older, complete stack* with different data. The conductor's
  systemd unit sets `BOSS_JOBS_URL` explicitly for this reason; a verb
  run by hand inherits no unit.

- **Before pushing — `infra/gate.sh --quick`.** fmt plus every
  build-free lint, ~11s. It is not a gate and says so. Skipping it once
  cost 17 minutes of cluster time to learn that `cargo fmt` had been run
  on one crate and not another.

- **Gating a branch — `boss gate <branch> [--wait]`.** Files or reuses
  the packet, renders the runner, creates the Job. `--wait` polls to a
  verdict; hand-rolled pollers have been written three times and two
  were broken.

- **Publishing a branch to the forge.** A workstation has no forge
  credential. Push to the conductor clone (`gcp` remote) under
  `refs/tmp/*`, push that to `origin` from there, delete the temp ref.
  Never push a car branch to a push-mirror target — it force-syncs from
  the forge and the branch disappears.

**And the rule behind all of them: a wrong target answers instead of
erroring.** A query against the wrong deployment returns `total: 0`. A
non-existent systemd unit is `inactive`. A service checked on the wrong
host is `inactive`. All three happened in one session, all three are
well-formed and confident and wrong. Before concluding something *does
not exist*, run a query on the same connection whose answer you already
know.

---

## What We Don't Do

- No ORM magic — explicit queries, explicit mapping
- No global mutable state
- No inheritance hierarchies — composition via traits/interfaces
- No "just in case" code — delete it; the working tree is the canonical record
- No framework lock-in — frameworks are adapters, not architecture
- No blocking I/O in async contexts
- No bespoke workflow code paths — new work types are **Workflow rows**, not `match` branches in core code
- No bespoke step UX in `apps/web` core — new surfaces ship as **StepPlugin rows + a JS bundle**
- No tenant-specific assumptions in BOSS core — keep used-device-shop logic in the example crates
