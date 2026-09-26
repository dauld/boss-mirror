# Runbook: BOSS operator procedures

Day-to-day and break-glass operations for a BOSS instance. Covers
**backup, restore, deploy, rollback, and reading a host without
ssh**. Keep it terse — every procedure should fit on one screen so
you can scroll while someone else is on the phone.

**There is one way to run BOSS: the container.** The image built by
`infra/oss-quickstart/Dockerfile` runs every service under
`services-launcher.sh`; the cluster runs it as `deploy/boss` in
namespace `boss` (`infra/cluster/manifests/`), and the OSS quickstart
runs the same image under docker compose
(`infra/oss-quickstart/README.md`). The bare-metal path — per-service
systemd units, a host backup tarball, `deploy-services.sh` — was
deleted on 2026-09-18 (design 42277636, backlog e109bd71); nothing in
this runbook applies to a host running services outside the image.

Audience: whoever holds the operator role on the cluster. Assumes the
[CLI](../../crates/orchestrators/boss-cli/README.md) is on PATH with
`BOSS_JOBS_URL` and `BOSS_ACTOR` set (CLAUDE.md §Doors), and — for
the kubectl lines — a kubeconfig for the cluster
(`docs/runbooks/dev-pod-access.md`).

## Quick reference

```sh
boss orient                                  # trains, gates, dock, queue — one read
boss-api GET /api/jobs/health                # the SoR's own commit + health
boss ops forge unit-status boss-train.service --wait   # read a host through the door
kubectl -n boss get pods,cronjobs,jobs        # what the cluster is running
kubectl -n boss logs deploy/boss --tail=200   # the pod's launcher + services
```

Reading a host is an **ops-request**, never ssh: `boss ops <host>
<verb> [args] --wait` files a packet the host's runner answers
(`infra/ops/verbs/` is the allowlist; `df`, `uptime`, `unit-status`,
`journal-tail`, `pod-logs`, `disk-report`, `forge-log` are the reads). The forge's
and boss-gcp's journals are also readable over HTTP on `:19531`
(`infra/forge/OPERATIONS.md` §Reading the host from the pod).

---

## Backup

### How backups work

- **The `boss-pg-backup` CronJob** in namespace `boss`
  (`infra/cluster/manifests/boss-backup.yaml`) runs nightly at 09:10
  UTC: `pg_dump` of the whole `boss` database, gzipped, onto the
  `boss-backups` PVC (14-day retention), then shipped offsite twice —
  to boss-gcp over a deposit-only forced-command key, and to the GCS
  bucket named in secret `boss-gcs-offsite`.
- **Every run leaves a packet** of kind `maintenance-backup` in the
  system of record, opened before the dump and closed with the
  verdict after the last leg; a failed leg fails the Job loudly.
  The cadence-silence sweep alarms when the nightly packet stops
  arriving.
- The dump is plain SQL of everything, but the system of record
  inside it is `audit_log`: a restore is schema + `audit_log` +
  `boss-rebuild-all` (see Restore).

### Trigger a manual backup

```sh
kubectl -n boss create job --from=cronjob/boss-pg-backup boss-pg-backup-manual-$(date -u +%Y%m%d%H%M)
kubectl -n boss logs job/boss-pg-backup-manual-<stamp> --all-containers -f
```

Every leg is an initContainer, in order; the Job stops at the first
leg that fails, so a completed Job means every leg passed.

### Verify the last backup is sane

```sh
boss-api GET '/api/jobs?kind=maintenance-backup&limit=3'     # newest closed "Maintenance completed" < 24 h old
kubectl -n boss get jobs -l job-name --sort-by=.status.startTime | grep pg-backup | tail -3
```

If the newest packet is more than 24 h old, read the last Job's logs
(`kubectl -n boss logs job/<name> --all-containers`) — the failing
leg names itself.

---

## Restore

### When to restore

- A migration or a bad write corrupted state and you want to rewind.
- The cluster's Postgres volume is gone and you are on a fresh
  provision.

**Restore is destructive** and the cluster's database is the system
of record: take a manual backup first (above) so you can compare.

### The procedure

The shape is the migration-as-log-copy sequence
(`docs/design/dev-cluster.md` §The migration is a copy of the log;
`infra/cluster/restore-log-copy.sh` is its executable form for a
log-copy tarball):

```sh
# 1. scale the services down so nothing writes during the reload
kubectl -n boss scale deploy/boss --replicas=0

# 2. reload the dump into the cluster's Postgres
kubectl -n boss exec -i sts/postgres -- sh -c 'gunzip | psql -U boss -d boss' < boss-<stamp>.sql.gz

# 3. bring the services back — the init container converges the schema
#    (infra/postgres/migrate.sh, idempotent) before anything starts
kubectl -n boss scale deploy/boss --replicas=1
kubectl -n boss rollout status deploy/boss

# 4. rebuild every projection from audit_log and prove the chain
kubectl -n boss exec deploy/boss -- boss-rebuild-all
kubectl -n boss exec deploy/boss -- boss-audit-integrity-check
```

### What restore does not cover

- **NATS JetStream state.** Messages in flight at dump time are lost;
  the event relay re-drives from the transactional outbox and the
  rebuild re-derives every projection, so nothing that reached the
  log is lost.
- **Secrets.** The `boss-secrets` Secret (machine token, session
  key) is not in the dump; it lives in the cluster, and a restored
  database against a rotated key needs the key ceremony re-run
  (`docs/runbooks/machine-token-activation.md`).
- **The tenant's files.** `file_refs` bytes live outside the
  database and are off in every container deploy today.

---

## Deploy

**Nothing is deployed by hand.** A car merges on a train; the merge
fires a `converge` ops-request; the forge's `cluster-deploy-runner`
builds the image for that sha, applies `infra/cluster/manifests/`
and rolls `deploy/boss`; the conductor then verifies convergence by
reading the sha the running jobs API reports
(`GET /api/jobs/health` → `capabilities.commit`) and completes the
train's `converged` step — or files a loud packet when convergence
lags. The whole chain is packets: `boss orient` shows where each
train sits.

```sh
boss orient                                          # which train is where
boss-api GET /api/jobs/health                        # what sha is live
boss ops forge unit-status cluster-deploy-runner.service --wait
boss ops forge journal-tail cluster-deploy-runner 200 --wait
```

A deploy that has to be forced — the runner is held, or the forge is
the thing that is down — is documented, by name, in
`infra/forge/OPERATIONS.md` §5–6. Read that before touching anything.

### Post-deploy checks

```sh
boss-api GET /api/jobs/health          # commit == the train's merge sha
kubectl -n boss get pods               # deploy/boss Running, restarts 0
kubectl -n boss logs deploy/boss --tail=100 | grep -i 'SKIP\|error'
```

---

## Rollback

**Roll to a named build, never "the previous one."** `rollout undo`
once moved between two revisions carrying the same broken image and
read as a rollback that was not one (CLAUDE.md §Diagnosis). The
lever is the `rollback-to` ops verb: it rolls `deploy/boss` to the
image built for a named sha and refuses to report success until the
pod is Ready on it.

```sh
boss ops forge rollback-to <sha> --wait
boss-api GET /api/jobs/health          # capabilities.commit == <sha>
```

The forge also runs `cluster-watchdog`, which reads the SoR from
outside the cluster and rolls to the last converged build on its own
after three dark checks — so a cluster that is dark for fifteen
minutes has usually already rolled by the time a human looks. Hold
the converge first (`boss ops forge hold-converge '<reason>' --wait`)
if the next train must not re-deploy over your rollback; release it
with `release-converge`.

### When a rollback alone is not enough

Migrations are forward-only. If the rolled-back binary refuses a
schema the newer one migrated, forward-fix: patch on a car, gate it,
land it. There is no downgrade path, and a restore from a dump that
predates the migration is the only rewind — see Restore.

---

## Incidents

### A service inside the pod will not start

```sh
kubectl -n boss logs deploy/boss --tail=300      # the launcher prints each service's start + SKIPs
kubectl -n boss describe pod -l app=boss         # init container (schema converge) verdict
```

The launcher SKIPs a binary that is not on PATH and says so; a
config a service cannot read is generated at container start by
`infra/oss-quickstart/generate-configs.sh` from the port registry
(`boss-ports`), so a wrong port is a `boss-ports` change, not a
config edit on a host.

### Postgres out of connections

```sh
kubectl -n boss exec sts/postgres -- psql -U boss -c "SELECT count(*) FROM pg_stat_activity;"
kubectl -n boss exec sts/postgres -- psql -U boss -c "SELECT pid, state, query_start, query FROM pg_stat_activity WHERE state != 'idle' ORDER BY query_start;"
```

The cluster's Postgres runs `max_connections=400`
(`infra/cluster/manifests/boss.yaml`) against ~24 services × pool 10;
a storm is one caller in a tight loop — find it before killing
connections.

### The system of record is dark

Do not diagnose from the pod first: its only route is the LAN.
`infra/forge/OPERATIONS.md` §5–6 is the sequence (the watchdog's
journal on the forge says what it saw and what it did; `boss ops
forge …` still answers when the cluster does not, because the forge
runs its own runner).

### A CronJob chore stopped leaving packets

The cadence-silence sweep files an alarm per silent kind. Read the
last Job:

```sh
kubectl -n boss get cronjobs
kubectl -n boss get jobs --sort-by=.status.startTime | tail -20
kubectl -n boss logs job/<name> --all-containers
```

### Disk filling up

On the forge: `boss ops forge disk-report --wait`, then
`boss ops forge reclaim-disk <floor-GB> --wait` (bounded; the
per-train CI images in the system docker daemon are the usual
culprit — `infra/forge/OPERATIONS.md` §1). On boss-gcp: `boss ops
boss-gcp disk-report --wait` reads the root by directory (read-only;
no reclaim verb serves that host yet). On the cluster: the
`boss-backups` PVC is capped by retention; Longhorn volume usage is
`kubectl get volumes.longhorn.io -n longhorn-system`.

---

## Post-incident checklist

After any break-glass intervention:

- [ ] `boss-api GET /api/jobs/health` reports the sha you expect.
- [ ] `kubectl -n boss get pods` — every pod Running, restarts not climbing.
- [ ] A manual `boss-pg-backup` Job completed (see Backup).
- [ ] If the database was touched: `boss-rebuild-all` +
      `boss-audit-integrity-check` green, and one revenue figure on
      `/finance` spot-checked against `GET /api/ledger/trial-balance`.
- [ ] The intervention is on a packet — an ops-request, an incident,
      or a note on the alarm — so the next operator sees prior art
      in the system of record rather than in someone's memory.

## Related

- [`infra/forge/OPERATIONS.md`](../../infra/forge/OPERATIONS.md) —
  the forge host: units, failure modes in the order they happened.
- [`docs/design/dev-cluster.md`](../design/dev-cluster.md) — topology,
  bring-up, the migration-as-log-copy restore shape.
- [`docs/runbooks/playground-baseline.md`](playground-baseline.md) —
  re-cutting a demo tenant's frozen baseline.
- [`infra/postgres/validate-brewery-sim.sh`](../../infra/postgres/validate-brewery-sim.sh)
  — the replay-rebuild correctness check (a sim-year, then 0 net
  drift across every rebuilder).
