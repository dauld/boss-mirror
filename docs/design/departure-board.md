# Design: the departure board — watching the information trains

**Status**: decided — all 5 questions resolved 2026-08-14 via the in-app tracker; see Decisions.
**Origin:** David, 2026-08-12 (feedback `2e98b211`): "It is exciting
to see job packets moving more dynamically across the network as we
work them. We are still missing good visuals for the IT department
to watch our information trains."
**Related**: [stations.md](./stations.md) — the nodes this board
sits beside; trains are consists of packets ·
[job-packet-network.md](./job-packet-network.md) — cars couple via
`job_edges`; the board is a queue lens ·
[queue-visibility.md](./queue-visibility.md) ·
[protocol-policy-publish.md](./protocol-policy-publish.md) —
`forge.*` events become the board's push feed ·
Design Language v1.0 §04 (chips, signal discipline), §05 (gauge
fragment, sparingly).

## The claim

The pipeline is already four queues wearing protocol names: cars
**parked** at review (the loading dock), trains **boarded** (the
consist assembled, cars coupled through `boarded_jobs` edges), trains
**departed** (merged into forge main), and two **arrivals** (the
playground deploy, the cluster convergence). The departure board is a
lens over those queues in the idiom the metaphor begs for — a
station board: one row per train, its consist expandable to cars,
its signal lights live, its arrivals stamped.

This is the second pages-as-queue-lenses conversion (after My Day,
`3f5f7f63`): **no new projections, no new state**. Every row derives
from ship-a-change and pr-train Jobs the conductor already writes,
and every transition the board animates is a marker event the system
already emits.

## The data, all of it existing

- **Loading dock** — ship-a-change Jobs, `status=open`, review step
  ready/active, no `train` edge: branch, title, parked-since.
- **Consist** — the train Job's `boarded_jobs` edge list joined to
  its cars; skips surface per-car from `skip_reason`.
- **Signal** — the CI verdict step (`result: green/failing`) while
  coarse; the forge's per-check statuses (fast/test) when the
  `forge.check.completed` ingress lands (PPP Q5's first consumer).
  Two lamps, §04 chip discipline: the *live dot* is the one
  signal-green element on the board.
- **Departure** — the `merged` step (`merge_ref` is the stamp).
- **Arrival: playground** — the `deployed` step's summary (already
  carries `main@<sha>; services: prod; web: deployed`).
- **Arrival: cluster** — the one genuinely missing fact (Q2 below).
- **History** — closed pr-train Jobs; per-stage durations
  (board→depart→arrive) computable from step timestamps the same way
  `/api/views/stages` already computes step latency.

## Anatomy (Design Language v1.0)

DM Mono caps throughout; hairline rules; square corners; VOID ground
with INK rows. Top: the **next departure** line (window cadence from
the timer) and the loading dock count. Center: the board — columns
`TRAIN · CONSIST · SIGNAL · STATUS`, status flowing
`BOARDED → DEPARTED → ARRIVED ×2` with the live dot on whichever
train is mid-flight. A row expands to its cars (branch, title, spec
slug, skip reason if left behind). Bottom: recent arrivals with
stage timings. The §05 gauge fragment renders CI progress on the
active train — the board's one decorative texture. Microcopy:
`PARKED → BOARDED → DEPARTED → ARRIVED`; a skipped car reads
`LEFT BEHIND — <reason>`; an empty window reads `NO DEPARTURES —
nothing ready to board`.

## Motion

The board rides the existing push stream: pr-train step transitions
emit `step.done.<kind>` markers on topics the SSE stream already
serves, so rows flip state live without polling; the consist's cars
flip as their `merged` markers land. Per sse-policy, the aggregate
parts (history durations, dock depth) poll at the os-map cadence.
Signal-light granularity upgrades from poll to push when the
`forge.*` ingress lands — the board is deliberately its first
consumer, so the ingress ships with a visible payoff.

## Open questions

All 5 open questions were resolved 2026-08-14 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q3: How deep does the consist view go? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Chips only (branch + title), or per-car diff stats and review
> provenance? Proposed: chips in v1; the car's Job page is one click
> away and already owns the detail. The board is for watching, not
> working — actionable depth belongs to the queue lenses that own the
> steps.

That sounds good


### Q2: How does the cluster arrival enter the record? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> The cluster-deploy-runner converges the cluster on forge main but
> tells BOSS nothing — the board's second ARRIVED stamp has no fact to
> read. Proposed: the runner reports its arrival as an event about an
> infrastructure Subject (the cluster host), which makes this the
> first concrete slice of infrastructure-as-Subjects (`62881872`) —
> `deploy.converged` with the image tag and sha, through the same
> webhook-ingress shape the forge events use. Until then the board
> shows the cluster lamp as `CONVERGING` on a poll of the deployment's
> image tag, honestly marked as observed-not-recorded.

Agree with proposal


### Q4: What history does the board keep on screen? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Proposed: the last five arrivals with three stage timings each
> (board→depart, depart→playground, depart→cluster), computed from
> step timestamps at read time. No new tables; if the timings prove
> load-bearing they graduate into `/api/views/stages` beside the step
> latencies it already serves.

Sounds good


### Q5: Poll-to-push cutover for the signal lights? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Proposed: ship v1 on the existing SSE markers + a 10s poll of the
> forge combined status for the two lamps; swap the lamps to
> `forge.check.completed` events when the ingress lands, and delete
> the poll in the same car — the ratchet discipline applied to a
> polling loop.

Agreed


### Q1: Where does the board live? (resolved)

Resolved 2026-08-14 — override.

**The question was:**

> Answered by David mid-review (2026-08-12): **the board is the IT
> app's landing route, viewable via Guest Access.** The yard is the
> department's front door — the first thing anyone sees of IT is its
> work moving. Two consequences the build honors: every read the board
> makes must be audit-readonly-safe (jobs-API lenses only in v1; the
> CI lamps read the train Job's own ci step, never the forge API,
> which guests cannot reach), and the canvas keeps `/system/flow` as
> the one wall for the *network* picture while the yard owns the
> *pipeline* picture at the IT landing. When the lens registry lands,
> the board becomes a saved lens row and its placement is data.

Done
