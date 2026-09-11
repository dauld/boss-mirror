# Extending BOSS — Workflows, StepTypes, and Step UX plugins

**Status**: stable — describes the Workflow v2 extensibility model.
**Audience**: anyone modeling a new workflow on top of BOSS, or
adding a new bespoke step surface to the SPA.

This doc ties together three **extensibility registries** —
**Workflows**, **StepTypes**, and **Step UX plugins** — and explains
the extensibility ladder operators climb when they need BOSS to do
something new. The detailed technical references live alongside:

- `../architecture-decisions.md` §Step types are property bundles
  + §Step UX & frontend — the decision record for the StepType
  registry and the plugin model.
- `infra/step-plugins/README.md` — author-facing tutorial:
  build, register, and deploy your first plugin from scratch.

---

## The picture

BOSS models a company as a state machine. The four foundational
primitives are Subject, Job, Step, and Event (see [CLAUDE.md](../../CLAUDE.md#primitives)
for the full vocabulary). This doc focuses on the **registry layer**
that sits on top of them:

```
Subject  — identity-bearing thing the work is about
            (an account, an asset, an employee, a campaign)
Job      — bounded unit of coordinated work, anchored on a Subject
Step     — typed transition inside a Job
StepType — entry in the registry that defines a Step's metadata
            schema + default UX (the alphabet of legal transitions)
Workflow  — entry in the registry that defines a workflow as a
            flat set of Steps + metadata schema + entitlements (the
            program written in the StepType alphabet)
```

A Workflow lists a flat set of Steps, each gated by a `ready_when`
predicate; the DAG is implicit in those predicates (an edge A → B
exists iff B's `ready_when` references A), not an author-drawn
graph. A Step's `kind` points at a StepType, which dictates the
metadata schema, the default UX treatment, and any side effect the
dispatcher fires on completion.

Layered on top: **Step UX plugins** let an operator override the
default render for a StepType (or a Workflow step) with a custom
surface — a checklist, a launch calendar, a triage form, a
per-tap quality check.

---

## The extensibility ladder

When you reach for an extension, you climb only as far up the
ladder as you actually need:

| You want to… | You author… | Code? | Where? |
|---|---|---|---|
| Run an existing workflow against a new Subject pool | Just open a Job | No | SPA, `/ux/jobs` |
| Compose a new workflow from existing StepTypes | A Workflow row | No (data only) | `/system/workflows` |
| Add a new step kind with a custom UX surface | A StepPlugin (JS bundle) | JavaScript only | `infra/step-plugins/` |
| Add a new domain entity (Subject kind) | A new crate | Rust | `crates/modules/` |
| Add a new event topic + projection | Cross-service contract | Rust | `crates/core/boss-core` events |

**Most extensions stop at level 2 or 3.** New domain crates are
rare and only justified when you're modeling a fundamentally new
*kind* of thing the platform doesn't already cover.

---

## Level 2 — Workflow authoring

A Workflow is a row in the `workflows` table (authored at
`/system/workflows`). It declares:

- `kind` — unique slug (e.g. `wholesale-keg-order`).
- `subject_kinds` — which Subject types the Job can target.
- `step` (one block per Step) — each declares a `title`, a
  StepType `kind`, and a `ready_when` predicate over the
  workflow's state. The DAG is recovered from the predicates;
  there are no author-drawn tier numbers or `blocked_by` edges.
- `metadata_schema` — typed fields on the Job itself.
- `entitlements` — policy hooks (who can open / close).

Every step references an existing StepType by its `kind` string.
**Adding a new workflow does not need new code** — you compose
what's already in the StepType registry.

Example brewery Workflows:

- `morning-brew` — daily production cycle on the brewhouse.
  Steps: `trigger` (periodic) → `demand-gate` (demand-check,
  reads finished-goods inventory and forks) → `scheduling` →
  `production-consume` → `task` × several → `production-produce`
  → two `outcome` steps (`brew-completed` and `day-skipped`).
  The fork is a metadata predicate, not a step-kind change: the
  brew path's steps are `ready_when` the demand-check completed
  with `metadata.outcome = "brew"`, and `day-skipped` is
  `ready_when` it completed with `metadata.outcome = "oversupply"`.
- `wholesale-keg-order` — recurring keg order from a
  wholesale customer. Steps: `trigger` (event-driven by
  customer order) → `intake` → `acknowledgment` → `scheduling`
  → `task` → `task` → `shipment` → `billing` → `outcome`.
- `sale` — wholesale account acquisition. Steps: `trigger`
  (operator-driven) → `outreach` → `qualification` →
  `scheduling` → `outreach` → `quote` → `approval` → `outcome`
  (`Account onboarded`).
- `ap-payment-run` — daily bill-payment batch. Steps: `task`
  (pull due-bills aging report, the entry point — its
  `ready_when` is `"true"`) → `bill-payment-batch` (settle
  every approved vendor invoice; emits the audit events that
  drive DR 2100 AP / CR 1000 Cash) → `task` (record check-run
  provenance). Mirrors how a real bookkeeper closes the day's
  AP cycle.

### Trigger + outcome conventions

The `boss-jobs` Workflow lint (`workflow_lint.rs`) enforces two
structural rules at load:

- **Triggers are the `ready_when = "true"` steps.** A Workflow's
  trigger steps are the entry-point alternatives — different ways
  the same workflow can open (periodic schedule fires, an external
  event arrives, an operator opens it manually, another Job spawns
  it). Their predicate is `"true"`, so the materializer marks them
  `Ready` at Job open; they represent a present fact (what opened
  this Job), not work to do. A Workflow needs at least one.
- **Outcomes are the steps carrying a `terminal` marker.** A
  step authored with `terminal = { outcome = "..." }` is a legal
  closure shape. Reaching `Completed` on it closes the Job and
  stamps that outcome label. Multiple terminals per Workflow is
  normal (e.g. `brewed` + `skipped`): authoring two models a
  branching workflow whose decision point isn't a step-kind
  change — it's a metadata fork. Point each branch's `ready_when`
  at the deciding step's outcome metadata (e.g.
  `steps.demand-check.metadata.outcome = "oversupply"`) so the
  implicit DAG renders the fork at the right place. A Workflow
  needs at least one terminal.

The lint also proves every step is reachable forward from some
trigger and backward from some terminal — no dead code. A Workflow
that fails any of these checks does not load.

### Provably-skipped branches

There is no `Aborted` status. When a branch is not taken — its
`ready_when` becomes provably false-forever (every step it
references reached a terminal state and the predicate still won't
hold) — the re-evaluator flips it `Pending → Skipped`, iterating
to a fixpoint so the skip cascades down the dead branch. Once one
branch reaches a terminal `Completed`, the Job closes and the
siblings settle as `Skipped`, so a Job's open-step count drops to
zero naturally instead of parking a dead branch forever.

The brewery's `seeds/workflows.toml` is the worked-example for a
tenant whose Workflows get published from data on first load (via the
converged prepare step, `boss-brewery-sim prepare`).

---

## Level 3 — StepType + Step UX plugin

When the StepType registry doesn't have what you need, you climb
to level 3. Two pieces ship together:

1. **The new StepType.** A `[[step_type]]` block that names the
   step kind, declares its metadata schema (`fields`), and picks
   a UX treatment (inline / expanded / full-screen). The catalog
   ships as data in `crates/core/boss-jobs/seeds/step_types.toml`,
   which `step_registry.rs` `include_str!`s into the binary and
   parses once at first registry call. The
   StepType row is pure schema; any side effect a step fires on
   completion lives in a dispatcher rule keyed `step.done.<kind>`
   (a row in the `dispatcher_rules` registry, seeded from
   `infra/dispatcher/rules/`), not on the StepType itself.

2. **The Step UX plugin.** A plain JS bundle (IIFE) that calls
   `window.__boss_register_step_plugin(kind, mount)` on load.
   When the SPA renders a step whose `kind` matches your `kind`,
   the host fetches the bundle, calls your `mount(container,
   props)`, and hands you a DOM element to render into. You bring
   whatever rendering tech you like (vanilla DOM, lit-html, a
   bundled micro-library); the host ships zero framework runtime.

The plugin registry is a row in `step_plugins` keyed `(kind,
version)`, with a `frontend_url` naming the bundle — operator-
authored at `/system/step-plugins` or canonical-seeded via SQL.
`boss-gateway` serves bundles from `/var/lib/boss/step-plugins/`
at `/plugins/*`. The full
walk-through of authoring + deploying a plugin from scratch
lives at `infra/step-plugins/README.md`.

---

## The shipped StepType catalog

The catalog is the **alphabet** of legal step transitions. v1
ships 43 kinds across 5 categories (Coordination, Operations,
Commercial, Logistics, Admin). The TOML rows are pure schema —
which kinds need a module-tier service is no longer baked into the
registry. A kind counts as "company-modeling" when a dispatcher
rule's `on_event` matches `step.done.<kind>`
(a row in the `dispatcher_rules` registry); deploy it without that service
running and the step still completes, but the downstream
projection is missing. The other kinds (generic transitions,
coordination gates, acknowledgment-style data templates) fire no
side effect and are safe for any tenant, company or not.

The canonical roster + each kind's metadata schema lives in
`crates/core/boss-jobs/seeds/step_types.toml`. A Markdown mirror
of the catalog drifts against the TOML within weeks — read the
TOML source, and don't maintain a copy.

---

## The two contracts every Step honors

Two rules across every StepType — they're worth committing to memory:

1. **Required-at-completion, not required-at-create.** A
   `scheduling` step can exist with no `scheduled_at`; that field
   is required only when the step flips to `status=completed`.
   Metadata validators run on completion, not on create. This lets
   you open a Job with a placeholder step list and fill in the
   typed metadata as the work happens.

2. **PATCH semantics on PUT.**
   `PUT /api/jobs/{id}/steps/{step_id}` fetches the current
   step, overlays the body, then saves. Callers can send
   `{"status":"completed"}` and keep every other field intact.
   Clients providing new `metadata` must merge with the existing
   keys themselves — top-level fields are replaced wholesale, so
   partial metadata wipes unmentioned keys.

Both contracts are enforced server-side; UI plugins can rely on
them.

---

## When to climb past level 3

Level 4 (new domain crate) is rare. Reach for it when:

- You're modeling a **new Subject kind** that the existing
  registry doesn't cover (and that's not just a new
  `subject_kinds.custom_kind` value, which is data).
- You're wiring a **new external integration** that needs its
  own port + adapter + service.
- You're owning a **cross-cutting projection** the existing
  domain crates don't host (a new financial fact kind, a new
  tax filing rule).

In any of those, the new code is its own crate under
`crates/modules/<name>` (or `crates/tenants/<name>` for
tenant-specific pieces) following the hexagonal shape: domain
types in the crate, port traits in `*-client`, Pg adapter behind
a `postgres` feature, HTTP surface in a binary. The brewery's
`boss-brewery-engine` is the worked example.

---

## What stays load-bearing

Adaptability lives in **registries, not branches**. If you find
yourself adding a `match kind { ... }` in core code to handle a
new tenant's flavor, the answer is almost always "lift this into
a registry the tenant authors against." The Workflow registry is
data the tenant authors; the Class registry handles tenant-varying
taxonomies (roles, account_type, asset-model categories) as data;
the StepType catalog ships as `step_types.toml` (still
`include_str!`'d at compile time — runtime plugin-style hot-load is
the remaining step).

The further down the ladder you stay, the easier it is to evolve.
Build for what the work needs now; pull a primitive up the
ladder when the cost of the registry is less than the cost of
duplication.
