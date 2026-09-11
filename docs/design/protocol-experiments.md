# Design: protocol experiments — variants as data, verdicts from the log

**Status**: decided — all 4 questions resolved 2026-08-14 via the in-app tracker; see Decisions.
**Origin:** David, 2026-08-12 (verbatim): "We can definitely take
empirical data from our train deployment protocol and use that to
propose changes. We haven't really exercised our capabilities for
supporting experimentation with new protocols, but it will be
important."
**Related**: [protocol-cadence.md](./protocol-cadence.md) ·
[job-packet-network.md](./job-packet-network.md) — fixed protocol
set at creation is what makes cohorts clean ·
[protocol-policy-publish.md](./protocol-policy-publish.md) ·
[queue-visibility.md](./queue-visibility.md)

## The experiment that already ran

The forge cutover day doubles as the first measured protocol
comparison, and its numbers motivate this design:

- **Old protocol** (GitHub-gated): train #228 sat **~10 hours
  all-green** on a merge permission — the gate cost was the whole
  cycle time.
- **New protocol** (forge-native): train 1801's clean run went
  **CI-green → merged in 1 second → playground deployed in 8
  minutes → cluster converged within the runner's next tick**. The
  landing leg is now measured in minutes.
- **The repair tax moved**: five red rounds on train 1650, and
  **every one was environment, not car code** — missing
  interpreter, runner-cached image, container-root semantics, and a
  timing flake twice. Each attribution cost a manual log
  excavation (docker-cp + zstd + grep) of a 15–25-minute test run.

Three protocol changes fall straight out of that data:

1. **A locomotive check before boarding** — a seconds-long canary
   validating the environment against the suite's declared needs
   (toolchain present, runner image digest == registry digest, uid
   expectations), so environment drift fails before a 25-minute
   test run, not after. Every red tonight would have been caught by
   it.
2. **Attribution lands on the Job** — the `forge.*` ingress
   (already resolved) writes the failing check's log excerpt into
   the train Job's ci step metadata; blame becomes a step read, not
   an excavation.
3. **Re-signal becomes a verb and a metric** — `boss train
   resignal` stamps a counter on the train Job; retry cost turns
   into protocol telemetry instead of anecdote. (Likewise the
   flush-window practice: boarding when the dock is deep is a
   cadence trigger — `basis: queue-depth` joins wall|clock in
   protocol-cadence.md's row.)

## The second experiment: consist size, 4 → 8

Ran before this doc could describe it, which is itself a finding.

**The arm.** `cadence_rules` row `train-board-on-dock-depth` v1
(`min_dock_depth: 4`) retired, v2 (`min_dock_depth: 8`) published
active — a registry edit, no deploy, which is the fat-protocol claim
working exactly as advertised. Motivation was David's: CI is the
bottleneck, so a deeper dock amortises a 20–40 minute `test` job over
more cars.

**The verdict.** David, 2026-08-13: *"Let's keep at 8 cars for now,
until we get evidence that our repair rate is rising."* So 8 stands,
with a named stopping condition rather than a fixed review date — the
right shape for a protocol arm.

**The problem with that stopping condition.** Repair rate is not
measurable today. Cars get `ship-a-change` packets; the commits that
repair a red train get none, and the `pr-train` Workflow has no field
that can represent a repair round — train #21's `ci` step read
`completed` through two failed CI runs. The two attribution timings
this experiment has produced (~2 min for a semantic conflict on round
1, ~2 min for a test failure on round 2) were written onto the train
Job by hand. So the arm has a verdict and no instrument for its own
exit condition. Filed as `bb86d687`; until it lands, "repair rate is
rising" is a judgement someone makes from memory, which is the state
this whole doc exists to end.

**What the two rounds suggest so far** — anecdote, explicitly not
data. The 8-vs-4 worry was that a bigger consist makes attribution
hard. Both failures so far attributed in about two minutes, including
the test failure where the compiler cannot point, because the
assertion printed both strings and `git log -S` named the responsible
car in one query. If that holds, the thing that governs attribution
cost is how well a check reports itself, not how many cars are in the
consist — which would make assertion quality, not consist size, the
variable worth tuning. Two rounds cannot support that claim; they can
only motivate measuring it.

## The capability

Everything an experiment needs already exists as data; what is
missing is the harness that ties it together:

- **A variant is a workflow version.** The registry is append-only
  and versioned; registry writes are now log events. An experiment
  arm is a draft published alongside the incumbent, not a fork of
  anything.
- **Assignment is at admission.** The packet model fixes the
  protocol set at creation — so cohort membership is decided once,
  recorded on the envelope, and never ambiguous mid-flight.
  Arm selection per packet (hash-spread like the dispatcher's
  assignment) or per window (alternating cadence firings) are both
  deterministic and replayable.
- **Measurement is the log.** Marker events are correlatable
  (`27341d5d`); per-version traffic has an index waiting
  (`jobs_kind_version`); the flow-strip machinery computes
  depth/latency per queue. An experiment's verdict is a query
  filtered by workflow_version — no bespoke instrumentation.
- **Conclusion is publish or retire.** Adopting the winner is the
  registry operation that already exists, and the log shows exactly
  which packets ran under which arm forever.

## Open questions

All 4 open questions were resolved 2026-08-14 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q2: What guards a bad arm? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Proposed: arms are subject to the same lint/viability proofs at
> publish; a kill = retiring the arm version (packets in flight finish
> under their pinned version per the packet model); and the algedonic
> default — an arm whose queue depth or failure rate exceeds the
> incumbent's by a declared margin raises to the experiment's owner.

Agreed.  As long as the protocol is valid. But we should also use this opportunity to think about how we do traffic duping to support running an experiment on shadow traffic before attempting to run on real traffic.


### Q1: What declares an experiment? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Proposed: an `experiments` registry row — name, the kind, the arm
> versions, the assignment rule (`per-packet-hash | per-window`), the
> split, the metrics (named queries over the log), and a review Job
> that owns the verdict. Registry data like everything else; the
> admission edge reads it when fixing a packet's protocol set.

Sounds good


### Q3: Where do verdicts render? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Proposed: the experiment is a lens (views-as-queue-lenses): its two
> arms are two queue predicates over the same stations, its flow
> strips are the comparison, and the verdict review lands in the same
> Design Review queue as everything else. The yard shows a train's
> arm the way it shows its consist — cohorts visible, never hidden.

I like it . Let's give it a try.


### Q4: First experiment? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Proposed: the locomotive-check change itself — run windows with and
> without the canary for a week and measure red-round rate and
> time-to-green. The protocol that measures protocols should be the
> first thing it measures.

superseded
