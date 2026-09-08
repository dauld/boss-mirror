# Design: Protocol, Policy, Publish — the network's API

**Status**: decided — all 6 questions resolved 2026-08-12 via the in-app tracker; see Decisions.
**Origin:** David, 2026-08-12 (verbatim, feedback `63bf89d1`): "whether
we should have a Protocol, Policy, and Publish service that is
essentially the API for the network. It can evaluate the payload
against the protocol, ensure policy is met, and then publish to the
proper queue. We should be able to shrink the need for the dispatcher
to have a bunch of automated rules, which are now encoded in the
protocol/workflow definition."
**Related**: [job-packet-network.md](./job-packet-network.md) — the
packet model this is the API for ·
[requirements-based-addressing.md](./requirements-based-addressing.md) —
Q2's evaluator is this doc's evaluator ·
[transactional-audit-log.md](./transactional-audit-log.md) — the
staging discipline every consequence must ride ·
[human-powered-state-machine.md](./human-powered-state-machine.md)

## The claim

The network gets one admission edge. A write arrives carrying a packet
mutation; **Protocol** evaluates it against the packet's pinned
protocol set (may this transition happen, and what follows from it);
**Policy** checks the actor may perform it; **Publish** stages the
consequences — queue placement, notifications, spawns, obligations —
in the same transaction as the write. Reaction survives only where
admission cannot see: timers, external ingress, and cross-protocol
reactors.

This inverts the dispatcher's contract. Today the write lands, events
emit, and 38 data rules *react* — at-least-once, milliseconds-to-
seconds later, in a separate process whose rules are data but whose
relationship to the protocol is folklore. Under 3P the protocol
definition itself declares its consequences, and admission computes
them synchronously with the fact they follow from.

## Two-thirds of 3P already exists

The boss-jobs write path is the prototype, unnamed:

- **Policy** — every write already passes `policy_glue` through the
  `PolicyClient` port before touching a row. This half does not move;
  it gets named.
- **Protocol (the "may" half)** — `ready_when` is already evaluated
  at admission: the two-pass atomic materialization promotes steps to
  ready inside the write transaction and emits `step.ready.<kind>`
  markers transactionally. The alphabet check (StepType), the
  required-at-done field validation, and `workflow_lint`'s viability
  proof are all protocol conformance running at write or authoring
  time.
- **Publish** — the transactional outbox is the publish half: every
  consequence the write path computes already stages in the same
  transaction and reaches the log and the bus through the single
  relay.

The missing third is **protocol-declared consequences**: what today
lives in `infra/dispatcher/rules.toml` as reactions keyed on
`step.done.<kind>` topics. The precedent that they belong in the
protocol already shipped: `on_complete_create` is a consequence
declared in the WorkflowSpec — a cross-protocol spawn the definition
owns, not a rule the dispatcher owns.

## The rule census — what moves, what stays

38 event-triggered rules today (the schedule runner has zero):

- **Jobs-internal consequences (7)** — `jobs.spawn` (3),
  `jobs.complete_step`, `jobs.clear_waiting`, `jobs.subjob_resolve`,
  `docs.flush_queue` + `messages.notify` (3) alongside. Same service,
  same transaction: these move into the protocol definition whole,
  and come out *stronger* — exactly-once by construction instead of
  the JetStream consumer's at-least-once, and replay-deterministic
  because the consequence is in the log beside its cause.
- **Domain effects of protocol steps (~22)** — `inventory.*`,
  `ledger.*`, `products.*`, `commerce.*`, `people.*`, `shipping.*`,
  `packaging.*`, `gate.resolve`: "the brew step completing MEANS
  parts were consumed." These are protocol consequences too, but the
  execution is another service's transactional write — admission
  cannot reach into a second database. Split the decision from the
  delivery: admission stages an **obligation event** (the protocol
  says X must now happen), and the existing handler machinery becomes
  the executor draining obligations instead of interpreting rules.
  The obligation is exactly-once in the log; delivery stays
  at-least-once with the handler's existing idempotency guards.
- **External glue (9)** — `webhook.notify`: egress to systems that
  are not on the log. Stays reactive, alongside the two other
  survivors: wall-clock timers (the maintenance family's ExecStartPre
  wrapper, deliberately not dispatcher rules) and external ingress
  (webhook receivers, the future `forge.*` events).

End state: `rules.toml` shrinks to the reactive residue, and every
remaining rule carries a sentence naming why it cannot be a protocol
consequence — enforced by a ratchet in the outbox-migration-ratchet
mold, so the count only goes down.

## Constraints this must respect

- **The staging discipline is non-negotiable.** A consequence
  computed at admission ships via `record_event_in_tx` in the write's
  transaction — never a post-commit call. The 2026-07-13 incident
  class and its CI ban apply with more force here, not less: 3P
  concentrates the emit sites.
- **Policy stays in boss-policy.** 3P *consults* policy at admission;
  it must not become a second authorization surface. The same warning
  requirements-based-addressing gives address predicates applies to
  protocol consequences that touch other actors' work.
- **Pinning governs evaluation.** A packet is evaluated against the
  protocol set fixed on its envelope at creation — a republish
  changes nothing for in-flight packets, and changing a live packet's
  governance is a **translation** (job-packet-network Q3, revised):
  a new packet under the new set, admitted through this same edge
  with a `translated_from` edge back. Admission therefore composes a
  protocol set exactly once per packet — at creation — which is what
  keeps the evaluator simple.
- **Protocol changes are routing changes.** The workflow registry's
  draft/publish/bootstrap writes are un-evented today. Under 3P a
  protocol edit *is* a network configuration change; landing registry
  writes on the outbox is a prerequisite, not a nicety.
- **One evaluator.** `ready_when`, address predicates
  (requirements-based-addressing Q2's set-valued extension), and
  consequence conditions should be one `boss-expr` surface. Two
  expression languages at one admission edge is the fact-lives-twice
  failure with a parser.

## Open questions

All 6 open questions were resolved 2026-08-12 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q1: New service, or boss-jobs promoted? (resolved)

Resolved 2026-08-12 — accept.

Promote in place: the admission edge is a named role - extract a boss-admission crate boundary; no network hop inside the write transaction; a standalone service waits for a real second writer.

**Rationale:** David reviewed and accepted 2026-08-12; recorded by claude:fable.


### Q2: Where do consequences live in the protocol definition? (resolved)

Resolved 2026-08-12 — accept.

Consequences live in WorkflowSpec on-blocks per step transition (notify/spawn/assign/obligation rows, optional when), versioned with the workflow; seed by migrating the seven jobs-internal rules; workflow_lint proves every consequence resolvable at authoring time.

**Rationale:** David reviewed and accepted 2026-08-12; recorded by claude:fable.


### Q3: Are consequences computed sync and delivered async? (resolved)

Resolved 2026-08-12 — accept.

Sync decide, async deliver: admission computes and stages the consequence set transactionally; jobs-internal consequences apply in the same tx; cross-domain effects become obligation events drained by the existing handler machinery.

**Rationale:** David reviewed and accepted 2026-08-12; recorded by claude:fable.


### Q4: What does the queue-placement half publish? (resolved)

Resolved 2026-08-12 — accept.

Admission resolves the next station, records requirement-addressed pools as events (determinism rule), and emits the existing ready/assigned markers; the queue lens reads what admission published - no new placement state.

**Rationale:** David reviewed and accepted 2026-08-12; recorded by claude:fable.


### Q5: What is the migration path — and its ratchet? (resolved)

Resolved 2026-08-12 — accept.

Three shippable stages: (1) extract the admission crate, no behavior change; (2) migrate the seven jobs-internal rules one car each, ratchet tightening with each (ratchet shipped 2026-08-12, baseline 38); (3) convert domain handlers to obligation-drainers kind by kind, inventory first. External glue and timers never migrate - the ratchet allowlist names them.

**Rationale:** David reviewed and accepted 2026-08-12; recorded by claude:fable.


### Q6: Does 3P admit non-Job writes? (resolved)

Resolved 2026-08-12 — accept.

Scope 3P to packet admission (jobs) in v1; other domains keep their write paths until the everything-is-a-packet horizon is decided. No quiet annexation of other front doors.

**Rationale:** David reviewed and accepted 2026-08-12; recorded by claude:fable.
