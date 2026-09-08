# Design: deployment as a network — generations, confirms, waves

**Status**: decided — all 5 questions resolved 2026-08-12 via the in-app tracker; see Decisions.
**Origin:** David, 2026-08-10 (`8b508f95`): "I think we should model
our deployment around networking principles" — following the
discussion of how networks patch at scale.
**Related**: [schema-migrations.md](./schema-migrations.md) (the
N-1 compatibility that makes this safe) ·
[internal-forge.md](./internal-forge.md) (the pipeline this deploys)
· [dev-cluster.md](./dev-cluster.md)

## The three layers, applied to deploys

Networks patch forward because they distinguish what KIND of thing
changes — and deployment maps onto the same split BOSS already runs:

- **Traffic** — requests in flight, Jobs mid-step, the audit log.
  Never rolled back; a delivered response is history.
- **Derived state** — the running binaries, the projections, the
  served SPA. RECONVERGED, not restored: rebuilt from intent, freely
  replaceable, no snapshot nostalgia.
- **Intent** — the repo at a commit + the registries + config. The
  versioned layer. "Rollback" exists here only as rolling FORWARD to
  a prior intent: a new deploy action that reproduces yesterday's
  shape, then convergence.

Two prerequisites are ALREADY policy, which is why this design is
mostly plumbing: **expand/contract migrations are exactly the N-1
compatibility** that lets a reverted binary run on today's schema
(networks call it graceful restart; we legislated it 2026-08-08),
and the SPA's content-hashed dist is make-before-break natively.

## The mechanisms

- **Generations (make-before-break).** Installs land in versioned
  directories (`/usr/local/boss/releases/<sha>/`) with a `current`
  symlink; the previous generation stays on disk. Deploy = install
  beside, flip, restart. Revert = re-point, restart — seconds, not a
  rebuild. The old path exists until the new one carries traffic.
- **Commit-confirmed (the dead-man switch).** After the flip, the
  deploy is UNCONFIRMED for N minutes: the health gate (the existing
  per-service checks + dispatcher readyz + a smoke probe) must
  confirm, or the deployer auto-reverts to the previous generation
  and says so loudly. A bad train costs minutes, unattended — which
  matters more as the box's own network path (forge, IdP, cluster)
  joins the loop.
- **Waves (canary).** Wave 1 is the scratch environment the deploy
  script already serves; prod follows only on scratch's confirm.
  Cluster nodes become waves 2..n when they exist.
- **Drains.** Real drain-patch-undrain arrives with k8s; on the
  single box, restart order + health gates approximate it. Named so
  nobody mistakes the approximation for the thing.

## Open questions

All 5 open questions were resolved 2026-08-12 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q1: What is the generation store, and how many do we keep? (resolved)

Resolved 2026-08-12 — accept.

releases/<sha>/ holding bin/ + web-dist/ + step-plugins/ + the .boss-src-fingerprint stamp, with current/previous symlinks and unit ExecStart lines going through the symlink. Keyed by the deployed HEAD short sha — what the conductor records after pull; the fingerprint pre-flight verifies HEAD's content. Keep 3 generations, with an explicit prune step that prints sizes (this box has had its disk-full day). The web dist joins the generation: rsync --delete retires, and the SPA's content-hashed naming becomes real make-before-break with a revert path.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q2: What constitutes the confirm, and what is N? (resolved)

Resolved 2026-08-12 — accept.

Confirm = every deployed unit's health probe 200, reusing the probe_one roster so the confirm cannot drift from the deploy list; plus dispatcher readyz; plus one jobs-api write round-trip through the HTTP API (sentinel POST, read back, delete). Read at +2 and +8 minutes — the delayed second reading catches the dispatcher silent-death class — with N=10, aligned to the reconcile cadence. The conductor completes the train Job's deployed step only on the confirm marker; an auto-revert reopens it. Brewery-calibrated verify-replay floors stay out of the confirm.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q3: What does auto-revert cover? (resolved)

Resolved 2026-08-12 — accept.

Auto-revert re-points binaries + web dist (+ step-plugins) and restarts. Schema (expand/contract, roll-forward only), registries, the log, and data stay. Two riders: the emitted /etc config bodies snapshot into the generation and restore on revert — or config legislates the same N-1 tolerance expand/contract gives schema; and events written during the unconfirmed window stay in the log forever (traffic never rolls back), so projections and rebuilders tolerate unknown event kinds — closure doing revert-safety work.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q4: Who owns the mechanism? (resolved)

Resolved 2026-08-12 — accept.

The proposed split, with one hard amendment: the confirm/revert evaluator is a separate systemd unit (boss-deploy-confirm in the TIMERS array), armed at flip — never in-process waiting inside the deployer. The 45-minute TimeoutStartSec kill mid-build is the proof: a dead-man switch that dies with the deployer reverts nothing. deploy-services.sh grows generation/flip/revert verbs; the evaluator reads +2/+8 and fires revert at +10 on UNCONFIRMED; the conductor's reconcile does the Job bookkeeping; the maintenance family opens a Job on any standing UNCONFIRMED. TimeoutStartSec gets fixed regardless.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q5: When do waves become real? (resolved)

Resolved 2026-08-12 — accept.

Scratch-first ordering lands with the generations work — per-env current symlinks are what make a wave seam possible at all; today scratch units run the same binary file as prod, so installing flips both environments in the same instant. Named honestly: scratch's confirm covers the 9 paired services only (dispatcher and gateway are prod-only), so wave 1 reduces prod's exposure but never replaces prod's own confirm + dead-man. Per-node waves and true drains stay parked on dev-cluster Q4.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.
