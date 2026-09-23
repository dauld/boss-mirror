# Design: crates and layers — what BossMemory is, and what a "reboot" should actually change

**Status**: in-review (agent draft; the re-tiering call is David's)
**Origin:** David, 2026-08-13 (verbatim): "let's go ahead and create the
feedback job to rethink our crates and tiers in this new framing. I
think we have a cleaner isolation now that will justify a bit of a
reboot there. We probably also need BossMemory or something for the
persistence layer, at a minimum the audit_log of the network, but I
think it will be another helpful unit."
**Feedback packet:** `2b1d4929` (kind `user-feedback`, disposition `design`)
**Related**: the three-layers framing doc (in flight on train #21) ·
[stations.md](./stations.md) ·
[correctness-protocol.md](./correctness-protocol.md) ·
[job-packet-network.md](./job-packet-network.md) ·
[transactional-audit-log.md](./transactional-audit-log.md)

---

## The short answer

David's instinct that the new framing gives "a cleaner isolation" is
**correct, and stronger than expected** — the layer order is already
respected by the build graph today, without anyone having enforced it.
But the thing that follows from that is *not* the reboot it sounds
like. Three findings, in descending order of how much they change:

1. **The layering is already true.** Every dependency edge among the
   layer-critical crates points downward. The re-tier is therefore
   mostly *naming and enforcing what already holds*, which costs a
   lint, not a refactor. This is a much cheaper and much safer change
   than "reboot the crate tiers" implies.
2. **The layer axis cannot replace the tier axis** — they are
   orthogonal, and the layer axis is undefined for 20 of 55 crates.
   Proposal: add layer as a *second* axis, keep tiers.
3. **There is exactly one crate worth actually splitting** —
   `boss-jobs`, at 30,541 lines the largest in the repo, which holds
   BossNET and BossProtocols in one package. Plus two small file-level
   evictions from `boss-events`.

BossMemory earns its name, and has a sharper claim available than
"the persistence layer" — see below.

## Finding 1: the layer order already holds

Measured 2026-08-13 from `Cargo.toml` path-deps across the
layer-critical crates. Read the table as "depends on":

| crate | LOC | depends on | layer |
|---|---:|---|---|
| `boss-core` | 5,444 | — | *(foundation)* |
| `boss-ports` | 505 | — | *(foundation)* |
| `boss-expr` | — | — | *(foundation)* |
| `boss-nats` | — | core | transport |
| `boss-events` | 4,991 | core, nats | **BossMemory** |
| `boss-jobs` | 30,541 | core, events, nats, expr, ports | **BossNET + BossProtocols** |
| `boss-dispatcher` | 6,996 | core, events, jobs, expr, ports | **BossActors** |
| `boss-gateway` | 5,868 | core, events, ports | **BossApps** |

Nothing points backwards. Memory does not know about packets; the
substrate does not know about dispatch; the gateway does not link
`boss-jobs` at all (it routes over HTTP, prefix by prefix — which is
also why train #10's station routes 404'd at the door until the
gateway learned the prefix).

This is the load-bearing result. It means the five names are not a
proposal to restructure the code — they are a description of a
structure that already emerged, which has never been written down and
therefore has never been defended. The gap is enforcement, not layout.

This table was first assembled by reading `Cargo.toml` files by hand.
That hand pass was then checked mechanically by
`infra/lint/layer-order-audit.sh`, written for this review, and the
check found two things the reading had missed — see "What writing the
check changed" below. The result above survives; the process that
produced it did not survive contact.

## Finding 2: layers cannot replace tiers

The two axes answer different questions:

- **Tier** answers *how domain-specific is this?* — core (29) /
  modules (18) / orchestrators (6) / tenants (2). Enforced by
  `infra/lint/tier-import-audit.sh`: a core crate must not depend on
  modules or tenants, with orchestrators explicitly exempt because
  fanning out across tiers is their purpose.
- **Layer** answers *which layer of the network is this?* — BossMemory
  / BossNET / BossProtocols / BossActors / BossApps.

The decisive fact: **12 of the 18 module crates depend on
`boss-events`**, and both tenant engines depend on `boss-jobs`. The
arrow points *from* the domain *into* the network. Modules and tenants
are not a layer — they are the payload the network carries. Ask "which
layer is `boss-ledger`?" and the honest answer is "none; it is a
Subject that rides on all of them."

So replacing the tier axis with the layer axis would leave 20 of 55
crates unclassifiable, and would delete a rule that is currently
enforced in CI. That is a strict downgrade. The layer axis is defined
only within tier 1, and that is fine — it is exactly where today's
tiering says nothing at all, because *everything* network-shaped is
lumped into one 29-crate tier.

**Recommendation:** keep `core/modules/orchestrators/tenants` as the
directory layout and the enforced dependency rule. Add `layer` as
declared metadata on tier-1 crates (`[package.metadata.boss] layer =
"memory"`), and enforce the layer order with a second lint. No crate
moves on disk for this part.

## Finding 3: BossMemory, sharply

David proposed BossMemory as "the persistence layer, at a minimum the
audit_log." There is a stronger claim available, and it is worth
taking because it comes with an obligation:

> **BossMemory is the system of record: the append-only log, plus the
> projections that are pure functions of it.** It is the owner of the
> five correctness properties — provenance, conservation, closure,
> idempotence, determinism.

That last clause is the reason to name it. Today those five properties
are stated in [correctness-protocol.md](./correctness-protocol.md) as
the design north star and owned by *nobody*. That ownership vacuum is
not theoretical: it is why provenance turned out to have no real check
at all, and why conservation is swept by a chore
(the `boss-conservation-invariants` CronJob) rather than held by the layer
that would know when it broke. Giving the properties a home crate
makes "which property is unenforced?" a question with an address.

**BossMemory already exists.** It is `boss-events` — `store.rs`,
`audit_pg.rs`, `ledger.rs`, `replay.rs`, `integrity.rs`, `outbox.rs`,
`registry.rs`. It has 12 module consumers. Naming it is a rename plus
a boundary, not new construction.

Three files in it belong to other layers, and the naming is what makes
them visible:

- **`claude_dispatcher.rs` → BossActors.** It implements
  `AgentDispatcher`, spawns `claude --print` as a subprocess, and
  tracks per-agent concurrency and cost. That is the agent-attach
  contract living inside the memory crate. Under the new names this is
  a layer violation you can *say out loud*, which is the whole test of
  whether a naming scheme earns its keep.
- **`dispatcher.rs` → BossActors.** `StubDispatcher`, the deterministic
  fake used for end-to-end loop testing. Same layer, same eviction.
  This one the hand pass missed entirely; the check found it.
- **`tail_http.rs` → BossApps.** It is an HTTP read surface over the
  log and the only reason `boss-events` links `boss-policy-client`
  (for `AccessTier` / `CurrentUser` authz). A door, bundled into the
  thing behind the door.

So it is not one stray file — `boss-events` contains a whole
agent-attach subsystem (two `AgentDispatcher` implementations, real
and stub) plus a door, sitting inside the crate whose job is to
remember. None of it is urgent, none of it is expensive, and all of it
is now held by a ratchet.

**Update 2026-09-23 (backlog 05a003da).** The agent-attach subsystem
was not evicted; it was deleted. Once boss-cybernetics was retired
(train #582), `claude_dispatcher.rs`, `dispatcher.rs`, `ledger.rs`,
`queue.rs` and `registry.rs` had no consumer, and they went with the
`AgentDispatcher` port they implemented. Their two allow-list lines
went too; `tail_http.rs` is the one exception left.

## What writing the check changed

The check (`infra/lint/layer-order-audit.sh`, with `--self-test`, 4/4
fixtures) was written to confirm Finding 1. It disagreed with its
author twice, which is the argument for writing it at all:

1. **`boss-nats` was misclassified as BossNET.** The first draft of
   the layer map ranked the NATS broker driver as `net`, and the check
   immediately flagged `boss-events → boss-nats` as a backward edge.
   The map was wrong, not the code: **BossNET is the packet substrate
   — stations, routes, admission — not networking-the-wire.** The
   broker sits *underneath* everything, including the log. This is the
   one place the throwback name costs something, and the mistake was
   made by the person writing the definition down, roughly an hour
   after writing it. Expect it to recur; it is now a comment in the
   map. (Incidentally, `boss-events` only touches `boss_nats` from
   `src/bin/boss_event_relay.rs` — the relay binary that pumps the log
   onto the bus — so even the edge was thinner than the package
   manifest implied.)
2. **A third misplaced file.** The hand pass found
   `claude_dispatcher.rs` and `tail_http.rs`. The check also found
   `dispatcher.rs`. Two out of three is a decent hit rate for reading,
   and a bad one for a boundary you intend to defend.

The general lesson is the one the repo already believes and keeps
having to relearn: a fact that lives twice gets an equality test. The
layer map lived in prose, and prose does not disagree with you.

## Finding 4: `boss-jobs` is the one real split

`boss-jobs` is 30,541 lines — six times `boss-events`, the largest
crate in the repo — and its module list splits cleanly along the
BossNET/BossProtocols seam:

| BossNET (substrate) | BossProtocols (operating model) | BossActors | glue |
|---|---|---|---|
| `station_queue.rs` | `registry.rs` | `escalation.rs` | `policy_glue.rs` |
| `stations.rs` | `workflow_lint.rs` | `owner_resolution.rs` | `calendar_hook.rs` |
| `job_edges.rs` | `workflow_quarantine.rs` | | `subject_existence.rs` |
| `port.rs` | `step_registry.rs` | | |
| `postgres.rs` / `in_memory.rs` | `step_plugins.rs`, `seed_loader.rs` | | |

This is the crate the framing doc was describing when it said a
protocol that cannot be replaced without a deploy has leaked into the
substrate — the leak has a package name, and this is it. Splitting it
would make the central prohibition mechanically checkable: BossNET
must not import BossProtocols, so the substrate *cannot* learn what a
workflow means.

It is also, by a wide margin, the most expensive item here (30.5k
lines, and every tenant engine depends on the crate). It should be its
own job, sequenced after the cheap parts, and it is the one piece of
this that genuinely deserves the word "reboot."

## Proposed sequence

Cheapest and safest first; each step is independently valuable and
independently abandonable.

1. **~~Declare layers as metadata~~ — done differently.** The layer map
   lives in the check itself (`layer_of`), following
   `tier-import-audit.sh`, rather than in 29 `Cargo.toml` files. One
   place to read, one place to argue with. Revisit only if something
   other than the lint needs to consume the map.
2. **~~Add `infra/lint/layer-order-audit.sh`~~ — done.** Order check
   plus two ratcheted shape rules, `--self-test` green, clean against
   the tree with three allow-listed exceptions. Still to do: wire into
   `infra/gate.sh` so it runs in forge CI.
3. **Register the layer prohibitions** in `docs/invariants/` as
   `enforced` once (2) is wired into the gate. Each name carries one:
   BossNET must not know what work means · BossProtocols must not
   require a deploy to change · BossActors is the attach contract, not
   where logic lives · BossApps must stay thin · BossMemory must not
   depend on anything above it.
4. **Evict the three files** from `boss-events` (Finding 3), deleting
   their allow-list lines as each lands.
5. **Rename `boss-events` → BossMemory's home** — *if* renaming is
   worth 12 consumers' churn. Arguably the metadata declaration in (1)
   already buys the clarity and the rename buys only tidiness.
6. **Split `boss-jobs`** along the seam in Finding 4. Separate job.

## What this does not resolve — for David

1. **Is (5) worth it?** A rename touches 12+ crates for naming
   clarity alone, and `boss-events` is not a *wrong* name for a log.
   The alternative is that BossMemory stays a declared layer that
   `boss-events` implements, and the five names live in docs and lints
   rather than in package names. My read: skip the rename, keep the
   names conceptual. But "the names should be real packages" is a
   defensible opposite call and it is yours to make.
2. **Does BossMemory own the five correctness properties?** Adopting
   the sharp definition means the enforcement gaps become that layer's
   debt with a named owner. That is the point, but it converts a
   north-star doc into a scoreboard — worth confirming you want that.
3. **Does the split in (6) get scheduled now or noted for later?**
   It is the only genuinely large piece. Everything above it is small
   enough to land inside a normal train.
4. **`boss-brewery-engine` depends on `boss-sim`**, a tenant crate
   reaching into an orchestrator. It does not violate the stated tier
   rule (which constrains core only), but it is the sort of edge the
   rule would forbid if it were stated symmetrically. Tighten the tier
   rule, or accept it as intended?

## Decision history

- **2026-08-13 — the layer axis supplements, it does not replace.**
  Evidence: 12 of 18 module crates depend on `boss-events`; both
  tenant engines depend on `boss-jobs`. Domain crates are payload, not
  layers, so a layer-only taxonomy leaves 20 of 55 crates
  unclassifiable and discards an enforced CI rule. Recorded here
  rather than left implicit because "reboot the tiers" was the natural
  reading of the request, and the evidence argues against it.
- **2026-08-13 — the re-tier is a lint, not a refactor.** The layer
  order already holds across all eight layer-critical crates with no
  backward edges. This was checked before proposing the work, not
  assumed from the framing.
- **2026-08-13 — BossNET is the packet substrate, not the wire.**
  Ratified by getting it wrong: the layer map's first draft ranked the
  NATS broker driver as `net`, and the check caught the resulting
  backward edge from the log. The broker is foundation; BossNET is
  stations, routes, and admission. Recorded because the name invites
  this reading and the author of the definition fell for it first.
