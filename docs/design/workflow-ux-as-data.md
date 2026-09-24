# Design: workflow UX as data — when vanilla is the exception

**Status**: decided — all questions answered by David in review `b4cc0406`, 2026-08-19.
**Source**: David, 2026-08-09: "having vanilla UX for a workflow or
step is going to become the exception rather than the norm… my
department app will want to build custom views for the workflows we
engage with, to triage efficiently via drag and drop… and useful
Step UX for any real work that needs to occur during a step. That
way I can pull in and render all the useful contextual data riding
along with the job."
**Related**:
[architecture-decisions.md](../architecture-decisions.md) §Step UX &
frontend (which also holds the folded Views / department-apps
decisions) · [queue-visibility.md](./queue-visibility.md) ·
[public-api-mcp.md](./public-api-mcp.md)

---

## The claim, named

The StepPlugin decision already settled this question for *steps*:
surfaces ship as data (a registry row + a JS bundle), the SPA never
changes, the generic card is the fallback. David's claim extends it
one level up — *workflow-scoped* views (a department's triage board,
a drag-and-drop queue, a kind-specific pipeline) will outnumber the
vanilla surfaces too. The core surfaces that shipped this week
(TriageBoard, TriageFlow, Fleet) should be read as the **floor**:
always present, always correct, and increasingly not what a
department actually works in.

## What exists

- **StepPlugins** — the settled per-step mechanism: append-only
  versioned `step_plugins` rows; bundles served at `/plugins/<path>`;
  steps pin the plugin version at creation; framework-free mount
  (`window.__boss_register_step_plugin(kind, mount)`;
  `mount(container, {step, jobId, onUpdate, currentUser})`); the
  platform surface, then the generic fields/notes card, as fallback.
- **What the host does NOT provide**: an API client. Every plugin
  fetches for itself — including the step PUT whose top-level
  metadata replace would wipe unmentioned keys, and since e39a9d2a
  refuses a body that omits one (409, naming the step merge door,
  `PATCH .../steps/{step_id}/metadata`). The merge-safe discipline
  is documented in TriageBoard's `patchStep` because it once made
  cards vanish; today each plugin must rediscover it.
- **Core generic surfaces** — TriageBoard (fork columns), TriageFlow
  (per-step queues on the DAG, routing by edge — parsed from
  `ready_when`), Fleet (per-step depth for any kind). All core code.
- **The kit fragments** — the rules that have already drifted once
  and now live exactly once: `fork.ts` (which step carries the
  decision), `position.ts` (which node a Job sits at — pinned to the
  server's grouping), `workflowToDag` (edges + routing conditions),
  `StepDag`. These are the pieces a custom view would otherwise
  reimplement wrong.
- **The Views decision** (2026-08-05): local scratch state is fine
  while it stays local; the moment it flows into a Job, Step, or
  Event it is subject to the same rules as everything else.
- **Server-side guardrails**: policy on every write, `ready_when`
  gating, required-at-done validation. A view cannot invent a
  transition — the server refuses moves the Workflow doesn't declare.

## The shape

Extend the StepPlugin principle to workflow-scoped surfaces: a
**workflow-view registry** — kind-keyed, append-only versioned rows
naming a surface slot (`queue` | `board` | `flow` | detail panels)
and a bundle — mounted by the same host pattern, with the core
generic surfaces as the permanent fallback. A department app then
*is* data all the way down: its nav entries (already registry-shaped),
its workflow views (rows + bundles), its step surfaces (StepPlugins),
its saved Views. Core ships instruments and the kit; departments
ship views.

## The considerations

1. **The state machine is what makes custom UX safe.** Drag-and-drop
   routing is presentation over the same legal transitions — dropping
   a card on "build" completes the fork step with
   `disposition = "build"` through the same PUT everything uses, and
   a drop the predicates don't allow is refused by the server, not by
   the view's good manners. Custom views can be arbitrarily wrong and
   the model stays right. This is the whole reason the norm *can*
   flip to custom: the alphabet is enforced below the UX.
2. **The host must hand plugins the sharp-edged primitives, once.**
   The metadata-merge PUT, fork reading, position grouping, the DAG
   with routing edges — every one has a drift incident behind it.
   A plugin contract that provides a small client (`readJob`,
   `patchStep` merge-safe, `route`, the kit components for those who
   want them) turns "every plugin rediscovers the trap" into "the
   trap is unreachable." This is the single highest-leverage decision
   in this doc.
3. **Contextual data is already riding along; the contract is the
   schema.** `metadata_schema` on the Workflow and the StepType
   `fields` registry describe exactly the typed data a view can
   render. A schema-driven renderer in the kit covers the long tail;
   bespoke rendering is for the cases that earn it.
4. **Published contracts are promises** (the MCP doc's Q5, arriving
   here early): plugin props and the provided client version like the
   schema layer — additive freely, destructive in two steps. Views
   pin versions the way steps pin plugin versions today.
5. **Vanilla as floor is a guarantee, not a default.** The generic
   surfaces must always render every kind — the auditor's view, the
   debugging view, the view that works when a bundle is broken.
   Custom views are additive; nothing may *depend* on one existing.
6. **CI has to crawl what it can't import.** The route-smoke crawl
   covers core surfaces; plugin bundles are opaque to it. Plugin
   surfaces need their own smoke contract (a mount-under-adversarial-
   mock harness, or a deferred-with-reason discipline) or the "norm"
   becomes a fleet of unguarded UX.

## Open questions

None — every question was answered in review `b4cc0406` on 2026-08-19; see Decision history.

## Decision history

**Q2 — What exactly does the host hand a view (decided by David in review `b4cc0406`, 2026-08-19).**
the step-plugin props shape, plus a provided client — `readJob`, merge-safe `patchStep`, `route(edge)`, `listAtNode` — and optional kit components (StepDag, the schema renderer). Decides consideration 2. Sub-question: does the existing step-plugin host retrofit the same client, so the trap closes everywhere at once?

**Q3 — Is drag-and-drop a kit interaction or per-view freedom (decided by David in review `b4cc0406`, 2026-08-19).**
**kit component first, per-view freedom allowed** — the compose option the paragraph above already names, chosen deliberately rather than by default.

**Q4 — How do plugin surfaces get gated in CI (decided by David in review `b4cc0406`, 2026-08-19).**
**the plugin smoke harness, in the CI-gated mocked suite** — and the measurement says this is not a close call.

**Q5 — Who may publish a view, and as whom (decided by David in review `b4cc0406`, 2026-08-19).**
**a policy rule on `(publish, ui-plugin)`, and nothing else** — the same answer David gave to the same question one door along.

**Q1 — One UI-plugin registry, or a sibling table per slot? (resolved before this review; carried from Open questions).**
Resolved 2026-08-16 — override.

**The question was:**

> `step_plugins` could generalize to a `ui_plugins` registry with a
> `slot` column (`step:<kind>` | `workflow:<kind>:<surface>`), or a
> sibling `workflow_views` table keeps the two lifecycles apart. One
> registry means one authoring surface, one version discipline, one
> serving path; the split means simpler rows. Precedent (the Class
> registry: one table, kind-keyed) leans single-registry.

One registry. `step_plugins` (03-jobs.sql) is the single table and carries nine active rows today. The sibling-table-per-slot alternative was never built and nothing has needed it since.

