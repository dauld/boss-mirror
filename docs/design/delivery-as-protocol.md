# Design: the delivery pipeline obeys the doctrine it ships

**Status**: in-review — the diagnosis is measured; the questions are about scope and sequencing.

**Origin**: David, 2026-08-24: *"I am afraid we are just putting in
place patch fix after patch fix instead of creating efficient,
reliable protocols and improving our systems to run them."*

**Related**: [the-three-layers](the-three-layers.md) ·
[protocols-as-data](protocols-as-data.md) ·
[dispatcher-station-boundary](dispatcher-station-boundary.md)

---

## The measurement

`crates/orchestrators/boss-cli/src/train.rs` is **6,852 lines**. It
holds, as Rust:

- the hold policy (how many strikes before a car is held, and what a
  strike is),
- the CI verdict vocabulary (which run states count as failing),
- the stall threshold and its auto-cancel,
- the consist check's lint roster and its four exclusions, its time
  budget, its output budget, its filename budget,
- the skip-reason budget, the blip-cause budget.

Line 120 carries a comment we wrote ourselves: *"this threshold
belongs in the `cadence_rules` registry."* The code already knows.

## The contradiction

CLAUDE.md §9: *"New work types, new step UX, new posting rules — they
land as **data in append-only versioned registries**, not as new
branches in core code… If you find yourself adding a `match kind` in
core code, there's a registry you should be using instead."*

We honour that for the tenant. A brewery protocol changes in seconds:
edit the row, publish, the running dispatcher hot-loads it. Nothing
is built, nothing is deployed, in-flight packets stay pinned to the
version they were admitted under.

We do not honour it for ourselves. Changing *our* delivery policy
means editing a monolith, gating it, boarding it on the very train
system being changed, waiting ~90 minutes for CI, merging, deploying,
and converging. On 2026-08-24 that loop ran five times.

## Why this produced "patch after patch"

Every question that arose this week was a **policy** question:

| the question | what it cost | what it should cost |
|---|---|---|
| does a cancelled run strike its cars? | a code car, a train, a deploy | a registry edit |
| how many strikes before a hold? | still hardcoded | a registry edit |
| which lints run on an assembled tree? | a code car (+672 lines) | a registry edit |
| how long before a stalled train auto-cancels? | still hardcoded | a registry edit |

Each answer was correct. Each arrived as a patch **because the only
available shape was a patch.** A pipeline whose policy is compiled
can only be improved by patching, and every patch rides the pipeline
it is trying to fix — which is how a conductor bug takes six trains
to remove.

## The shape

Not a rewrite. The executor stays; the *decisions* move out.

1. **A `delivery_policy` registry**, versioned and append-only like
   every other: hold thresholds, verdict vocabulary, stall windows,
   the consist-check roster and exclusions, the budgets. Read at
   boarding time, hot, the way the dispatcher reads its rules.
2. **`train.rs` becomes an executor of that policy** — it merges,
   pushes, opens, watches, merges, reports. It stops *deciding*.
3. **In-flight trains pin their policy version**, exactly as packets
   pin their workflow version, so a policy edit mid-flight cannot
   rewrite the rules a train departed under.
4. **The gate-run protocol already proves the shape works** — gates
   became packets in an afternoon and have been reporting verdicts
   ever since, with no code change to anything that runs them.

The prize is not elegance. It is that the next policy question —
and there will be one this week — gets answered in a registry write
that takes effect on the next boarding, instead of a car that must
survive the pipeline it is repairing.

## What this is not

It is not a case for deleting the checks built this week. The consist
check, the advisory lock, and the two designed-out classes are all
correct and stay. The claim is narrower: **their policy content
belongs in rows, and their mechanism belongs in code**, and we
currently have both in code.

## Open questions

### Q1: How much policy moves in the first version?

The cheapest honest slice is the numbers already crying out for it
(hold count, stall hours, consist roster + exclusions, the four
budgets) — no new concepts, just relocation, and line 120's comment
becomes true. The larger slice adds the verdict vocabulary (which run
states mean failing/aborted/pending), which is where this week's
worst bug lived. Start narrow and prove the loop, or take the
vocabulary too because that is where the value is?

### Q2: Does the conductor read the registry, or is it handed policy?

Reading it directly matches the dispatcher (hot, no deploy). Being
handed a resolved policy object at invocation keeps `boss train` a
pure function of its inputs, which is easier to test and to reason
about when a run is replayed. The dispatcher precedent argues for
reading; the conductor's replay story argues for being handed.

### Q3: Is there a `delivery` tenant, or is this platform?

Trains, gates, and cars are how *this* organization ships software —
which is either a platform capability every BOSS deployment gets, or
the IT department's own tenant protocol set that happens to live in
the same repo. The answer decides where the registry rows live and
whether another deployment inherits our hold policy or writes its own.
