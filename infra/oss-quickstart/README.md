# BOSS OSS quickstart

The supported clean-install path is **Docker compose**: three
long-running containers (Postgres, NATS, and `boss-services`
running every BOSS binary) plus a one-shot `boss-init`. No local
Rust / Bun / Postgres / NATS install needed — the image carries
everything. **Expect ~20–25 min on a 2-vCPU VM** for the first
`docker compose up` (the Rust image build dominates); subsequent
runs start in seconds because the image cache and Postgres volume
persist.

Working on BOSS itself? See
[Developing against the source tree](#developing-against-the-source-tree)
below — a host-native script that runs each service as a plain
process you can rebuild one crate at a time. That is a development
convenience, not a second supported install.

> **Verified on:** the compose path first passed end-to-end on
> 2026-08-22, on an image built from train #94 plus the
> `fix/empty-roster-is-not-a-fact` roster-cache fix. That fix is
> **not yet on `main`** (checked at train #98, `ab19b988`) —
> until it lands, a fresh install from pure `main` still aborts
> at brewery prepare, on Q7 owner resolution.

> ⚠  **Not production-ready.** The stack runs the whole platform
> on one machine. Auth is the file-backed local-auth provider —
> real Argon2id-hashed credentials, signed session cookies,
> admin-issued password reset tokens; see
> [Authentication](#authentication) — but there is no SSO, no
> MFA, no account lockout, no rate limiting, no edge-tier
> hardening. For production deployments see the **Post-release**
> section in [`TODO.md`](../../TODO.md) — **Production
> infrastructure template**, **Integrated IAM** (Authelia /
> OIDC), and Workflow modeling UX are queued there.

## Install

Needs Docker Engine + the Compose v2 plugin (`docker compose`, not
the legacy `docker-compose`). On a fresh Ubuntu/Debian VM:

```sh
curl -fsSL https://get.docker.com | sudo sh
```

Then:

```sh
git clone https://github.com/algedonic-dev/boss.git
cd boss/infra/oss-quickstart
cp .env.example .env
# edit .env — set BOSS_BOOTSTRAP_ADMIN_EMAIL=you@example.com
./preflight.sh          # readiness check: leftover volume, stale image, busy ports
docker compose up
```

`preflight.sh` is a non-destructive readiness check: it flags a leftover
`boss_postgres-data` volume, a stale cached `boss:latest` image (old code),
or a busy host port 4443/5432 from a previous run — each with the exact
`docker compose down -v` / `--build` command to clear it. This is the step
that catches "I ran it before and now `boss-init` errors with *relation
already exists*."

When `boss-services` logs `all services up`, open
**http://localhost:4443**. Every visitor signs in: use the
bootstrap-admin email you set in `.env` + the default password
`change-me` for a `platform-admin` session (full write access), or
click **Browse as a guest** for a read-only look around. Rotate
the password right after — see [Authentication](#authentication).

Stop the stack with `docker compose down`. Add `-v` to wipe the
Postgres volume — the next `up` re-runs the first-start init steps
and re-seeds.

## What a healthy install logs

`boss-init` runs a four-step chain on every start and prints a
numbered checkpoint per step — these are the lines to grep for
when an install misbehaves:

1. `==> [1/4] converging per-module schema` — `migrate.sh` applies
   whatever the database is missing and summarizes with
   `applied N, already recorded M, of K manifest entries`. A
   migration failure fails the container loudly rather than
   starting services against a half-migrated database.
2. `==> [2/4] seeding the platform Workflow bundle (insert-if-missing)`
   — `boss-platform-workflow-seed` loads the protocol kinds
   shipped as data in `infra/platform/workflows/` and reports
   `platform-workflow-seed: 15 inserted, 0 already present` on a
   fresh database. If this step fails, services still start, but
   tenant prepare will name the first missing kind it hits.
3. `==> [3/4] provisioning bootstrap-admin credential` — writes
   the local-auth credential file and prints
   `✓ Credential set for <your email>`. (First start only.)
4. `==> [4/4] priming sim_clock to 2025-04-01` — primes the
   formula clock and prints
   `✓ formula clock primed to 2025-04-01 @ 1000x warp`. At warp
   1000 the playground advances **~1 sim-day per 86 wall-seconds**.
   The epoch and warp numbers live in [`init.sh`](init.sh) (step
   [4/4], override via `BOSS_DEMO_EPOCH_START`) — if this page and
   `init.sh` ever disagree, `init.sh` is right. (First start only;
   re-priming would drag a running playground's epoch backwards.)

Then ends with `==> boss-init done.` and `boss-services` takes
over:

- `==> boss-launch starting 31 services` — the launcher walks its
  roster. **A binary missing from the image is logged
  `SKIP: <name> (binary not in image)` and the stack continues
  without it** — a SKIP is never fatal, so if a page 502s, check
  the launcher log for a SKIP of the service behind it. (The
  verified 2026-08-22 run started 26 of the 31 listed.)
- `waiting for dispatcher readyz` — the launcher gates the sim on
  the dispatcher's consumer loops being live, so side effects
  (invoices, COGS, shipping) fire from the first sim tick.
- `seed-operator-baseline` then the brewery tenant seed run
  through the public API, and `boss-brewery-sim prepare` builds
  the 411-person roster (`roster ready — opening design Jobs`).
- `==> all services up — pid count: N` — the SPA is live at
  **http://localhost:4443**.

## What you're looking at

The brewery (Algedonic Ales) is the public OSS demo tenant. The
install seeds the reference data — employees, accounts, vendors,
recipes, equipment, the Workflow catalog — then starts the brewery
sim, which ticks sim-days forward at the primed warp (the [4/4]
clock note above) and builds the rest live: orders, work,
invoices, ledger entries, projections. The SPA is sparse on first
load and fills in as the sim runs.

Try:

- `/ux/exec` — the executive dashboard.
- `/system/monitoring` — service health, deployment topology, ML
  oversight.
- `/system/kb` — architecture diagrams, ADRs, hardware/software
  reference.
- `/ux/jobs` — every Job in flight.
- `/system/workflows` — the Workflow catalog + authoring (writes
  need your platform-admin role).

## Developing against the source tree

For working on BOSS itself, `quickstart.sh` builds the workspace
on the host and runs each service as a plain background process —
so you can rebuild a single crate and restart one service instead
of rebuilding the Docker image. This is the dev-mode path; the
compose stack above is the supported install.

### Prerequisites

**System packages first** — on a fresh Ubuntu/Debian VM, the bun
installer needs `unzip` and the Rust build needs a C toolchain.
Install both before the per-tool table below:

```sh
sudo apt-get install -y curl ca-certificates unzip build-essential pkg-config libssl-dev git
```

(macOS users: `unzip` ships in the base system; install Xcode CLT
via `xcode-select --install` for the C toolchain.)

Then four tools running on `localhost`:

| Tool | Version | Install |
|---|---|---|
| Rust | stable | https://rustup.rs/ |
| Bun | 1.1+ | `curl -fsSL https://bun.sh/install \| bash` (requires `unzip`) |
| Postgres | 16+ on `:5432` | `apt install postgresql-16` / `brew install postgresql@16` |
| NATS | any | [download](https://nats.io/download/) a release binary, then run `nats-server -js` (JetStream is required) |

After installing Bun, **open a new shell** (or `source ~/.bashrc`)
so `bun` lands on your `PATH` — the installer modifies your shell
rc but it doesn't take effect in the current process.

The Postgres role `boss` (password `boss`) must exist as a
superuser — the bootstrap scripts create the database but **not**
the role. Create it once before running the quickstart:

```sh
sudo -u postgres psql -c "CREATE ROLE boss WITH LOGIN SUPERUSER PASSWORD 'boss';"
```

### Run it

```sh
git clone https://github.com/algedonic-dev/boss.git
cd boss
./infra/oss-quickstart/quickstart.sh
```

The script will:

1. Check the four prereqs above (bailing with install hints if any
   are missing), then run a non-destructive **preflight readiness
   check** that halts with a clear message if a prior run's BOSS
   services are still up or the gateway port 4443 is taken — pass
   `--skip-preflight` to override.
2. Prompt for your **bootstrap-admin email** — the seed Employee
   record that owns the platform-admin role. (Or pass
   `--email=you@example.com`.)
3. Build the workspace via `infra/bootstrap-local.sh` (~40–60 min
   cold on a 2-vCPU VM, ~10 min on an 8-vCPU dev workstation,
   ~30 s warm), then drop and recreate an empty local `boss`
   Postgres database. The first cold build dominates wall-clock —
   expect **~60–80 min clone-to-SPA total on a 2-vCPU VM**;
   subsequent runs reuse `target/` and finish in seconds.
4. Build the SPA via Bun.
5. Seed your bootstrap-admin's `change-me` credential in
   `/var/lib/boss/auth/credentials.toml`.
6. Start every service as a background process (PIDs in
   `~/.boss-pids`) — including the operator-baseline + brewery
   tenant seed through the public API and the sim that builds the
   demo live, and the gateway on `127.0.0.1:4443`.

When it prints `Quickstart complete.`, open
**http://127.0.0.1:4443** and sign in — bootstrap-admin email +
`change-me`, or the guest button for read-only (see
[Authentication](#authentication)).

Stop it:

```sh
kill $(cat ~/.boss-pids)
```

Re-run it:

```sh
./infra/oss-quickstart/quickstart.sh --email=you@example.com
```

Re-runs auto-detect existing state: if the `boss` database already
has a populated `audit_log` (>1000 rows — i.e. the sim has been
running), the DB bootstrap is skipped and the script just rebuilds
the SPA + restarts services. To start the demo over from an empty
log, drop the DB first:

```sh
sudo -u postgres dropdb boss
./infra/oss-quickstart/quickstart.sh --email=you@example.com
```

The bootstrap-admin email upserts in either path.

## Exposing the stack to a public hostname

Both the compose stack and the dev-mode script land you on
`127.0.0.1:4443`. The gateway is HTTP-only — it does NOT
terminate TLS, validate hostnames, or rewrite the SPA's fetch
origin. For a public deployment:

1. **Run a TLS-terminating reverse proxy in front** — Caddy,
   nginx, an ALB, a Cloudflare Tunnel — pointing at
   `127.0.0.1:4443`.
   The reference Caddyfile at `infra/caddy/Caddyfile` reads
   `BOSS_HOSTNAME` from env and proxies to the gateway:

   ```sh
   sudo BOSS_HOSTNAME=boss.example.com caddy run --config infra/caddy/Caddyfile
   ```

   Caddy fetches a Let's Encrypt cert via HTTP-01 challenge.
   The hostname must resolve directly to the VM (no proxy in
   between), or use `BOSS_HOSTNAME=localhost` for HTTP-only
   local testing.

2. **Set `BOSS_SESSION_KEY` to a strong random value** — see
   the next section. The default (`please-rotate-me-in-prod-do-
   not-leak`) is correctly named.

3. **Rotate the bootstrap-admin password.** See
   *Authentication* below.

The Docker compose stack does not bundle Caddy; bringing it up
is a separate concern outside the container. v1's framing is
"an install that runs on a single VM"; multi-tier production
deploys (HA gateway, separate TLS terminator, dedicated DB)
are tracked under the **Production infrastructure template**
TODO.

## Authentication

Every visitor signs in. Local-auth
(`BOSS_AUTH_PROVIDER=local-auth`) serves `/login`, where the
bootstrap-admin email + password mints a `platform-admin`
session with full write access.

A deployment with `BOSS_GUEST_ACCESS=1` adds a **Browse as a
guest** button to that page. It signs the visitor in as
`guest@algedonic.dev` with the `audit-readonly` role — read
every projection, write nothing. Leave it unset and the button is not offered.

Earlier versions did this without the button: a middleware
minted the `audit-readonly` session for anyone who arrived
without a valid cookie. Convenient until a session expired —
the next request minted a guest session over the expired admin
one and reissued the cookie under the same name, so the SPA
still looked signed in while every write returned 403. A
session now appears only when someone asks for one.

The bootstrap-admin credential is provisioned automatically on
both paths. Default password: `change-me`. The credential lives
in `/var/lib/boss/auth/credentials.toml` (Argon2id hashed).

Rotate it before exposing the stack to anything other than your
laptop:

```sh
# Docker:
docker compose exec boss-services boss-auth set you@example.com

# Source-tree dev mode:
BOSS_AUTH_FILE=/var/lib/boss/auth/credentials.toml \
    target/release/boss-auth set you@example.com
```

`boss-auth` is the admin CLI for the file-backed credential
store:

```sh
boss-auth list                    # list every credentialed email
boss-auth add  alice@example.com  # onboard a new user (prompts for pw)
boss-auth set  alice@example.com  # rotate an existing user's pw
boss-auth remove alice@example.com
boss-auth verify alice@example.com  # exit 0 on match, 1 on miss
```

Set a **strong** `BOSS_SESSION_KEY` (Docker: in `.env`;
dev mode: in your shell env before running `quickstart.sh`)
before deploying anywhere reachable — it's the HMAC key the
gateway uses to sign session cookies. The default value
(`please-rotate-me-in-prod-do-not-leak`) is correctly named.

To withdraw the guest button, unset `BOSS_GUEST_ACCESS`
(Docker: remove the line from
`docker-compose.yml`; dev mode: edit
`infra/bootstrap-local.sh`'s gateway env). A login is then the
only way in.

> ⚠  This is the v1 launch auth — file-backed credentials, no
> account lockout, no email-based password reset, no MFA.
> Production deployments will use Authelia (or any OIDC IDP)
> fronting the gateway via forward-auth headers; tracked under
> the **Integrated IAM** post-release entry in
> [`TODO.md`](../../TODO.md).

## Troubleshooting

**`relation "..." already exists` / `boss-init exited with code 3`.**
A previous (often failed) install left an already-initialized Postgres
volume, or a cached `boss:latest` image is running older code against it.
Run `./infra/oss-quickstart/preflight.sh` to see what's lingering, then
clear it for a clean slate:

```sh
docker compose -f infra/oss-quickstart/docker-compose.yml down -v   # wipe the volume
docker compose -f infra/oss-quickstart/docker-compose.yml up --build # rebuild from current source
```

**A page 502s in a fresh install.** Check the `boss-services` log
for `SKIP: <name> (binary not in image)` — the launcher skips
missing binaries and keeps going, so a stale or partial image
surfaces as a missing service rather than a failed start.

**`pg_isready` fails** (dev mode). Postgres isn't listening on
`127.0.0.1:5432`. Start it: `brew services start postgresql@16`
or `sudo systemctl start postgresql`.

**`could not connect to server: Connection refused` on NATS**
(dev mode). Start `nats-server` on port 4222: `nats-server -js`.

**`error: linker 'cc' not found`** (dev mode). Install
`build-essential` (Linux) or `xcode-select --install` (macOS).

**Build takes much longer than expected** (dev mode). The first
cargo build does cold compile of ~150 crates (49 boss-* + their
transitive deps). On a 2-vCPU VM this is 40-50 minutes; on an
8-vCPU dev workstation closer to 10. Subsequent runs reuse
`target/` and finish in seconds. If you're evaluating on cloud
VMs, a 4+ vCPU instance halves the wait.

## Validating the brewery sim (maintainers)

The demo builds itself live, so there's nothing to fetch or load. To
check that a year of sim still reconstructs and reconciles cleanly (the
correctness gate maintainers run before a release):

```sh
sudo ./infra/postgres/validate-brewery-sim.sh
```

It drops the `boss` DB, prepares the brewery tenant
(`boss-brewery-sim prepare`), runs `boss-brewery-sim run` for 365
sim-days from 2025-04-01 with hard-fail (any non-2xx aborts), then
asserts every projection rebuilds from `audit_log`
alone (`failures=0`) and passes the conservation + dangling-FK integrity
checks. ~30 minutes on a 4-core box; watch the per-step echo to follow
along.

The source-of-truth inputs are the brewery seed files
(`examples/brewery/seeds/{workflows,tenant,accounts,vendors,parts,products,classes}.toml`)
plus the sim engine (`crates/tenants/boss-brewery-engine`), which ticks
one sim-year against the live API.

Short cycles for iteration:

```sh
# 14 sim-days — completes in ~5 min
sudo BOSS_REGEN_DAYS=14 ./infra/postgres/validate-brewery-sim.sh

# custom start date to exercise a specific cadence ramp
sudo BOSS_REGEN_DAYS=30 BOSS_REGEN_START=2025-07-01 \
    ./infra/postgres/validate-brewery-sim.sh
```

`--hard-fail` surfaces the failing request on stderr — common roots: a
Workflow step referencing a SKU/employee/account the tenant seed didn't
create, a side-effect handler error (empty line_items, inventory
underflow, FK violation), or service-bootstrap timing on a slow box.

To reset a running demo back to "seeded day 0" without a full regen, use
`infra/postgres/reset-to-baseline.sh` (host-level, drop + reseed) or the
in-app **Reset** button (trims the audit_log back to the seeded
baseline).

## What's next

Once you've kicked the tires:

- Read [`README.md`](../../README.md) for the platform thesis +
  architecture frame.
- Read [`docs/architecture-decisions.md`](../../docs/architecture-decisions.md)
  for every load-bearing design decision.
- Read [`examples/brewery/DOMAIN.md`](../../examples/brewery/DOMAIN.md)
  for how the brewery models its operations on BOSS primitives.
- Open issues / PRs at https://github.com/algedonic-dev/boss.
