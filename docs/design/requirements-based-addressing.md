# Design: requirements-based addressing — queues as predicates, protocols as data

**Status**: decided — all 6 questions resolved 2026-08-14 via the in-app tracker; see Decisions.
**Origin:** David, 2026-08-11: "we let people define addresses based on
requirements instead of a known destination, and then the protocol
(defined now as a workflow) facilitates the movements of the payload."
And: "An actor can forward a job to a requirements-based queue that can
literally be constructed on the fly. We can then construct the pool of
the actors that meet the requirements and can manifest the queue payload
availability to them somehow. At worst, we should be able to surface
queues that aren't being drained for someone to make a decision about
how to handle."
**Related**: [stations.md](./stations.md) — the network's nodes ·
[human-powered-state-machine.md](./human-powered-state-machine.md) ·
[class-registry.md](./class-registry.md) ·
[correctness-protocol.md](./correctness-protocol.md)

## The inversion

A queue today is a thing you create and then fill. This doc proposes the
opposite: **a queue is a predicate, and it exists exactly as long as
something satisfies it.** You do not route a payload to a destination;
you state what the payload requires, and the pool of actors meeting
those requirements *is* the queue.

This is the same move `ready_when` already made one level up. Edges
between steps used to be drawn; now the DAG is implicit in each step's
predicate, and an edge A → B exists iff B's `ready_when` references A.
Nobody maintains a graph. Requirements-based addressing applies that to
the other half of a hop: not *when* may this payload move, but *where
may it move to*.

The payoff is that protocol composition becomes an operator rather than
a feature. If an address is a predicate over candidates, layering a
second protocol onto a payload in flight is **conjunction**: a
confidentiality protocol ANDs a clearance requirement and narrows the
pool; a legal-preservation protocol ANDs a retention obligation and a
hold. Neither needs routing code of its own. "Add a compatible protocol
as a payload travels" and "address by requirements" are the same idea
stated at two altitudes.

## What exists today, grounded

Three layers carry a payload through the system. Addressing is in the
only one that is not data:

| Layer | Lives in | Defines | Data? |
|---|---|---|---|
| Workflow | `workflows` table | **when** a payload may move (`ready_when`) | yes |
| Dispatcher rules | `infra/dispatcher/rules.toml`, 80 rules | **what fires** on an event (`on_event` → handler) | yes |
| Handlers | compiled Rust | **who receives it** | no |

Verified 2026-08-11: a workflow step's keys are `authority_role`,
`fields`, `kind`, `metadata_defaults`, `ready_when`,
`sign_offs_required`, `terminal`, `title`, `title_template`. There is no
assignee field of any kind. Dispatcher rules are `on_event` +optional
`when` + `do.handler`, dispatching to named Rust functions
(`messages.notify`, `inventory.po.place`). The routing decision — the
address — is the one thing that escaped into code.

So this is not a new subsystem. It is moving one decision from layer 3
back into layer 1, where the other half of the hop already lives.

## The vocabulary

- **Payload** — a Job, or a Step within one. The thing that moves.
- **Requirement** — a predicate over actor attributes: role, department,
  clearance, capacity, territory, class membership.
- **Queue** — the extension of a requirement. Not a row; a query. It
  materializes when a payload is addressed by it and evaporates when
  nothing is addressed there.
- **Pool** — the set of actors currently satisfying a requirement. The
  pool is the queue's other face: same predicate, evaluated against
  actors rather than payloads.
- **Protocol** — a Workflow. Defines the legal hops and, under this
  proposal, the address at each hop.
- **Layer** — an additional protocol conjoined onto a payload in flight.
  Narrows the pool and may add obligations.

## Undrained queues are the algedonic signal

The failure mode of on-the-fly queues is the interesting part, not the
risk to be mitigated. A requirement nobody satisfies, or a queue nobody
drains, is precisely a pain signal in Beer's sense: the system reporting
that work has been addressed somewhere it cannot be worked. The brewery
is named for this.

Two distinct conditions, and they want different handling:

- **Empty pool** — the requirement resolves to zero actors. The payload
  was addressed nowhere. This is detectable at address time and should
  fail loudly rather than silently parking.
- **Unattended pool** — the pool is non-empty but nothing is being
  claimed. Detectable only over time, and it is a genuine management
  signal: either the requirement is wrong, the pool is overloaded, or
  the work is not actually wanted.

Both are computable from the traffic capture rather than needing their
own bookkeeping — but see the constraint below.

## Constraints this must respect

**The expression language is smaller than this needs.** `boss-expr`'s
entire value type is `Null | Bool | Int | Float | String`, with two
registered functions (`open_po_exists`, `vendor_for`). It answers "is
this scalar condition true". Requirements-addressing needs "which
candidates satisfy this" — a set-valued result and a function registry
that can reach people, roles, capacity, and clearance. This is the
load-bearing extension, and it is a language decision the system lives
with for years.

**Packets are not correlatable yet.** `jobs.step.created` carries step
identity under `id`; `jobs.step.completed` carries it under `step_id`;
the intersection of the two identifier sets over the whole `audit_log`
is empty. Until that is fixed (`ship-a-change` job `784d26c9`), no
queue-drain metric computed from the log can be trusted — a query
joining the two returns zero rows and reads as "nothing happened"
rather than failing. Measurement of this design is downstream of that
fix.

**Addressing is a policy surface.** A predicate that selects actors is
one edit away from being a predicate that grants access to a payload.
`boss-policy` is already row-level `(action, resource, scope)` and is
the right place for that check to stay. Address resolution must not
become a second, weaker authorization path.

**Determinism.** Per the five-property correctness protocol, resolving
the same payload against the same state must produce the same pool. A
pool computed from live mutable state (who is online, current load) is
not reproducible from the log unless the resolved pool is itself
recorded as an event.

## Open questions

All 6 open questions were resolved 2026-08-14 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q2: Does `boss-expr` become set-valued, or do addresses compile to a query? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Extending the evaluator with a collection type and candidate-query
> functions keeps one language for `ready_when` and addresses. Compiling
> an address predicate down to SQL against the people projection is far
> more powerful and much harder to keep deterministic or safely sandboxed.
>
> Proposed: extend `boss-expr` with a set-valued result and a small,
> closed function registry (`in_department`, `has_role`, `has_clearance`,
> `has_capacity`). One language, auditable surface, no arbitrary query
> execution driven by tenant data.

Agreed


### Q1: Where does the address predicate live? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> On the workflow step alongside `ready_when`, or as its own registry of
> named, reusable queue definitions that steps reference?
>
> Proposed: both, in that order — an inline predicate on the step is the
> primitive, and a named-queue registry is sugar over it once the same
> requirement is written a third time. Starting with the registry invents
> a naming problem before we know which requirements recur.

Agreed


### Q6: Can a layered protocol only narrow the pool, or may it widen? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Conjunction is clean, composes in any order, and is easy to reason
> about. Allowing a layer to widen the pool (an escalation protocol adding
> a fallback group) is more expressive and destroys order-independence.
>
> Proposed: narrowing only in v1. Escalation is modelled as a distinct hop
> with its own address rather than as a widening layer, which keeps
> composition commutative and keeps every widening visible as traffic.

Agreed. I think escalation is a sub-job that can be fired off within a job, and the sub-job can go to a queue with wider access potentially.


### Q5: What is the undrained-queue signal, and who receives it? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> An empty pool and an unattended pool are different conditions. Who is
> told, on what latency, and does the payload keep waiting meanwhile?
>
> Proposed: empty pool fails at address time and routes to the payload's
> owner, because it is a modelling error rather than a workload problem.
> Unattended pool raises after a per-requirement threshold to the owner
> of the requirement, not to an on-call broadcast.

Agreed, and further, I just put some feedback in that I think we need to elevate the Dispatcher to a 'Q' network service that manages our queues and addresses. I think it can maintain the active registry of valid queues and/or allocate the address when a new queue needs to be formed for a new requirement.


### Q4: How does an available payload manifest to the pool? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Push (a message per actor per payload), pull (it appears in My Day for
> everyone in the pool), or both?
>
> Proposed: pull is the default and push is opt-in per requirement. The
> notification flood item is the evidence — push-per-actor across a pool
> multiplies rather than routes, and the measured inbox was 1,016 unread
> of which 619 were machine notifications.

Agreed


### Q3: When is the pool resolved, and is the resolution recorded? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> At hop time only, continuously while the payload waits, or both?
>
> Proposed: resolve at hop time and **emit the resolved pool as an event**,
> so determinism holds and the log can answer "who could have done this"
> after the fact. Re-resolve on a schedule while a payload waits, emitting
> only on change — otherwise a payload addressed to a pool that later
> empties sits in a queue that no longer exists.

Agreed
