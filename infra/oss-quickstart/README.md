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
click **Browse as a guest** for a read-only look at what this
install lets a stranger read — a basic `visitor` by default (see
[Authentication](#authentication)). Rotate
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

There is one way to run BOSS: the container image. Working on BOSS
itself means building that image from your tree and running it under
the same compose file — `docker compose up --build` rebuilds
`boss:latest` from `infra/oss-quickstart/Dockerfile` (the Rust build
layer is cached; a one-crate change rebuilds in minutes, a cold build
is ~40–60 min on a 2-vCPU VM). The host-native path that built the
workspace and ran each service as a background process
(`quickstart.sh` + `bootstrap-local.sh`) was deleted on 2026-09-18
(design 42277636): it had drifted from the image three ways and was
starting a service the tree no longer had.

For the Rust and web toolchains, tests and lints — everything that
runs before a change reaches the image — see
[`docs/runbooks/dev-environment-bootstrap.md`](../../docs/runbooks/dev-environment-bootstrap.md).
BOSS's own development runs on a cluster dev pod
([`docs/design/dev-cluster.md`](../../docs/design/dev-cluster.md)).

Re-run against a clean demo: the sim builds the demo live from an
empty `audit_log`, so starting over is dropping the volume:

```sh
docker compose down -v          # drops postgres-data and boss-auth
docker compose up --build
```

The bootstrap-admin email in `.env` upserts on every start.

## Exposing the stack to a public hostname

The compose stack lands you on
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

`BOSS_GUEST_ACCESS` decides whether that page also offers a
**Browse as a guest** button, and what the guest may read. It
takes one of three answers, and the quickstart ships `basic`:

| value | what a stranger who clicks the button gets |
|---|---|
| `basic` (the quickstart's) | a session as `guest@algedonic.dev` with the `visitor` role. It reads only what this install's policy grants `visitor` (rules naming that role in the tenant's `policy_rules.toml`), and nothing else: a page whose data the role cannot read shows empty or refused, not an error. |
| `audit` | the `audit-readonly` role: Read on every shipped resource, the employee roster and the books included. Right for a public demo whose company is synthetic; wrong for your own records. |
| `0`, or unset | no button. A login is the only way in. |

A guest writes nothing under either role — every write is refused
at the gateway before it reaches a service. `1`, the value earlier
versions documented, still means `basic`; any other value is
refused by name in the gateway's log, and no guest is offered.

Earlier versions did this without the button: a middleware
minted the `audit-readonly` session for anyone who arrived
without a valid cookie. Convenient until a session expired —
the next request minted a guest session over the expired admin
one and reissued the cookie under the same name, so the SPA
still looked signed in while every write returned 403. A
session now appears only when someone asks for one.

The bootstrap-admin credential is provisioned automatically at
first start. Default password: `change-me`. The credential lives
in `/var/lib/boss/auth/credentials.toml` (Argon2id hashed).

Rotate it before exposing the stack to anything other than your
laptop:

```sh
docker compose exec boss-services boss-auth set you@example.com
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

Set a **strong** `BOSS_SESSION_KEY` (in `.env`)
before deploying anywhere reachable — it's the HMAC key the
gateway uses to sign session cookies. The default value
(`please-rotate-me-in-prod-do-not-leak`) is correctly named.

To withdraw the guest button, set `BOSS_GUEST_ACCESS: "0"` in
`docker-compose.yml` (or remove the line). A login is then the
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

**`docker compose up --build` takes much longer than expected.** The
first image build cold-compiles ~150 crates (49 boss-* + their
transitive deps). On a 2-vCPU VM this is 40-50 minutes; on an
8-vCPU dev workstation closer to 10. Later builds reuse the cached
Rust layer and finish in minutes. If you're evaluating on cloud
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
