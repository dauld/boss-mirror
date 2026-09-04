# The cluster is the system: three planes, one operator

**Status**: in-review — the target architecture for internalizing BOSS's
own operations, decided by David 2026-09-03 ("I want the conductor
moved... everything that can be part of the cluster should be part of
the cluster. boss-gcp is for bastion and public website access. The
build systems should not be kubernetes-managed, but everything should
be internalized and controllable by BOSS itself so we dogfood by
running our own system"). Prompted by a four-lens independent review
of yard operations whose findings this doc carries.

## Why this exists

A four-lens independent review (the car's journey, the human-touch
audit, the incoherence hunt, the adversarial topology review) reached
one verdict by four roads: **the operating model became registry data,
as §9 intends, but the surfaces to read and act on that data were never
built — so the truth is precise and unreadable without an SSH and a
source-dive.** The proof was the review itself: the four reviewers
disagreed about where the conductor reads its data, because the answer
lives in a systemd drop-in on a host none of them could read. Four
expert audits of a fully-documented system produced contradictory
models of its most important data flow.

The cause is structural, not clerical: the pipeline spans four runtimes
(cluster, the boss-gcp conductor VM, the build host, CI containers),
each with its own units, credentials, checkouts, and update cadence,
and the conductor-on-a-VM is the most inflamed joint — it cannot read
its own configuration from the system it supervises, so that config
lives in hand-edited drop-ins, and it deploys from a mutable tree that
goes dirty and blocks trains silently (it did so for four hours the day
this was written, on a hop this design deletes).

## The target: three planes, each with one job

### Plane 1 — The cluster is the system

Everything that *can* be a Kubernetes workload is one. The SoR stack
and gate-runner already are; **the conductor moves here** (train verbs
+ cadence loop as a Deployment, forge token as a Secret the broker
already rotates, the train-assembly clone on a PVC or emptyDir). Config
converges from the tree; there are no per-host drop-ins because there
is one host abstraction. An operational parameter is a registry row
with an HTTP surface, not a constant in a drop-in on a box.

### Plane 2 — The build plane: BOSS-managed, not Kubernetes-managed

Forge, the OCI registry, the CI runner, and the cluster-deploy-runner
stay on the build host. This is deliberate and correct: they need a
docker socket Talos does not provide, and a single-tenant single-repo
runner is right-sized where it is. The forge→cluster flow is one-way
and fortress-like (the runner reaches into the cluster; nothing in the
cluster reaches back), which is a property worth keeping.

What changes is that the build plane stops being *un-modeled*. It
becomes a first-class part of the system BOSS operates on itself:

- its services are **estate subjects** (Forgejo, registry, runner,
  deploy-runner — each a row, with its unit name, restart verb,
  and health probe, so nobody guesses `act_runner` vs `forgejo-runner`
  again);
- its operations are **ops-request verbs** (restart, status, journal,
  reclaim-disk — the read-only set ships already; the mutating set
  arrives behind per-verb policy);
- its config **converges from the tree** where it can (the runner
  already self-updates via exec-from-checkout);
- its health is **observed** (the unit + disk observers landed;
  the alarm that hears them landed) and raises when it dies.

This is the dogfood: BOSS runs its own build infrastructure through
the same Subjects, Jobs, protocols, and audit log it offers a tenant.

### Plane 3 — boss-gcp: bastion and public edge only

WireGuard/SSH ingress and the public website front door (Caddy /
Cloudflare tunnel termination). No BOSS services. The playground
already routes to the cluster gateway, so the local `/opt/boss` stack
this plane retires is the vestige behind the day's silent train stall.

## What moves, stays, retires

| thing | today | target |
|---|---|---|
| SoR stack, gate-runner | cluster | cluster (unchanged) |
| conductor (train + cadence) | boss-gcp systemd | **cluster workload** |
| playground deploy hop | boss-gcp `/opt/boss` | **retired** (cluster converge is the deploy) |
| boss-gcp local BOSS stack | running | **retired** |
| Forgejo, registry, CI runner, deploy-runner | build host, un-modeled | build host, **estate + ops-managed** |
| per-host systemd drop-ins (conductor config) | hand-edited | **converged / registry rows** |
| operational parameters (thresholds, intervals) | schema comments + constants + drop-ins | **registry rows with read surfaces** |

## The conductor move, specifically

It is the defining migration and the one to do first, because it is the
worst joint and its removal deletes a failure class. The credential
work already done is the enabler: the forge token is a broker-managed
Secret, so a pod reads what the broker rotates — no drop-in, no
hand-placement.

Sequence, safest-first, each phase reversible:

- **Phase 0 — see before you move.** Build the read surfaces (yard
  status including the deploy-block reason; `/api/cadence/rules`
  rendered; the service/timer/topology inventory as estate data). These
  are pure additions, and they are what lets a migration run in shadow
  and be *watched*. Verify the load-bearing unknowns below.
- **Phase 1 — conductor in the cluster, shadow.** A cluster workload
  runs reconcile in dry-run alongside the boss-gcp conductor; compare
  outcomes over a week — same rules fire at the same times, same verbs,
  same audit records.
- **Phase 2 — cutover.** The cluster conductor becomes primary; the
  boss-gcp systemd unit is disabled; the playground-deploy hop is
  retired (the build-host runner's cluster converge is the only deploy).
- **Phase 3 — shrink boss-gcp.** Retire the local stack and `/opt/boss`;
  boss-gcp is bastion + edge.

## Open questions

### Q1: Is the boss-gcp playground stack load-bearing or vestigial?
Phase 0 must answer by measurement, not assumption: does anything read
the boss-gcp local stack (`127.0.0.1:7900`), or does every consumer of
"the playground" already hit the cluster gateway? If vestigial, the
deploy hop and the stack retire together and the migration is smaller
than it looks. If something depends on it, name the dependency before
cutover.

### Q2: Where does the conductor read cadence/train data today — cluster SoR or a boss-gcp-local Postgres?
The review's reviewers disagreed. The systemd unit documents
`BOSS_JOBS_URL` as required and pointed at the cluster, and preflight
refuses a loopback; the cadence migration 131 comment implies a
read-from-API cutover was intended. Resolve definitively (read the live
drop-in) before Phase 1 — a shadow conductor writing to a different
database than the primary is the split-brain, not a test of it.

### Q3: Cadence as a cluster workload — Deployment loop or CronJob-driven?
The loop today ticks every 60s under systemd `Restart=always`. In the
cluster: a long-running Deployment (same shape, kubelet supervises) or
a CronJob per tick (more k8s-idiomatic, but the exactly-once firing
claim already handles concurrency and restarts). Decide against the
existing firing-dedup semantics.

### Q4: What is the minimum estate model for the build plane?
Plane 2 says the build host's services become estate subjects. What is
the smallest schema that answers "which service, which unit, which
restart verb, which health probe, from which tree" without duplicating
what the unit files already state — collapse where possible (§9a), pin
where not.

### Q5: How much of the visibility surface (Phase 0) is worth building before committing to the move versus alongside it?
The read surfaces have standalone value (they kill "I SSH to know")
independent of the conductor migration. Sequence them as a prerequisite
(safest — you migrate what you can watch) or in parallel (faster)?
