# Design: stations — priority queues as the network's nodes

**Status**: in-review
**Origin:** David, 2026-08-13 (verbatim): "Stations are the abstract
priority queues we use to either route or hold job packet traffic
until we have bandwidth or capability to handle the job packet in
question... These are all defined in data, so we can be really
flexible and interesting. Now that we separated the queuing from the
dispatching, I think we should have a clean architecture too."
**Related**: [job-packet-network.md](./job-packet-network.md) ·
[views-as-queue-lenses.md](./views-as-queue-lenses.md) ·
[protocol-cadence.md](./protocol-cadence.md) ·
the retired operating-system view (pre-network framing, dropped
2026-08-13 — its Q1/Q2/Q5 resolutions live in Decision history here)

---

**Lineage:** the fork-era `docs/priority-queues` draft and the
operating-system view both fold in here — **priority queues,
stations, and network nodes are one concept** (David, 2026-08-13).

## The definition

A **station** is an abstract priority queue that **routes or holds
job-packet traffic** until there is bandwidth or capability to handle
the packet. Stations are the nodes of the network; routes between
them are its edges. **Queuing is separated from dispatching**: a
station holds and orders; the dispatcher (the router) moves packets
between stations. Everything about a station is **registry data** —
never a code path.

## The taxonomy (all rows, one registry)

- **Actor stations** — every executor has one: humans *and*
  registered agents. The personal queue (My Day / assignments) is
  this station rendered.
- **Group stations** — served by a set of actors. At minimum **every
  department has a station**; teams can have their own.
- **Constraint stations** — membership defined by capability
  predicates: skills, authority, sign-off rights (Class-registry
  vocabulary), not by an enumerated roster.
- **Batch stations** — the SDLC's bundling points, where packets
  accumulate for **periodic, higher-bandwidth handling**: the loading
  dock, the review queue, board windows. Cadence rules
  (protocol-cadence.md — notably the `queue-depth` basis) are how
  batch stations drain.

Visual rollup — collapsing actor stations into team/department
groupings — is a **view-level aggregation** for clutter control, not
a data-model change; showing every station is acceptable for now.

### Per-actor stations without per-actor rows

"Every executor has an actor station" reads, naively, as one registry
row per person — rows that must be minted at every hire and go stale
at every departure. That is not a registry; that is a generator.

The predicate shape carries a **self placeholder** instead: the
literal `"@me"` in a value position (a `metadata_equals` value, or the
step clause's `assignee_id`), which the evaluator binds to the
**requesting actor** at read time, once, before any packet is
compared. So an actor station is **one row every actor can query**,
and its queue is derived per request like every other station's.

Two rules keep it safe, and both fail closed:

- An unbindable placeholder — a guest, nobody to bind to — yields no
  predicate at all, and the read edge answers with an **empty** queue.
  Failing to bind can never widen one.
- An *unbound* predicate matches nothing, so a placeholder that
  somehow reached the evaluator cannot compare `"@me"` against packet
  data and hand one packet to everybody.

The first row to use it is `my-watchlist` (117-watchlist-station.sql):
the packets a person filed, which is where David's ask landed —
"we should always notify the filer with the terminal state and it
should show in their watchlist" (2026-08-13).

### Departed packets: the terminal window

Stations hold *in-flight* traffic, so a station's universe is packets
that have not reached a terminal status. A watchlist inverts that: it
is read by the person who filed the packet, and the terminal state is
the information they came for — a list whose entries vanish at closure
is empty at exactly the moment it matters.

`terminal_window_days` on the station row keeps departed packets
visible for N days after `closed_on`, then ages them out. It sits on
the row beside `discipline` and `wip_limit`, not inside the predicate,
because it is a **retention** rule rather than a membership one: the
predicate says which packets are this station's, the window says how
long a departed one lingers on the board. Keeping it off the predicate
also keeps predicate evaluation clockless.

## What already conforms

The claim CAS is "an actor takes a packet from a station" made safe.
The yard's dock is a batch station rendered. `parked_ready()` and
every page's hand-rolled fetch+filter are station predicates trapped
in code — the simplification this doc unlocks is moving them into
the registry and rendering all of them through one lens machinery.

## Decision history

**Q1–Q4 all resolved 2026-08-13 (David): "I agree with all 4
question proposals."** Membership is derived (stations are
predicates), motion is evented (router emits arrival/departure
markers); discipline is data on the station row, `priority, then
age` default, visible in the lens header; capability gates at the
claim CAS and `wip_limit` is advisory-first; the registry lives in
`boss-jobs` beside the workflow registry.

### Resolved Q1 — Is station membership derived or assigned?

Derived: a station is a predicate over packet state, membership
recomputed from the log (Hickey-clean, no new mutable field; motion
is inferred from events). Assigned: packets carry an explicit
current-station, and routing is an event that moves it (motion is
first-class, but a second source of truth appears). Proposed:
**derived membership, evented motion** — predicates define the queue;
the router emits arrival/departure marker events so the map and flow
metrics read motion without a mutable location field.

### Resolved Q2 — What orders a station's queue?

Priority disciplines as data on the station row: by packet priority,
by age, by due date, or a declared composite. Proposed: a small
`discipline` vocabulary per station with `priority, then age` as the
default, and the discipline visible in the lens header — an operator
should never wonder why the queue is in this order.

### Resolved Q3 — How are bandwidth and capability declared and enforced?

Capability: the constraint predicate (skills/authority from the
Class registry) gates who may claim from the station — enforced at
the claim CAS. Bandwidth: does a station declare WIP limits or
service rates (making "until we have bandwidth" measurable and
giving the algedonic layer a signal when a queue exceeds it)?
Proposed: optional `wip_limit` per station, advisory first (a lens
warning + telemetry), enforcing later if the data says it matters.

### Resolved Q4 — Where does the station registry live?

Proposed: rows in `boss-jobs` beside the workflow registry (same
append-only versioned posture, migrations in the 11x sequence), not
a new crate — stations are packet-adjacent data, and the evaluation
port ("give me this station's queue, ordered") belongs where jobs
and steps already live.

**Where a platform station is DECLARED moved on 2026-09-18** (backlog
393d3234, consolidation H4). "Migrations in the 11x sequence" was the
registry's only home for its first five weeks — seven migrations
inserted or bumped rows, four more UPDATEd columns in place — and a
migration is the wrong home for a registry row: it runs once, a fresh
instance cannot re-declare it without replaying history, nothing
drift-checks it against the live row. The home is now
`infra/platform/stations/<name>.toml`, one file per station carrying
every column, published insert-if-missing by (name, version) at every
start by `boss-platform-workflow-seed` (the same seed path the Workflow
bundle rides; the directory is found beside `--seed-path`). A bundle
row that differs from the live active row of the same (name, version)
is refused by field — a version bump is the edit path — and
`infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
stations` in any migration newer than its cutover stamp. The historical
inserts stay as history, and the bundle is pinned equal to the rows they
produce (`the_stations_bundle_is_the_migrations_pg.rs`).

## Open questions

_None — Q1–Q4 resolved in Decision history; further questions arise from the build._
