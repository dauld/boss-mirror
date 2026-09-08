# Design: shadow and experimentation — the network runs its own A/B

**Status**: approved — all five questions resolved in review (packet `574c2adf`); carried to a file 2026-08-22.

**Origin**: David, 2026-08-22: *"we really haven't built out the shadow
and experimentation capabilities of the network yet."*

## The worked example that motivates this

Every experiment run on the brewery this week was run BY HAND: publish
`tasting-panel` v1, watch the first packets, correct the spawn-rule
payload refs, publish v2 to delete a duplicated fact, v4 to fix field
types — measuring each step with ad-hoc SQL against terminals. It
worked, and it proved the doctrine (protocols are cheap; terminals are
the measurement). But nothing about it was repeatable by anyone else,
nothing recorded which packets were "the experiment," and there was no
control group: v1 packets and v4 packets differ by version *and* by
everything else that changed that day.

## What already exists (the bones)

- **Version pinning.** In-flight packets stay pinned to the workflow
  version they were admitted under. This is the hard half of A/B: the
  network already remembers, per packet, which protocol variant ran it.
- **The partition boundary.** `simulated` is a fail-closed lane the
  workforce, epoch trim, and census all respect. A second partition of
  the same shape is a known, proven pattern.
- **Terminals as measurement.** `jobs.job.closed` carries kind, outcome,
  and version-derivable identity; outcome distributions and cycle times
  per version are one query away — they just have no home.
- **Registry authoring end-to-end.** Draft → publish → hot-load works
  for workflows and dispatcher rules alike.

## The gaps

1. **Split admission.** Admission always uses the active version. There
   is no way to say "admit 20% of new `wholesale-keg-order` packets
   under v3-candidate, the rest under v2" — so a candidate can only be
   tested by full cutover, which is rollout, not experiment.
2. **No shadow lane.** There is no way to run a protocol against real
   traffic *without* effects: a shadow packet should flow steps and
   reach a terminal while side-effect handlers (ledger postings,
   inventory consumes, shipping) refuse it the way the workforce
   refuses real packets today.
3. **No experiment entity.** Which packets belong to an experiment,
   what its arms are, when it started, what would conclude it — none of
   that is data anywhere. Conclusions live in chat transcripts.
4. **No paired demand.** The sim can't drive two arms with the same
   demand seed, so arm differences are confounded with traffic noise.

## Proposed shape (smallest first)

**Tier 1 — measure what pinning already records.** A terminal report
per (kind, version): outcome distribution, count, median cycle time.
Pure read surface; no admission changes. This alone would have replaced
every ad-hoc SQL query of the past two days.

**Tier 2 — split admission.** An `experiment` registry row: kind,
control version, candidate version, split (hash of packet id → arm, so
replay-deterministic), window. Admission consults it; packets get an
`experiment_arm` stamp. The Tier-1 report grows an arm dimension.
Promote = publish candidate as active + close the experiment row;
retire = close the row, candidate stays draft history.

**Tier 3 — the shadow lane.** A third partition value (`shadow`)
sharing the simulated lane's fail-closed machinery, plus a hard rule:
side-effect handlers skip shadow packets (the dispatcher already knows
how to skip by partition — the sim-origin header path generalizes).
Shadow admission mirrors a real packet's trigger into the candidate
protocol; its terminal records what WOULD have happened.

## Decision history

All five questions were resolved by David in the design review, 2026-08-22
(packet `574c2adf`).

- **Q1: Is the workflow version the experiment unit?** — "Let's start with version vs version."

- **Q2: Shadow as a third partition value, or a nested flag?** — Third partition value (`real | simulated | shadow`), keeping fail-closed provable at every boundary consumer.

- **Q3: Where does an experiment conclude?** — "Yes, let's make experiments packets with promoted / retired terminal states." Experiments are jobs of kind `protocol-experiment`; the network's own machinery carries them.

- **Q4: Who may experiment on what?** — IT department + platform-admins only for now; experimental protocols supporting other actors can come later.

- **Q5: Does the sim learn paired demand?** — No. "The sim should be entirely independent from any experimentation capabilities in the network." Unpaired splits; the sim is load, never a participant.

## Tier 2 as landed (packet 6ea5a12a)

Per Q3 there is no experiment table: the record is an open packet of
kind `protocol-experiment` (shipped as bundle data in
`infra/platform/workflows.toml`), and the split declaration is that
packet's JOB metadata — `kind_under_test`, `control_version`,
`candidate_version`, `split` (candidate share percent, default 50).
The window is the packet's open interval; both edges are already in
the log. Admission (`boss-jobs/src/experiments.rs` +
`http/jobs.rs::create_job`) hash-splits each new packet of the kind
under test by its own job id (fixed FNV-1a — replay-deterministic),
pins it to the arm's version (the candidate is ordinarily a draft:
this is the sanctioned way a draft meets traffic), and stamps
`experiment_arm` / `experiment_id` into job metadata before
`JOB_CREATED` is built, so rebuilders replay the recorded choice. The
Tier-1 terminal report groups by (version, arm); unstamped bystanders
ride a null arm. Fail-safe throughout: a malformed declaration or a
missing arm version admits under the active version, unstamped — an
experiment must never break the kind it measures. Promote/retire
remain registry verbs an operator runs at the packet's terminals;
nothing publishes automatically.
