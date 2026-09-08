# The build plane manages itself: maintenance as protocol

**Status**: in-review — the operating principle for internalizing
build-system operations (David 2026-09-03: "write down the steps we
perform for maintenance into protocol so we don't have to remember or
relearn each time; trigger on both time and monitoring — this is the
power of protocol + Claude"). Companion to
[the-cluster-is-the-system.md](the-cluster-is-the-system.md): where
that doc moves the conductor into the cluster, this one makes the
build plane (forge, registry, CI runner — Plane 2, which stays *off*
Kubernetes) a first-class BOSS-managed system whose upkeep is
protocol, not tribal memory.

## 2026-09-03: it happened again, and the record is why

The bottleneck this doc names recurred while the doc sat uncommitted.
Train `20260903-2046` went red. The forensics, kept here so the next
occurrence is a lookup and not a re-derivation:

- **Every test passed.** The `test` CI job died *after* the suite, at
  the `web install` preflight, on the disk gate: `9GB free, need 12GB.
  Refusing to continue`. The remediation the gate itself prints is
  "drop target/ dirs from landed worktrees." This is a disk failure
  wearing a test-failure's job name — the authoritative signal is the
  per-commit status (`CI / test → failure`), and the cause is only in
  the job log's tail, not in any `test result: FAILED` line.
- **The prior reclaim addressed the wrong consumer.** A registry-blob
  reclaim had been run earlier that day and freed a different
  filesystem; a `df` on some volume read 51GB free. The gate reads the
  **root** volume, where the CI *workspaces* live, and that was at 9GB.
  Lesson, now protocol: the forge root volume has **several
  independent disk consumers**, and reclaiming one says nothing about
  the others. The ownership map (below) is the fix for that confusion.
- **The door that should have let an agent reclaim disk was inert.**
  The `ops-request` mechanism and its `reclaim-disk` verb are on
  `main`, authorized. A read-only `df` ops-request filed against host
  `forge` sat `execute=ready`, unanswered — **zero ops-request jobs
  have ever been answered in the system's history.** The runner is on
  main and not on the host.

Each of those was recoverable knowledge that lived in a head or a
transcript and had to be rebuilt under incident pressure. That is the
exact failure §The principle describes, and writing it down here is
the holding action until the machine below runs.

## The disk-consumer ownership map

The root volume filled and three different reclaimers each disclaimed
it, because **each owns a different consumer and none owns all of
them.** The recurring confusion ("I reclaimed disk, why is CI still
red?") is this table not being written down:

| consumer | who owns reclaiming it | touches the others? |
|---|---|---|
| regenerable docker/build caches, dangling images | `disk-floor-sweep.sh` (hourly, below-floor) | no — never volumes, never registry-by-API |
| **named workspace volumes of crashed CI jobs** (the 63GB-orphan class) | `reap-dead-ci-jobs.sh` (exited FORGEJO-ACTIONS-* only) | no — a *live* job's 74GB `target/` is healthy and must not be pruned |
| Forgejo **registry** container blobs (old image tags) | the registry-tag prune loop (needs a `package`-scoped token) | no — API operation, not host filesystem |
| `target/` in a *live/landed* worktree | the runner's own post-job reclaim (normal finish) | orphaned only when a job crashes → becomes the reap case |

The load-bearing consequence: **a reclaim is only as good as the
consumer it targets.** An agent (or a human) who reclaims caches when
the fill is an orphaned volume has done real work and freed nothing
CI needs. Any reclaim verb must report which consumer it addressed and
the before/after free space on the **root** volume specifically.

## The disk is one 232GB volume, and the reclaim tools are sized wrong

Probed directly on 2026-09-03 (`df -h` + `lsblk` + `docker info`, read
off the host over the journal gateway): the forge host is a **single
232GB NVMe → one partition at `/`**, and Docker's data-root is
`/home/david/.local/share/docker` — *on that same root filesystem*. No
LVM, no dedicated build volume. So `df /` — what every reclaim tool
measures — has always been the right (and only) volume; the confusing
"6GB free" then "93GB free" minutes apart was one volume at two times,
not two volumes. A cold build filled it and then freed it.

The real dynamics, now that the layout is known:

- Baseline occupancy is ~122GB (images, registry blobs, ~45GB of build
  cache) → ~110GB free at rest.
- Every CI job builds `target/` **cold** into a per-job workspace
  volume — **~74GB each** (`locomotive.sh` documents the number and
  sets `BOSS_CI_MIN_FREE_GB=70`). So there is room for roughly *one*
  build's headroom. Concurrent builds, or a creeping baseline, tip it
  past what CI needs; it recovers when builds finish.
- **The reclaim tools are aimed at the wrong size of problem.**
  `disk-floor-sweep`'s floor is 25GB while CI needs 70GB, and it only
  prunes cache crumbs (measured: ~0.5GB a run). `reap` only clears
  *crashed*-job orphans. Neither touches the 74GB live workspaces or
  the ~45GB of stale build cache — the two things that actually move
  the needle.

The clean fix is structural: **give the build system its own volume.**
Put Docker's data-root on a dedicated LV/disk separate from root, so
build growth cannot starve the host that also runs the registry;
reclaim becomes a bounded wipe of *that* volume; and the CI floor
check measures the filesystem that actually fills. Short of that, the
in-place levers are (a) raise the sweep floor toward the ~70GB CI
needs and actually prune build cache + old image tags (both
regenerable), and (b) cap build concurrency so two 74GB builds never
coexist. This section is a `### Qn` below.

## The principle

A maintenance procedure performed by hand is a belief held by whoever
last did it. It decays the moment they forget, and it is relearned —
painfully, under incident pressure — every time. On 2026-09-03 the
build host's disk filled, CI failed, and the fix was a sequence a
human had to reconstruct: mint a scoped token, list the registry,
delete old image versions keeping the newest few, free the space.
That sequence is *knowledge*, and knowledge that lives only in a head
gets the same treatment as every other belief in this system: it is
converted to an artifact — here, a **protocol** — that a machine runs
and the audit log records.

The power is the pairing. The **protocol** encodes what to do and when;
**Claude** (agents) executes the parts that need judgment and writes
the parts that don't into cadence rules and handlers. Together they
turn "someone remembers how to reclaim disk" into "the disk reclaims
itself, and when it can't, it says so to a human."

## Two triggers, one procedure

Every maintenance protocol fires two ways, and the procedure is the
same regardless of which fired it:

- **On time** — a cadence rule (the `maintenance-*` family, the
  `disk-floor-sweep` timer). Routine upkeep that should happen whether
  or not anything is wrong: prune regenerable caches hourly, keep the
  floor.
- **On monitoring** — the estate observer detects a condition (disk
  below a floor, a unit unhealthy, a series gone silent) and the alarm
  raises; a rule turns that raised condition into the same maintenance
  verb firing. Upkeep that should happen *because* something is wrong,
  faster than the next scheduled tick.

The 2026-09-03 disk incident is the worked example: a time-triggered
hourly sweep would have kept the floor; a monitoring-triggered reclaim
(disk-observer → alarm → reclaim verb) would have caught the fill
before CI did. Neither fired, because the pieces exist but were never
installed on the build host — which is the gap this design closes.

## What already exists (and why it didn't fire)

The parts are built; they are inert because the build host is not yet
a BOSS-managed plane:

- `infra/forge/install.sh` — the one idempotent command that installs
  the host's `reap-dead-ci-jobs`, `cluster-deploy-runner`, and
  `disk-floor-sweep` unit pairs from the checkout. **It exists and is
  correct.** Its own header records the exact defect it was built to
  end: units authored and committed but never installed
  (`reap-dead-ci-jobs` sat uninstalled through the 2026-08-17 fill).
  But **nothing runs `install.sh`** — it is invoked by hand over ssh
  (`ssh 10.20.0.15 'cd /home/david/boss && git pull && sudo
  infra/forge/install.sh'`). So a unit is only as installed as the
  last person to remember, and `disk-floor-sweep` — added after the
  last hand-run — has never fired.
- `infra/ops/ops-runner.sh` + its `reclaim-disk` verb — the door an
  agent uses to reclaim without ssh. **On main, and NOT in
  `install.sh`'s UNITS list** (it lives under `infra/ops/`, a
  different directory), so even a hand-run of `install.sh` would not
  install it. This is the same authored-but-uninstalled defect, one
  directory over.
- `infra/forge/disk-floor-sweep.sh` + its timer — prunes docker build
  cache, dangling images, and registry-verified old tags below a floor.
  **Landed on main; its timer has never run** (zero
  `maintenance-disk-floor-sweep` packets — the timers-leave-a-packet
  signal), because `install.sh` has not been run since it landed.
- The `reclaim-disk` ops verb — the same sweep as an on-demand
  ops-request packet. **Needs the ops-runner installed on the build
  host.**
- The estate disk + unit observers and the alarm that hears them —
  **built and landed**, but the observers are not installed on the
  build host, so the disk series that would trigger a monitoring
  reclaim does not flow.

**The keystone, stated plainly:** every "landed but never installed"
gap above has one cause — the forge host does not converge. The
cluster runs `cluster-deploy-runner` every ten minutes to adopt its
own manifests from `main`; the forge host has no equivalent loop for
its *own* `infra/forge` + `infra/ops` units. `install.sh` is the
adopter; what is missing is the thing that *runs* it. Give the forge
host a converge timer (`git pull && install.sh`, ops-runner added to
UNITS) and the whole "authored but inert" class closes at once — this
is car 4 below, promoted from "one structural gap" to the keystone,
and it is the answer to Q1: **converge, do not one-time-bootstrap.**
Note the bootstrap paradox: the converge loop itself must be installed
once by hand, so exactly one `sudo install.sh` on the host remains a
human action — but only ever *one*, and it also activates the reclaim
timers that unblock the currently-red train.

## The gap: the registry cleanup needs a credential, and the credential should be protocol too

The disk-floor-sweep prunes *docker/containerd* (host-local, no
credential). But a large share of the build host's disk is the
**Forgejo registry** (container package blobs), and reclaiming that is
a Forgejo API operation needing a `package`-scoped token — which the
sweep script does not have. On 2026-09-03 that token had to be minted
by hand from the admin root.

That is the last piece of tribal knowledge, and it resolves the same
way: **the broker mints the scoped token the protocol needs.** The
credential broker today mints exactly one `write:repository` token; it
should mint *scoped tokens on request* (a `package`-scoped token for
the disk protocol being the first real need), so a sufficient-policy
actor — a maintenance handler, an agent — obtains the credential by
following protocol, never by reading root. This is
[the credential-registry leg](../design/) made concrete: the token is
available to any actor of sufficient policy, following protocol,
including Claude.

## Cars this decomposes into

1. **Broker mints scoped tokens on request** — extend the
   `credential.rotate.forgejo` broker so a rotation/issue packet can
   name its scopes (e.g. `read:package,delete:package`), mint a
   short-lived token, hand it to the requesting handler, and revoke it
   after. Retires the hand-minting the 2026-09-03 reclaim required.
2. **Registry reclaim joins the disk-floor-sweep** — the sweep gains a
   registry-cleanup pass (delete container versions older than the
   newest N via the Forgejo API, using a broker-minted package token),
   so time-triggered upkeep covers the registry, not just containerd.
3. **The build plane becomes estate** — the forge/registry/CI-runner
   services become estate subjects with unit names + restart verbs +
   health probes; the disk observer installs there; the monitoring
   trigger (alarm → reclaim) is wired.
4. **(KEYSTONE) The build host adopts its own units** — a
   `forge-converge` unit pair (timer + service) that runs `git pull &&
   sudo infra/forge/install.sh` on a cadence, plus **adding
   `ops-runner` to `install.sh`'s UNITS list** so the door installs
   with everything else. This closes the entire "landed but never
   installed" class (Q1, resolved: converge, do not one-time-bootstrap)
   and is prerequisite to cars 1–3 actually *running* rather than
   merely existing. The one remaining human action is a single
   bootstrap `sudo install.sh` to install the converge loop itself.
5. **The overdue-step signal triggers a reclaim** — connects to the
   expected-step-completion-time work (the anomaly-alert protocol,
   David 2026-09-03). A CI `test` step that blows past its expected
   duration, or a train step stalled at the disk gate, is a monitoring
   trigger: the same `reclaim-disk` verb fires from an overdue-step
   alarm, not only from a disk-floor observer. An expected-duration on
   the maintenance steps also makes *this* protocol self-observing —
   a reclaim that never completes is itself an anomaly.

## Decision history

### Q1 (RESOLVED 2026-09-03): The build host converges — it is not a one-time bootstrap.
The 2026-09-03 fill answered this empirically. A single bootstrap
leaves the *next* authored-but-uninstalled unit inert exactly as
`disk-floor-sweep` was left — the failure recurs the moment a new unit
lands. The build host grows a `forge-converge` loop that runs
`install.sh` from the tree on a cadence (car 4, keystone), mirroring
the cluster's 10-minute converge. Exactly one hand action survives —
the bootstrap that installs the converge loop itself — and nothing
after it. Chosen over the one-time-bootstrap option because the
recurring cost is the whole point of the doc.

## Open questions

### Q2: How short-lived should broker-minted maintenance tokens be?
A `package`-scoped token minted for one reclaim run should be revoked
after — but the reclaim may run unattended on a timer. Mint-per-run
and revoke-after (more API calls, smallest exposure window), or a
standing scoped token the broker rotates on a schedule (fewer calls,
a live credential to protect)? The estate-alarm cadence and the
rotation policy decide.

### Q3: Which reclaim actions are safe to run unattended vs need a human?
Pruning regenerable caches (build cache, dangling images) is safe
unattended. Deleting registry image versions is *mostly* safe (old
images are rollback targets; keeping the newest N is the guard) but
is a destructive shared-artifact operation. Should registry-version
deletion be time-triggered unattended, or should it require the
monitoring trigger (only reclaim aggressively when disk is actually
low) plus a floor on how many versions it will ever delete in one run?

### Q4: Does the build system get its own volume, or do we size the in-place tools right?
The forge host is one 232GB disk with Docker on root; a cold CI build
wants ~74GB and there is room for one. The structural fix is a
dedicated LV/disk for Docker's data-root, so build growth can't starve
the host and reclaim is a bounded wipe of one volume. The in-place
alternative is to raise the sweep floor toward 70GB, prune build cache
and old image tags aggressively, and cap concurrency so two cold builds
never coexist. The dedicated volume is more provisioning (a David
action — new disk or repartition) but ends the "builds starve the
registry" coupling for good; the in-place path ships as data/registry
changes today but leaves the single-volume coupling in place. Which?
