# Design: a node for the work — build capacity in BossInfra

**Status**: in-review — one question, and it is David's to answer.

**Origin**: David, 2026-08-23, after a day of gates dying under
their own weight: *"But could we add a small new server to the
cluster to help?"*

---

## The measurement that answers "why does this keep happening"

| node | role | cores | RAM | ephemeral |
|---|---|---|---|---|
| cp-1 | control plane | 8 | 15 Gi | 236 Gi |
| cp-2 | control plane | 12 | 31 Gi | 236 Gi |
| cp-3 | control plane | 8 | 31 Gi | 463 Gi |

**There are no worker nodes.** Every workload in the cluster —
the system of record, its Longhorn replicas, etcd, the dev pod, and
every gate — runs on the three machines that also hold the control
plane. That is the whole story behind 2026-08-22's incidents: a
six-gate cargo chain starved etcd into failing readiness at 18:10Z
and bounced cp-2's kubelet at 19:17:49Z. Nothing was misconfigured.
The cluster simply has no place to put heavy, bursty, unimportant
work, so heavy bursty work lands next to the most important process
running.

Gate-runs are cheap to lose and expensive to run: 8 of 9 tracked
runs finished green, but the ones that died took a control plane
with them. That asymmetry — low value, high blast radius — is
exactly what a dedicated node is for.

## What the work actually needs

Measured from real gate runs, not estimated:

- **~74 GB** of disk for one cold `cargo test --all-features`
  target tree. Two branches' targets do not fit in 100 Gi; that
  produced two ENOSPC failures that read as code failures.
- **6–8 cores** usefully (`CARGO_BUILD_JOBS=4` plus test threads);
  more cores shorten a 30-minute gate, they do not change whether
  it passes.
- **12–16 GB RAM** under the current limits, with the peak in
  linking, not testing.
- **A Postgres sidecar** with ≥1 GB of shared memory.
- Nothing persistent. A gate node can be reimaged at any moment
  and lose nothing that matters.

## Three ways to get it

**(a) Enroll the existing forge host as a Talos worker.** It is
already here: 16 cores, 30 GB RAM, on the LAN at 10.20.0.15. Cost:
zero money. Cost in risk: it currently runs Forgejo, the CI runner,
the OCI registry, and the container registry that every deployment
pulls from — and it sits at **87% disk** (29 GB free) with a load
average near 6 from CI alone. Putting gates there moves the blast
radius from "the control plane" to "the forge everything depends
on", which is not obviously an improvement, and the disk cannot
host a 74 GB target tree today.

**(b) One small dedicated worker node.** A mini PC in the class the
forge host already is — 8+ cores, 32 GB RAM, 1 TB NVMe — joins as a
Talos worker with no control-plane role. Gates, the dev pod, and
any future build work get a `nodeSelector` and physically cannot
touch etcd or the SoR replicas again. This is the smallest change
that removes the incident class by construction rather than by
scheduling discipline.

**(c) Rent it.** A cloud VM as a Talos worker over WireGuard.
Elastic, no hardware, but it puts the cargo cache and clone on the
far side of a home uplink; a cold 74 GB build pulling crates over
that link is a different kind of slow, and it adds a recurring bill
to a lab that currently has none.

## What we have already done without new hardware

Worth stating so the question is honestly framed as *additive*, not
*remedial*: the dev container is bounded (8 CPU / 16 Gi), gates run
as Jobs with their own disk pinned to cp-3 rather than cp-2, and the
gate-runner car takes its own clone so it needs no workspace volume.
Those changes stop gates from *starving* the control plane. They do
not change the fact that the only disk and RAM available belong to
machines running etcd.

## Open questions

### Q1: Which of (a), (b), or (c) — and if (b), does it also take the dev pod?

The recommendation is **(b), one small worker**, on the grounds that
it is the only option that removes the failure class by construction
and the only one whose cost is bounded and one-time. (a) trades one
critical host for another and is disk-blocked today; (c) trades
money and network latency for elasticity this workload never needs.

The follow-on: a worker node makes the dev pod portable too. Moving
both gates and the dev workspace off the control plane would leave
cp-1/2/3 running only the system of record and its replicas — which
is what those machines are for. Worth deciding together, since it
changes the sizing (a shared dev + gate node wants the 1 TB disk and
32 GB, not 16).
