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
  `boss-backup-key`, `forgejo-registry`,
  `cloudflare-tunnel-credentials`); the Secret objects
  themselves are created out-of-band and stay out-of-tree.

**One source, rendered per instance.** This directory is written for
ONE instance — prod, namespace `boss` — and every instance in
`infra/cluster/instances.toml` (today: prod and the playground,
namespace `boss-playground`, brewery tenant, sim on) is this directory
rendered by `infra/cluster/render-instance.sh` with its namespace,
tenant directory (`BOSS_TENANT_DIR` — a directory the image ships, or
`/opt/boss/tenant` where the converge delivers a `tenant_repo` instance's
tenant as the generated `boss-tenant` ConfigMap; backlog f4f5c387),
`BOSS_SIM_ENABLED` and public hostname (`BOSS_PUBLIC_URL`) substituted,
plus the `.boss.svc.cluster.local` names and the LoadBalancer IP pins
(commented out: only prod's copy may hold an address). The converge
renders and applies every instance on every train (design ffc83387,
David 2026-09-16); prod's render is byte-identical to these files, and
a test holds it so. Which manifests are per-instance and which exist
once for the pipeline is `infra/cluster/instance-manifests.txt`, with
the reason on each line — a new file here must be classified there, or
the render refuses by name. Secrets are still per namespace and still
out of tree — and PROVISIONING MINTS an instance's own (backlog
dc1bc724, David 2026-09-16: no hand work unless absolutely required):
on the converge that finds one absent, the runner creates the
instance's Namespace and then mints the internal ones (`boss-secrets`,
`boss-session-key` — random values nobody needs to know), copies the
shared ones (`forgejo-registry`, `resend`) from the instance named as
`shares_with` in `instances.toml`, and creates `boss-oidc` with an
empty `client-secret` so the gateway boots guest-only until the Kanidm
client is registered. Values ride kubectl's stdin, never a journal;
the packet records names as `instance_secrets_minted`. Anything else
the manifests reference is still a person's, and the instance is
skipped by name until it exists (`infra/forge/cluster-deploy-lib.sh`,
`provision_instance_secrets`). Today nothing is: `boss-tls`, the
lego-issued certificate the Caddy front mounted, was the one, and it
kept the playground skipped on every converge (`instances_skipped:
boss-playground (secrets absent: boss-tls)`, measured 2026-09-17) until
Let's Encrypt left the cluster with backlog 21c17ebc — see "What's
deliberately not here". After that a fresh instance comes up on its
first converge and needs only its Kanidm client secret to leave
guest-only.

**The tunnel's credentials — one Secret, created empty by the converge,
filled by the broker.** The connector (`cloudflared.yaml`; backlog
5a2bb0ce, design 4c565f8c) mounts cloudflared's `credentials.json` from
a Secret it names and never carries. No hand mints it (backlog
51c98681, David 2026-09-16: no hand work unless absolutely required):

- **The object.** A Secret the credential broker fills is declared by
  its rule under `infra/dispatcher/rules/` (`secret_namespace` /
  `secret_name` on a `credential.rotate.*` handler — the args the
  broker itself PATCHes). The converge reads those declarations and
  creates each absent Secret EMPTY, never a value, never touching one
  that exists (`infra/forge/cluster-deploy-lib.sh`
  `ensure_declared_secrets`); the converge packet records
  `secrets_declared: created … | present …`. The broker is deliberately
  not granted `create` (`boss-credential-broker.yaml`: it cannot be
  name-scoped), which is why the converge, which holds the admin
  credential, does this half.
- **The value.** A rotate-a-credential packet opened on
  `cloudflare-tunnel-credentials` fires the
  `broker-rotates-the-cloudflare-tunnel` rule (04e5f833): a new tunnel
  is minted, its file PATCHed into the Secret, the connector
  rollout-restarted.

Until the object exists the connector's pods wait in
`ContainerCreating` (the volume is deliberately not `optional`) and
every converge packet records
`cloudflared: skipped (secret absent: cloudflare-tunnel-credentials)`;
between the create and the first rotation the pods start without a
file and the packet records `not-ready` — the designed order. Neither
holds the converge: the connector is not what a train delivers.
(`connector_status` derives the Secret's name from the rendered
manifest exactly as the instance secret gate does.) Once filled, the
field flips to `connected` — the readinessProbe is cloudflared's own
`/ready`, which answers 200 only with a live edge connection — or stays
`not-ready`, which is then a fault to read from the pods. The
credential is a registry row (`boss credential list`: id
`cloudflare-tunnel-credentials`, consumer `cloudflared`).

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
| `cloudflared.yaml` | The Cloudflare Tunnel connector (2 replicas, namespace `boss`): every public hostname's way in, plain HTTP to each instance's gateway Service; TLS is the edge's |
| `cloudflared-config.yaml` | Its config — GENERATED by `infra/cluster/render-tunnel-config.sh` from `infra/cluster/instances.toml` (one ingress rule per instance, then the 404 catch-all); a test holds the file equal to the render |
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
established — Postgres, NATS, cloud-sdk, and the backup
pod whose ship-key only ever worked because it runs as root — guessing
breaks the service rather than one check. That is an exemption with a
reason and its own car, not a declaration.

**What's deliberately not here**

- **Let's Encrypt, lego, a Caddy TLS front, a `boss-tls` Secret, a
  Cloudflare API token** — gone since 2026-09-17 (backlog 21c17ebc;
  design 4c565f8c, decided by David 2026-09-16). From 2026-08-12 the
  cluster minted its own certificate: `boss-tls.yaml` ran two one-shot
  Jobs in `cert-manager` (a grey-cloud A record `boss.algedonic.dev ->
  10.20.0.33` and a lego DNS-01 issue, both with an in-cluster
  Cloudflare token), and `boss-tls-front.yaml` was a Caddy Deployment
  on LB 10.20.0.33 terminating that hostname with the `boss-tls`
  Secret the Job published. The tunnel made every piece dead weight:
  both public hostnames are proxied CNAMEs behind Cloudflare Access
  (`infra/cluster/dns/algedonic.dev.toml`; measured 2026-09-17, zone
  observation f0f2767f: 3 match), the edge holds the certificate, and
  the connector proxies each hostname to
  `http://boss-gateway.<ns>.svc.cluster.local:80` — the front was never
  in that path. What it still cost was the playground: the front's
  `boss-tls` mount was a required Secret nothing mints, so the
  converge skipped `boss-playground` every train. The objects the
  deleted files declared (`Deployment`, `Service` and `ConfigMap
  boss-caddy-config` in `boss`; the two completed Jobs in
  `cert-manager`) are named by `a-deleted-manifest-leaves-no-object`
  on the next converge, and their deletion is the named step it
  prints; the out-of-tree `boss-tls` and `cloudflare-api-token`
  Secrets and lego's `lego-data` PVC are a person's to remove.

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
  Longhorn, Cilium/MetalLB pools, the `cert-manager` namespace and the
  lego/Origin CA issuers in it) — owned by the cluster, not by BOSS.
- `step-plugins.yaml` — a generated ConfigMap (72KB of JS built
  from `infra/step-plugins/*.js` via the `kubectl create configmap`
  command documented in its header). The sources are already in
  tree; committing the derived artifact would be a second copy that
  drifts (CLAUDE.md §9a).
