# Runbook: Dev environment bootstrap (fresh box)

End-to-end setup for spinning up a BOSS dev workflow on a fresh
Ubuntu 24.04 host. Walks from "nothing installed" to "all
services healthy + brewery data seeded via API". Captures the
sharp edges of a fresh-box bring-up so the next person hits them
as text on a page, not as crashes.

Audience: whoever is bringing up a new dev VM (or rebuilding one
after a wipe). Assumes Ubuntu 24.04, passwordless `sudo`, and
the repo cloned at `/opt/boss`.

## 0a. Disk layout (GCP-specific; skip on non-GCP boxes)

`boss-dev-1` carries state — Postgres data and build artifacts —
on dedicated disks separate from the boot disk so a fat-fingered
`rm -rf target` or a runaway DB never competes with the OS for
space, and the boot disk can be re-imaged without touching state.

Layout on `boss-dev-1` (the current `boss-dev-1`):

| Disk | Size | Mount | Holds |
|---|---|---|---|
| boot (`pd-balanced`) | 50 GB | `/` | OS, `/opt/boss` source, /usr/local/bin |
| `boss-dev-1-pg` (`pd-balanced`) | 100 GB | `/var/lib/postgresql-data` | Postgres data dir (`main/`) |
| `boss-dev-1-build` (`pd-balanced`) | 100 GB | `/var/lib/boss-build` | `target/`, `~/.cargo/` |

Provision the two extra disks once, on first setup of a fresh
`boss-dev-1`:

```sh
ZONE=us-central1-a
INSTANCE=boss-dev-1   # rename to boss-dev-1 once the rest of the fleet exists
gcloud compute disks create ${INSTANCE}-pg    --size=100GB --type=pd-balanced --zone=$ZONE
gcloud compute disks create ${INSTANCE}-build --size=100GB --type=pd-balanced --zone=$ZONE
gcloud compute instances attach-disk $INSTANCE --disk=${INSTANCE}-pg    --device-name=boss-pg    --zone=$ZONE
gcloud compute instances attach-disk $INSTANCE --disk=${INSTANCE}-build --device-name=boss-build --zone=$ZONE

sudo mkfs.ext4 -L boss-pg    /dev/disk/by-id/google-boss-pg
sudo mkfs.ext4 -L boss-build /dev/disk/by-id/google-boss-build
sudo mkdir -p /var/lib/postgresql-data /var/lib/boss-build

PG_UUID=$(sudo blkid -s UUID -o value /dev/disk/by-id/google-boss-pg)
BUILD_UUID=$(sudo blkid -s UUID -o value /dev/disk/by-id/google-boss-build)
echo "UUID=$PG_UUID    /var/lib/postgresql-data ext4 defaults,nofail 0 2" | sudo tee -a /etc/fstab
echo "UUID=$BUILD_UUID /var/lib/boss-build      ext4 defaults,nofail 0 2" | sudo tee -a /etc/fstab
sudo systemctl daemon-reload
sudo mount -a
sudo chown boss:boss /var/lib/boss-build
```

After §1 (Postgres bootstrap) installs the cluster on the boot
disk, relocate the data dir to the dedicated disk:

```sh
sudo systemctl stop postgresql
sudo rsync -a /var/lib/postgresql/16/main/ /var/lib/postgresql-data/main/
sudo chown -R postgres:postgres /var/lib/postgresql-data
sudo chmod 700 /var/lib/postgresql-data/main
sudo sed -i \
    "s|^data_directory = '/var/lib/postgresql/16/main'|data_directory = '/var/lib/postgresql-data/main'|" \
    /etc/postgresql/16/main/postgresql.conf
sudo mv /var/lib/postgresql/16/main /var/lib/postgresql/16/main.retired
sudo systemctl start postgresql
```

After §3 (workspace build), relocate the build artifacts:

```sh
mkdir -p /var/lib/boss-build/target /var/lib/boss-build/cargo
rsync -a /opt/boss/target/    /var/lib/boss-build/target/
rsync -a /home/boss/.cargo/  /var/lib/boss-build/cargo/
rm -rf /opt/boss/target
mv /home/boss/.cargo /home/boss/.cargo.retired
ln -s /var/lib/boss-build/target /opt/boss/target
ln -s /var/lib/boss-build/cargo  /home/boss/.cargo
```

The `.retired` directories are kept as a safety net — once a few
days of normal use confirm everything still works, delete them.

## 0. Toolchain prereqs

Nothing on the box ships with what we need. Order matters — Bun's
installer hard-requires `unzip`, so apt has to finish first.

```sh
# 1. apt packages — postgres, build deps, unzip (required by bun)
sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    build-essential pkg-config libssl-dev curl ca-certificates \
    unzip jq postgresql postgresql-contrib

# 2. Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
. "$HOME/.cargo/env"

# 3. Bun (only after apt installed unzip)
curl -fsSL https://bun.sh/install | bash
export PATH="$HOME/.bun/bin:$PATH"
```

Verify: `cargo --version && bun --version && psql --version &&
pg_isready` should all succeed.

## 0b. The pre-push pre-flight (one command, do not skip)

```bash
git config core.hooksPath infra/git-hooks
```

That is the whole install. It points git at the tracked hooks directory,
so `infra/gate.sh --quick` runs before every push: `cargo fmt --check`
plus the lints that need no build, about 11 seconds.

WHY IT IS A HOOK AND NOT ADVICE. The same class of failure cost a full
gate twice on 2026-08-28 — roughly 40 minutes of cluster time, a
scheduled pod and a clone, to learn that `cargo fmt` had been run on one
crate and not another. The second time, `--quick` already existed and had
been invoked; it was chained with `;` instead of `&&`, so the push went
out regardless. A check that can be stepped over by punctuation is not a
check.

It is NOT a gate: nothing compiles, so clippy, the build and the test
suites are still unproven when it passes.

Escape hatches, both loud:

```bash
BOSS_SKIP_PREFLIGHT=1 git push    # skips this hook, prints that it did
git push --no-verify              # git's own, skips every hook
```

## 1. Postgres role + databases

`bootstrap-boss.sh` and `bootstrap-scratch.sh` (thin wrappers
over the shared engine, `bootstrap-db.sh`) assume a `boss` role
already exists with password `boss` and connect via `127.0.0.1`.
Create it first or every subsequent step fails on auth.

```sh
sudo -u postgres psql -d postgres -c \
    "CREATE ROLE boss WITH LOGIN SUPERUSER PASSWORD 'boss'"
```

Bootstrap both databases. They're identical — `boss` is prod
shape, `boss_scratch` is the +1000-port scratch DB the simulator
writes to.

```sh
sudo /opt/boss/infra/postgres/bootstrap-boss.sh
sudo /opt/boss/infra/postgres/bootstrap-scratch.sh
```

Expect WARN lines about `boss-operator-baseline-seed` and
`boss-platform-workflow-seed` not being on PATH. That's fine —
the binaries don't exist yet; we'll re-run after the build.
(Workflow seeding is split three ways:
`boss-platform-workflow-seed` — a real, load-bearing binary that
`bootstrap-db.sh` invokes here, and that the compose install's
init runs as its `[2/4]` step — loads the platform Workflow
bundle shipped as data in `infra/platform/workflows.toml`,
insert-if-missing; `boss-jobs-api` additionally reconciles its
code-defined platform meta-kinds on every startup; and tenant
Workflows load via the per-tenant prepare step —
`boss-brewery-sim prepare`. See
`docs/design/platform-vs-tenant-jobkinds.md`.)

Tenant **policy** rules follow the same pattern: core's
`default_rules` ships only platform-admin / audit-readonly /
smoke-tester / guest, and tenant role grants (sales-rep,
brewer, controller, …) arrive via `boss-policy-bootstrap
--seeds examples/<tenant>/seeds/policy_rules.toml`. The
brewery seed-regen + reset-to-baseline scripts call it
inline; a fresh manual bootstrap should invoke it after the
Workflow bootstrap step.

## 2. NATS

```sh
sudo /opt/boss/infra/nats/setup.sh
```

Verify: `systemctl is-active nats-server` → `active`.

## 3. Build the workspace

A plain `cargo build --release --workspace` is **not** sufficient:
a handful of `*-api` bins declare `required-features = ["postgres"]`
on their bin target (`boss-policy-api`, `boss-classes-api`,
`boss-locations-api`, `boss-subject-kinds-api`), so the workspace
build skips them entirely. Use the canonical recipe, which reads
each bin's `required-features` from `cargo metadata` and builds it
with exactly those — no list to drift:

```sh
cd /opt/boss
./infra/build-release.sh
```

It runs two passes: `cargo build --release --workspace` for the
libs and default-feature bins, then one `cargo build -p X --bin Y
--features ...` per gated bin. `--verify` additionally asserts each
gated bin actually linked sqlx.

Install everything that begins with `boss-` to `/usr/local/bin`
(skip `.d` dependency files). `deploy-services.sh` and the
systemd units expect them there.

```sh
cd /opt/boss/target/release
sudo install -m 755 -t /usr/local/bin/ $(ls boss-* | grep -v '\.d$')
```

## 4. Re-seed the registries (binaries are now on PATH)

The first `bootstrap-*.sh` run skipped `boss-operator-baseline-seed`
(not built yet). Now that it's installed, re-run to write the
platform-baseline employees into `audit_log`:

```sh
sudo /opt/boss/infra/postgres/bootstrap-boss.sh
sudo /opt/boss/infra/postgres/bootstrap-scratch.sh
```

Expect two `operator hired` lines per DB — `emp-audit` (the
system audit account from `infra/operator-baseline/operator_hires.toml`)
plus `emp-bootstrap-admin`, which the seed injects from
`BOSS_BOOTSTRAP_ADMIN_EMAIL` (or the first email in `BOSS_AUTH_FILE`).
The Workflow registry is **not** seeded here. `boss-jobs-api`
reconciles the platform kinds (`platform_kinds()`) on startup (§5);
the brewery's tenant kinds are published by the converged prepare
step, `boss-brewery-sim prepare` (run by
`infra/postgres/validate-brewery-sim.sh`, `reset-to-baseline.sh`, and
§7 below). So the `bootstrap-boss.sh` "seeded Workflows" tail just
counts whatever already landed in the `workflows` table.

## 5. Deploy services

```sh
sudo /opt/boss/infra/deploy-services.sh both
```

Boots every service in the port table (`boss-ports-list` — 9
paired prod+scratch services, 16 prod-only solo services). Health
probes follow.

**Known false negatives in the probe output.** The script greps
`/api/<name>/health` but `boss-classes-api`, `boss-locations-api`,
and `boss-subject-kinds-api` don't expose `/health` at all —
they show as `000???` / 404 even when fine. Sanity-check with a
real route:

```sh
curl -s "http://127.0.0.1:7800/api/classes?subject_kind=employee" | head -c 200
curl -s  http://127.0.0.1:7820/api/locations                       | head -c 200
curl -s  http://127.0.0.1:7830/api/subject-kinds                   | head -c 200
```

`boss-jobs-api` and `boss-calendar-api` are **paired services
managed by deploy-services.sh** — the script folds them into
`PAIRED_SERVICES` and writes their config + unit files alongside
the others. No manual install needed; the
`./infra/deploy-services.sh prod` step above (or `both` to also
deploy the scratch counterparts on `:8900` / `:8860`) covers
them.

## 6. Frontend + gateway (optional)

Build the SPA and stage it where the gateway expects:

```sh
cd /opt/boss/apps/web
bun install
bun run build
sudo /opt/boss/infra/deploy-web.sh   # rsyncs dist → /var/lib/boss-web/dist
```

Minimum unit that just serves the SPA + reverse-proxies the APIs
(the v0.1 default — file-backed credentials, no SSH-CA):

```sh
sudo mkdir -p /var/lib/boss/auth
sudo tee /etc/systemd/system/boss-gateway.service > /dev/null <<'EOF'
[Unit]
Description=BOSS Gateway (dev)
After=network-online.target nats-server.service postgresql.service
Wants=network-online.target

[Service]
Type=simple
Environment=RUST_LOG=info
Environment=BOSS_LISTEN=0.0.0.0:4443
Environment=BOSS_STATIC_DIR=/var/lib/boss-web/dist
Environment=BOSS_AUTH_PROVIDER=local-auth
Environment=BOSS_AUTH_FILE=/var/lib/boss/auth/credentials.toml
ExecStart=/usr/local/bin/boss-gateway
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now boss-gateway

# Bootstrap the admin credentials before first login:
sudo boss-auth add admin@example.com
```

An SSH-CA flow (short-lived issued SSH certs + central
revocation) is future work — `infra/blueprints/` does not exist
in this tree. Cloud-provider blueprint recipes (an ssh-ca one
among them) are queued post-release; see the **Cloud-provider
blueprints** entry in `TODO.md`.

## 7. Seed the brewery tenant via the live API

The whole point of this exercise — every row goes through a
write API, never a direct INSERT.

```sh
boss-brewery-sim prepare
```

The converged prepare step seeds the whole tenant through the public
API, in order: the Class registry, the brewery Workflows (real
`workflow-design` Jobs), tenant policy grants, then the data — the
brewery exec team (CTO/COO/CEO/owner, from
`examples/brewery/seeds/operator_hires.toml`), the generated
employee roster (`seed_employees`, deterministic seed=42 — 405
rows in `examples/brewery/seeds/employees.json`; with the exec
team and operator baseline the prepared roster lands at 411
people), 50 wholesale
accounts + 30 prospects + the `acc-direct-shop` catch-all, 13
vendors, 6 calendar reservations, 3 bulletins, and finished-product +
raw + asset opening balances. Thousands of rows land in `audit_log`.
Messages are separate: 7 entries to `messages_events`, which by
design never touch `audit_log`. (Bulletins and reservations seed only
when the content (`:7090`) and calendar (`:7860`) services are
reachable; prepare skips them with a warn otherwise.)

Verify counts:

```sh
PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss \
    -c "SELECT source, kind, COUNT(*) FROM audit_log GROUP BY source, kind ORDER BY 1, 2"
```

## 8. Sim replay — full operating motion against scratch

The bigger seed test. The smoke config drives the used-device-shop
tenant through every operating motion (intake → refurb → sale →
install → service → work order → invoice → PO reorder → vendor
invoice → decommission) in ~45 simulated days, posting through the
same write APIs a real user would.

```sh
cd /opt/boss
boss-sim replay \
    --config crates/orchestrators/boss-sim/data/smoke.toml \
    --api-url "scratch://127.0.0.1"
```

`scratch://` is a boss-sim scheme that routes writes to the
scratch services (`+1000` ports, `boss_scratch` DB) so this
doesn't trample the brewery seed in `boss`.

Expect ~3,000+ events in `boss_scratch.audit_log` covering
~21 distinct `source.kind` pairs across assets, commerce,
inventory, shipping, people, and catalog services.

**Caveat:** `infra/verify-smoke.sh` probes the **prod-stack**
ports (7100–7900) over HTTP, so after a `scratch://` replay it
reports every motion as FAIL even though the data is present in
scratch. Verify by querying `boss_scratch` directly:

```sh
PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss_scratch -c "
  SELECT source, kind, COUNT(*) FROM audit_log
  GROUP BY source, kind ORDER BY source, kind"
```

## Open documentation gaps

Discovered while running this list end-to-end. These belong on
the TODO board:

- `deploy-services.sh` should either probe a route the registry
  services actually expose, or registry services should mount
  `/api/<name>/health`. Today's output is misleading.
- `bootstrap-db.sh` could `CREATE ROLE boss` itself rather than
  failing on missing role.
- A single command that chains §0–§7 exists:
  `infra/bootstrap-vm.sh` (fresh Ubuntu 24.04 VM; apt →
  toolchains → Postgres role + DBs → NATS → build → install →
  re-seed → deploy → tenant seed → health probes). Two known
  flaws, each with a packet filed: it does not deploy the SPA
  (§6 stays manual), and it carries a hardcoded binary list
  that can drift from the port registry.
- `infra/verify-smoke.sh` probes the prod-stack ports (7100–7900)
  over HTTP, so after a `scratch://` replay it FAILs every motion
  even though the data is in scratch. It should take the env (or
  the `--api-url` scheme) and probe the matching `+1000` ports.
