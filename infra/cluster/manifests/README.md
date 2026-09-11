# Cluster manifests — BOSS on the dev cluster (namespace `boss`)

The Kubernetes manifests that configure the cluster BOSS — the
system of record. This directory is the single source of truth for
cluster config, and it converges the same way code does:

**The rule**

- Cluster config changes land as cars through the train, like any
  other change. Edit a manifest here, ship it, done.
- On every converge, the runner on the forge host
  (`infra/forge/cluster-deploy-runner.sh`) runs
  `kubectl apply -f` on this directory from the freshly fetched
  tree — idempotently, before rolling the image, so the tag it just
  built supersedes the placeholder tag committed in `boss.yaml`.
- **Hand-applied changes are drift.** They survive only until the
  next converge, and the next converge is at most ten minutes after
  the next merge to forge main. If it matters, it goes through the
  train.
- **The apply is additive — there is no `--prune`.** Deleting a file
  here removes the *declaration* and leaves the *object* running.
  Removing it is a named human step (`kubectl -n <ns> delete
  <kind> <name>`), and
  `infra/lint/a-deleted-manifest-leaves-no-object.sh` is what makes
  sure you are told: the converge runs it after the apply, it names
  every object the tree deleted that is still live and every live
  object in `boss`/`boss-dev` that no manifest declares, and its
  header carries the argument for why `--prune` is refused rather
  than configured. So: a deletion lands as a car like any other
  change, and the converge hands whoever reads it the one command to
  finish the job.
- **Secrets never live here.** Every manifest references its
  secrets by name only (`boss-secrets`, `boss-oidc`, `resend`,
  `boss-backup-key`, `forgejo-registry`, `cloudflare-api-token`,
  `boss-tls`); the Secret objects themselves are created out-of-band
  and stay out-of-tree.

**Code, config and schema all converge from the tree, every deploy**

Config converges here, code converges in the image — and the database
schema converges the same way. The `boss-init` initContainer runs
`infra/postgres/migrate.sh` in manifest order on *every* pod start,
against a fresh database and an existing one alike; the runner applies
only manifest entries missing from `schema_migrations`, prints each file
it applied plus an `applied N, already recorded M, of K manifest
entries` summary, and fails the container — and so the rollout — if any
migration errors. It used to skip a database that already had a schema,
which is how four migrations (112, 113, 114, 116) accumulated unapplied
on 2026-08-13 while the image and the manifests rolled forward: the
station registry shipped, the deploy reported success, and
`GET /api/stations` answered 500 `relation "stations" does not exist`.
**Hand-applied schema is drift**, exactly like a hand-applied manifest —
it stabilises the box for an hour and hides the fact that the tree and
the cluster disagree. A schema change is a new file appended to
`infra/postgres/schema/`, shipped through the train, never
an edit to a file that has already been applied (the runner refuses
those by name).

**What's here**

| file | what it is |
|---|---|
| `boss.yaml` | The single-pod BOSS topology: namespace, Postgres, NATS, the boss deployment, gateway Service on LB 10.20.0.30 |
| `boss-jobs-internal.yaml` | Machine door for the SoR jobs API — LB 10.20.0.34:7900 for the conductor and operator tooling |
| `boss-tls.yaml` | One-shot Jobs: DNS A record + lego Let's Encrypt cert for boss.algedonic.dev (publishes secret `boss-tls`) |
| `boss-tls-front.yaml` | Caddy TLS front terminating boss.algedonic.dev on LB 10.20.0.33 |
| `boss-backup.yaml` | Nightly pg_dump CronJob, 14-day PVC retention, offsite ship to boss-gcp |
| `boss-dev.yaml` | Namespace `boss-dev`: a development pod on cluster hardware, with its own Postgres sidecar and a single-replica workspace volume |

`boss-dev.yaml` is the one file here that does not configure the
system of record. It is in this directory because it converges the
same way and by the same runner, and splitting it out would mean a
second convergence path to keep honest. Note that it declares its own
namespace: the dev pod is deliberately NOT in `boss`, so a shell in it
does not reach production secrets and services by default. Its Postgres
sidecar is the load-bearing part — `boss_testing::TestDb` defaults to
`127.0.0.1`, which in that pod can only be the sidecar, so the
"port-forward turned my test suite on production" incident of
2026-08-14 is removed by construction rather than by discipline.

**Pod security — a workload declares the uid it runs as**

A manifest here states its uid rather than inheriting it from its
image. The gate never builds or applies this directory, so a green
pre-flight says nothing about it; the thing that holds the invariant is
`infra/lint/a-workload-declares-the-user-it-runs-as.sh`, which also
carries the exemption list and the reason for each entry. That lint is
the authority — this paragraph deliberately does not repeat the list
(CLAUDE.md §9a).

Why it is the *declaration* that matters and not the behaviour: most
workloads here run the BOSS image, which is already uid 1500, so
declaring it changes nothing at runtime. But the pod-security cliff is
cluster-wide rather than per-namespace — `boss.yaml` enforces the
baseline profile on the `boss` namespace only, `boss-dev` declares no
labels at all, and `restricted:latest` is the Talos machine-config
default, outside this repo. A workload that states `runAsNonRoot` +
`runAsUser` survives that default being tightened; one that inherits
its uid from an image is admitted or refused on a property no file in
this tree records.

**Measure the uid, never infer it from the image name.** The two halves
are separate answers: `fsGroup` is what makes a *volume* writable, and
`runAsUser` is who the process is — a manifest can carry one and still
be missing the other, as `boss.yaml` and `boss-conductor.yaml` both
were. For a running pod, read it from the live cluster; for a CronJob
that is not running, read what the image actually does (the BOSS image
ends on `USER boss` over `useradd --uid 1500` in
`infra/oss-quickstart/Dockerfile`, the Dockerfile
`infra/forge/cluster-deploy-runner.sh` builds these tags from). Where
the uid a workload's *data or credentials* require has not been
established — Postgres, NATS, Caddy, lego, cloud-sdk, and the backup
pod whose ship-key only ever worked because it runs as root — guessing
breaks the service rather than one check. That is an exemption with a
reason and its own car, not a declaration.

Note on the one-shot Jobs in `boss-tls.yaml`: `kubectl apply` on an
existing completed Job with an unchanged spec is a no-op; the Jobs
re-run only if deleted from the cluster (deliberate — deleting the
cert Job is how you force a re-issue, mindful of Let's Encrypt
duplicate-cert limits).

**What's deliberately not here**

- Talos machine configs, kubeconfig, talosconfig — they embed
  cluster PKI and credentials; they stay in the operator's
  out-of-tree home (`~/talos-homelab/v2/`).

  **But the addresses are not secret, and their absence cost two
  hours on 2026-08-13.** When the jobs door went dark, nothing in
  this repo could answer "where is the cluster?" — so the question
  was answered from `~/talos-homelab/`, which holds TWO generations
  with nothing marking which is live. The v1 configs (`192.168.1.x`)
  were read as current, the nodes appeared dead, and a healthy
  cluster was power-cycled while the actual fault — a crash-looping
  init container — sat unexamined. The inventory below is the fix:
  the facts you need at 2am, none of which are credentials.

### Node inventory — the live cluster (v2)

Verified against the running cluster 2026-08-13 (`kubectl get nodes`,
`talosctl etcd members`), not copied from a config file.

| node | address | role |
|---|---|---|
| cp-1 | `10.20.0.11` | control plane, etcd member |
| cp-2 | `10.20.0.12` | control plane, etcd member |
| cp-3 | `10.20.0.13` | control plane, etcd member |
| — | `10.20.0.10` | control-plane VIP (shared, not a machine) |

There are **three** machines, not four or five: `10.20.0.10` is a
virtual IP. Workloads run on the control-plane nodes; there is no
separate worker in the cluster today.

Reaching them, with the v2 credentials and an explicit endpoint —
`talosctl` takes its endpoint from the config context, so a stale
`talosconfig` will dial the old addresses no matter what `-n` says:

```
talosctl --talosconfig ~/talos-homelab/v2/talosconfig -e 10.20.0.11 -n 10.20.0.11 version
KUBECONFIG=~/talos-homelab/v2/kubeconfig kubectl get nodes -o wide
```

`~/talos-homelab/*.yaml` (no `v2/`) is the **retired v1 generation**
on `192.168.1.x`. It is not the cluster. If you are reading addresses
out of `final-cp-*.yaml` at the top level, you are reading the wrong
cluster — that is exactly the trap that was fallen into.

Talos has no SSH. There is no shell on these machines; `talosctl`
over gRPC on `:50000` is the only interface, so "can we ssh in" is
always no, healthy or not.
- Non-BOSS cluster infrastructure (Kanidm, cert-manager installs,
  Longhorn, Cilium/MetalLB pools, the lego/Origin CA issuers) —
  owned by the cluster, not by BOSS.
- `step-plugins.yaml` — a generated ConfigMap (72KB of JS built
  from `infra/step-plugins/*.js` via the `kubectl create configmap`
  command documented in its header). The sources are already in
  tree; committing the derived artifact would be a second copy that
  drifts (CLAUDE.md §9a).
