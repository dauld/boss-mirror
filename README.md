# BOSS

[![CI](https://github.com/algedonic-dev/boss/actions/workflows/ci.yml/badge.svg)](https://github.com/algedonic-dev/boss/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/algedonic-dev/boss/badge)](https://scorecard.dev/viewer/?uri=github.com/algedonic-dev/boss)

> ⚠ **Preliminary release.** An early, opinionated cut shared to
> get reactions to the idea — direct-modeling, the four primitives
> (Subjects, Jobs, Steps, Events), the human-plus-agents executor
> model. A prototype that proves the shape, not a stable product:
> APIs, schemas, and registry contracts will move before the first
> non-preliminary tag. Expect rough edges — half-built surfaces,
> wired-but-unsurfaced code, intentional `TODO`s
> ([`TODO.md`](TODO.md) is the roadmap). Resonates or doesn't?
> Open an issue.
>
> **Agent-authored.** Every line of syntax — Rust, TypeScript,
> Svelte, SQL, prose — is written by coding agents under a human
> designer's direction (who has read the whole tree); expect the
> odd quirk.
>
> **The audit log is load-bearing, not the code.** It's the
> event-sourced system of record, third-party-verifiable without
> running any BOSS software (the [five-property correctness
> protocol](docs/design/correctness-protocol.md)).

**BOSS exists to help the people running a business operate it
better — and to make the basic software that running a business
needs reachable for organizations that can't afford a bespoke
enterprise stack.** Beer Open Source Software for System
Modeling, named in tribute to **Stafford Beer**, the British
cybernetician whose work on the Viable System Model and Project
Cybersyn shaped how I thought about modeling organizations while
designing BOSS. Event-sourced and network-shaped: **the network is
the substrate, the fat protocols dictate the current operating
model, the actors run it** — built around describing real-world
organizations directly.

**The thesis: a reasonably-sized business — a brewery, a
device-refurb shop, a dental practice, a 200-tech field-service
operation — can be modeled directly in software, in its own
terms, with a handful of primitives.**

Most businesses don't run on a model of themselves. They run on
a patchwork — a CRM that knows accounts, a ticketing tool that
knows work, an accounting package that knows money, none of which
knows the others. The real shape of the operation — who owns
what, what must happen before what, what state each thing is in —
lives in the seams: in spreadsheets, in people's heads, in
reconciliation nobody has time for. Each tool holds a partial,
approximate shadow of the business, and keeping the shadows in
sync *is* the overhead.

BOSS collapses the patchwork into one model. A **Subject** (an
account, an asset, an employee), a **Job** (a sale, a service
visit, a hiring pipeline), a **Step** (a typed transition gated
by ownership and sign-off), and the **Event** log over all of it
are one fabric — so an account, the job running against it, and
the GL entry that job posted cross-reference in a single record.
Modeling directly, instead of bending the business into generic
tools, buys three things the patchwork can't:

- **Fidelity — the model is the business, not an approximation
  of it.** A Step is a real person signing off; a Job is a real
  unit of work. The audit log records what the company did in the
  company's own vocabulary, not an analytics derivative
  reconstructed afterward. Operators reason about the software
  the way they reason about the operation, because they're the
  same shape.
- **Adaptability — the model bends as the business does, in
  data, not code.** New work types are Workflow rows; new
  categories — roles, account tiers, asset models — are registry
  entries; new step UX is a plugin. Reshaping the model is
  editing data, not forking a codebase — which is what keeps a
  small team able to follow the operation as it changes.
- **Coherence — one model where a generic stack keeps three.**
  Sales, work, assets, people, and the ledger share Subjects and
  one event log, so the cross-system reconciliation that eats
  operational time mostly stops existing.

Underneath, the model is event-sourced and third-party-verifiable
(above) — replay rebuilds every projection from t=0 — so
durability, auditability, and a one-policy-gate security posture
come from the foundation, not from operator discipline. It reads
in three layers, each replaceable without disturbing the others:
the **network** is the substrate — packets, the queues that hold
them, routes, the log, one admission edge — and has no opinion
about what the work means; the **protocols** are fat and carry the
meaning, which is why changing how the company works is publishing
a versioned registry row rather than shipping code; and the
**actors** — humans plus agents — are the CPUs that run it,
executing Steps inside the same schema, policy gate, and sign-off
rules, never in the request path.

The codebase is structured to be conducive to rapid customization
with AI coding agents — with a bit of care to preserve the audit-
log + correctness-protocol contracts at the bottom, the rest is
malleable.

## See it locally

Install it locally and open it in a browser: the demo tenant,
Algedonic Ales, **builds itself live**. A fresh install starts
with an empty audit log (schema + reference data only); the
brewery simulator then ticks sim-days forward from the demo
epoch and generates the operation as it
goes — jobs, orders, invoices, ledger entries, projections. So
the SPA is **sparse on first load and fills in as the sim runs**,
and the audit log grows while you click around. The install (see
[Quick start](#quick-start) below) builds from source and seeds
the tenant through the public API; the bootstrap-admin email you
set in `.env` becomes your login.

A few specific places that land the design quickly (all served
by the local install on `:4443`):

| If you want to see … | Open |
|---|---|
| The home dashboard with live system activity | <http://localhost:4443/> |
| A Job from the brewery flow with its step graph | <http://localhost:4443/ux/jobs/> (click any open Job) |
| The full event log streaming as the sim ticks | <http://localhost:4443/system/monitoring/events> |
| The System Atlas — every service + its event topics | <http://localhost:4443/system/monitoring/atlas> |
| The brewery's people, with role-based scoped views | <http://localhost:4443/ux/people> |
| A workflow's anatomy (Workflow authoring surface) | <http://localhost:4443/system/workflows> |

The install lands on `:4443` behind local-auth: log in as the
bootstrap-admin for write access, or use the **Browse as a
guest** button for read-only. What you see is the head of `main`
plus the data the simulator has generated since you started the
stack.

## Where to start

| If you want to … | Read |
|---|---|
| See it in a browser | [Quick start](#quick-start) below — `docker compose up` |
| Understand the architecture | [docs/architecture-diagram.md](docs/architecture-diagram.md) — four diagrams, conceptual to concrete |
| Read the BOSS core domain | [CLAUDE.md §Primitives + §Supporting concepts](CLAUDE.md#primitives) |
| See the public demo tenant | [examples/brewery/DOMAIN.md](examples/brewery/DOMAIN.md) — Algedonic Ales |
| See the second worked tenant | [examples/used-device-shop/DOMAIN.md](examples/used-device-shop/DOMAIN.md) |
| Read the design pattern docs | [docs/design/](docs/design/) — load-bearing patterns referenced from CLAUDE.md and the decision record |
| Run BOSS locally | [Quick start](#quick-start) below |
| Bring up a fresh dev VM | [docs/runbooks/dev-environment-bootstrap.md](docs/runbooks/dev-environment-bootstrap.md) |
| Operate a deployed BOSS | [docs/runbooks/operator.md](docs/runbooks/operator.md), [`boss` CLI](crates/orchestrators/boss-cli/README.md) |
| Track open work | [TODO.md](TODO.md) |
| Read the decision record | [docs/architecture-decisions.md](docs/architecture-decisions.md) |

## What's in the box

The workspace splits into four tiers (see CLAUDE.md):

- **Core state-machine OS** — `crates/core/`. The generic primitives every deployment uses:
  Subjects, Jobs, Steps, the audit log + projection rebuilders,
  the Workflow / StepType / StepPlugin registries, policy,
  gateway/auth/NATS, calendar, and the two taxonomy registries.
  Tenant-neutral.
- **Company-modeling layer** — `crates/modules/`. Useful for modeling a company on top of the core:
  people, accounts, commerce, inventory, ledger, products,
  shipping, messages, catalog, assets, plus their `*-client`
  HTTP-contract crates and the `boss-ml-plugins` extension
  surface. A non-company tenant (a research lab, a robot fleet)
  can deploy without these.
- **Orchestrators** — `crates/orchestrators/`. Binaries that fan out across both tiers:
  `boss-rebuild`, `boss-cli`, `boss-sim`, `boss-ml-api`, `boss-simulator`.
- **Tenants** — `crates/tenants/`.
  - **Algedonic Ales** (`boss-brewery-engine`) — the public OSS
    demo tenant. Data-first seeds at `examples/brewery/` (TOML +
    JSON) plus the brewery-specific Workflows. Industrial-scale
    brewer modeled across 5 beer styles.
  - **Used-device-shop** (`boss-used-device-shop-engine`) — sells,
    services, and resells used physical devices needing
    sophisticated diagnostics + repair. Data lives at
    `examples/used-device-shop/`; install it on a fresh VM with
    `sudo TENANT=device-shop infra/bootstrap-vm.sh` (same service
    stack as the brewery, no sim daemon — after
    `boss-used-device-shop-engine prepare` seeds the model, work is
    driven by human and agent actors).

A tenant is seed data — Workflows, policy grants, Class rows, a
roster — published through shared bootstrap doors
(`boss_jobs::bootstrap`, `boss_policy::bootstrap`, the classes batch
API). The only tenant-specific Rust a third tenant needs is a thin
`prepare` shell naming its seed files (see
`crates/tenants/boss-used-device-shop-engine/src/prepare.rs`); no new
core code, no new web code.

The full crate-by-crate map and service topology live in
[docs/architecture-diagram.md](docs/architecture-diagram.md).
Port assignments are the canonical
[`crates/core/boss-ports/src/lib.rs`](crates/core/boss-ports/src/lib.rs).

## Quick start

One supported install path — Docker compose:

```sh
git clone https://github.com/algedonic-dev/boss.git
cd boss/infra/oss-quickstart
cp .env.example .env
# edit .env — set BOSS_BOOTSTRAP_ADMIN_EMAIL=you@example.com
docker compose up
```

Open `http://localhost:4443` and log in with the bootstrap-admin
email you set. The install seeds the brewery tenant through the
public API and starts the live sim, which builds the demo from an
empty log. Expected timings, the init-chain log checkpoints, the
guest button, troubleshooting, and the host-native source-tree
dev path (`quickstart.sh`) all live in the one install runbook:
[`infra/oss-quickstart/README.md`](infra/oss-quickstart/README.md).

> **The demo builds itself live.** The install starts the brewery sim
> against an empty `audit_log`, so the SPA is sparse on first load and
> fills in as the sim runs — which keeps the clone small and the install
> fast. Maintainers validate a full sim-year with
> `sudo ./infra/postgres/validate-brewery-sim.sh`: it runs the sim and
> asserts a clean rebuild + conservation/integrity.

> ⚠  **Not production-ready.** The quickstart runs the whole
> platform on one machine behind file-backed local-auth — no
> SSO, no MFA, no account lockout, no rate limiting, no
> edge-tier hardening (see the quickstart README's
> Authentication section). An integrated IAM (Authelia or any
> OIDC IDP via forward-auth) and a hardened
> production-infrastructure template are queued under
> "Post-release" in [`TODO.md`](TODO.md).

## Production posture

For deployments past the eval stage, v1's recipe is **single VM
with a backup strategy**. One host runs the full stack —
Postgres, NATS, every `boss-*-api`, the gateway, the static SPA.
`infra/backup.sh` ships pg_dump snapshots off-box on a systemd
timer; the `audit_log` is the disaster-recovery primitive (any
snapshot replays cleanly via `boss-rebuild-all`).

The systemd-managed deploy lives at
[`infra/deploy-services.sh`](infra/deploy-services.sh); the
operator runbook is at
[`docs/runbooks/operator.md`](docs/runbooks/operator.md).
Cloud-provider provisioning recipes (Azure, GCP, AWS,
Cloudflare, Hetzner) are queued post-release under
`infra/blueprints/<provider>/` per the TODO entry.

Multi-VM topologies (warm-standby Postgres replication,
active/active data planes, edge load balancers) are deliberately
out of scope for v1 — they get reconsidered when a real tenant's
SLAs force the conversation. The post-release **Production
Infrastructure Template** TODO entry is where that work lands.

## Building & quality

Test discipline matters because the platform claims correctness as
a first-class invariant (see *Founding ideas* above). The full
strategy — what each layer catches, where it runs, what fails the
build vs what triggers an incident — lives at
[docs/design/testing-strategy.md](docs/design/testing-strategy.md).
The summary:

| Layer | Mechanism | Where it runs |
|---|---|---|
| Static checks | `cargo clippy --workspace --all-features --tests -- -D warnings`, `cargo fmt --check`, `bun run typecheck` (svelte-check, strict TS) | CI on every push + PR |
| Unit + integration tests | ~1,640 Rust `#[test]` cases across the workspace + Svelte component tests; `cargo test --all-features` | CI + local |
| Lints beyond the type system | `infra/lint/seed-bypass-smell.sh` rejects seed scripts that bypass the Workflow path; `cargo clippy` lint set is the strict superset | CI |
| Audit-log integrity | `boss-audit-integrity-check` verifies the per-row hash chain on `audit_log` and the `REVOKE UPDATE, DELETE, TRUNCATE` schema-level append-only enforcement | systemd timer (daily) in prod |
| Conservation invariants | `infra/lint/conservation-invariants.sh` proves the five-property correctness protocol — provenance, conservation, closure, idempotence, determinism — across every projection vs. the `audit_log` it derives from | systemd timer in prod, on-demand locally |
| Replay rebuild | `boss-rebuild-all` reconstructs every projection from `audit_log` alone and `infra/verify-replay.sh` diffs the result against live state | on-demand; in CI via `validate-brewery-sim.sh` (runs a sim-year, then asserts a clean rebuild) |
| Service + binary drift | `infra/check-service-drift.sh` validates the systemd unit set, `check-service-write-roundtrip.sh` exercises every write endpoint against real Postgres, and `check-binary-build-coverage.sh` catches `*-api` binaries that would silently boot in-memory because their `postgres` feature isn't activated | on-demand + cron |

The "always green" claim isn't aspirational. Audit-log integrity
runs every 24 h; conservation invariants run on a timer; replay
rebuild is a CI step on every push that touches a projection. A
red signal anywhere is an incident, not a flaky test to retry.

```sh
# Workspace build (15–20 min cold, ~30 s warm).
cargo build --release --workspace

# Full test sweep — ~1,640 Rust tests + the bun web suite (~3 min cold).
cargo test --all-features
( cd apps/web && bun run typecheck && bun test )

# Run the in-tree integrity checks against a local Postgres.
infra/lint/conservation-invariants.sh
infra/verify-replay.sh
```

CI configuration: [`.github/workflows/ci.yml`](.github/workflows/ci.yml).
The `release.yml` workflow cuts versioned `boss` CLI binaries; the
prod-deploy pipeline (cross-compile + scp + systemctl restart) is
queued — see [TODO.md](TODO.md).

## Founding ideas

Three intellectual lineages sit behind the design: **Stafford
Beer** (cybernetics — companies as viable systems shaped by
feedback loops and *algedonic* signals), **Rich Hickey**
("information is simple" — data is primary, mutation pays
forever, event-sourced + immutable values everywhere), and
**George Orwell** (*Politics and the English Language* —
communication decays when words drift from reality, so the audit
log + the five-property correctness protocol keep the language
operators use anchored to the facts).

All three converge at the same claim from different angles: the
company *is* its event log + its current state + the rules
connecting them. Full elaboration in
[CLAUDE.md](CLAUDE.md#founding-ideas--what-every-load-bearing-decision-is-measured-against).

## License + acknowledgements

Apache-2.0. Named in tribute to Stafford Beer. The brewery demo
data — every employee, account, vendor, invoice, and recipe in
the Algedonic Ales seed — was fabricated by Claude (Anthropic's
LLM) as part of building this demo. Names and figures are
LLM-generated; any coincidence with a real person, business, or
document is unintentional.
