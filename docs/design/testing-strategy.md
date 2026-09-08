# Design: BOSS Testing Strategy

**Status**: living guidance — describes how the codebase actually
verifies itself plus the principles that govern where new tests
land.

## Frame

The correctness protocol (`docs/design/correctness-protocol.md`)
states the *invariants* — every Workflow, projection, and adapter
must satisfy provenance, conservation, closure, idempotence,
determinism. This document says how those invariants get
**enforced** through the testing + lint pipeline.

Two principles shape the structure:

1. **Layers, not exhaustion.** Each layer catches a specific
   failure shape. We don't try to catch every bug at every layer
   — we let the cheapest layer that can see a class catch it,
   and we fail loud when something slips past.
2. **Defense-in-depth, not single-point-of-failure.** If the same
   bug class bites twice (the silent-skip / postgres-feature class
   is the canonical example), we add a *new layer* that catches it
   earlier. We don't tighten an existing layer until the structural
   cause is understood.

## The layers

Ordered cheapest-to-catch first:

### Layer 1 — Compile-time (type system + clippy)

**What it catches:** type errors, unused code, lint regressions,
formatting drift, syntax-level mistakes.

**Mechanism:**
- `cargo build --workspace` — every member compiles.
- `cargo clippy --workspace --all-features --tests -- -D warnings`
  — workspace clippy gate, denied warnings, includes test code.
  Workspace lint floor in `Cargo.toml` (`[workspace.lints]`)
  pins clippy::correctness + clippy::suspicious to deny so any
  new code must clear them.
- `cargo fmt -- --check` — formatting must match rustfmt.
- `bun run typecheck` (svelte-check, strict TS) — frontend.

**Where it runs:** every PR push (CI: `.github/workflows/ci.yml`)
+ pre-commit locally.

**Failure mode:** PR can't merge.

### Layer 2 — Static lints beyond the type system

**What it catches:** structural mistakes the type system can't see
— seed scripts that bypass the Workflow path, Tier-1 crates that
import from Tier-2, *-api binaries that would silently boot
in-memory, leaked credentials in release artifacts.

**Mechanism (each script lives under `infra/lint/` or `infra/`):**

| Script | Catches |
|---|---|
| `infra/lint/seed-bypass-smell.sh` | Seed SQL that INSERTs into projection tables instead of routing through a Workflow. |
| `infra/lint/tier-import-audit.sh` | Tier-1 library crates depending on Tier-2. 0 violations across 27 core crates today. |
| `infra/check-binary-build-coverage.sh` | `*-api` binaries that would silently produce in-memory builds because their `postgres` (or `s3`) feature isn't activated by default, required-features, or workspace dep unification. Catches both the silent-skip class and the startup-guard-trap class. |

**Where it runs:** CI on every push + on-demand locally.

**Failure mode:** PR can't merge.

### Layer 3 — Unit tests

**What it catches:** per-function correctness, edge cases, type-
boundary serialization round-trips.

**Mechanism:** `#[test]` (~1,070 today) and `#[tokio::test]`
co-located with source code under `crates/*/src/**/*.rs`. No
DB, no NATS — pure in-memory work via the in-memory adapter
implementations of each port trait.

**Where it runs:** `cargo test --all-features` in CI on every
push + locally during development.

**Failure mode:** PR can't merge.

### Layer 4 — Integration tests (per-crate, with real Postgres)

**What it catches:** adapter contracts (PgFoo vs InMemoryFoo
divergence), schema migrations that compile but break runtime
queries, projection-rebuild round-trips, side-effect handler
chains.

**Mechanism:** `tests/*.rs` files under each crate. Use
`boss-testing` to spin up a per-test schema in the local
Postgres and run the real `Pg*` adapter implementations.
`boss-dispatcher` additionally pins the step-side-effect chain
(`step.<status>.<kind>` topic → rule → handler → emitted event):
`tests/delegate_subjob_smoke.rs` drives the real
`Registry::from_toml` → `match_event` → `dispatch` path with the
rule bodies copied verbatim from `infra/dispatcher/rules.toml`,
so the test drifts if the production arg expressions drift.

**Where it runs:** CI (with the `postgres:16` service container)
+ locally.

**Failure mode:** PR can't merge.

### Layer 5 — Replay-rebuild verification

**What it catches:** the fundamental correctness claim — every
projection reconstructs cleanly from `audit_log` alone.

**Mechanism:** `boss-rebuild-all` walks the audit log and
re-derives every projection table; `infra/verify-replay.sh`
diffs the rebuilt state against live state. The brewery 365-day
regen (`infra/postgres/validate-brewery-sim.sh`) drives a
year of sim-events through the live API services then runs the
rebuilders — the assertion is "0 failures across all 15
rebuilders `boss-rebuild-all` runs, 0 net drift between live and
replayed projections."

**Where it runs:** locally on demand; the release leans on it as a
correctness gate before a cut.

**Failure mode:** the script hard-fails on the first divergence; a
rebuild mismatch is a rollback trigger, not a flaky-test retry.

### Layer 6 — Continuous integrity in production

**What it catches:** drift accumulated against the live audit
log over time — chain-hash anomalies, dangling cross-event
references, projection state that disagrees with its derivation
rules.

**Mechanism:** systemd timers run quietly:

| Timer | Cadence | What it does |
|---|---|---|
| `boss-audit-integrity-check.timer` | Daily | Walks `audit_log`, verifies the per-row hash chain, checks `created_at` monotonicity, flags dangling cross-event refs. Schema-level append-only enforcement (`REVOKE INSERT/UPDATE/DELETE`) is the structural complement. |
| `boss-conservation-invariants.timer` (`infra/lint/`) | Hourly | Re-asserts the five-property correctness protocol across every projection vs `audit_log`. |
| `check-service-drift.sh` | Per-deploy | Asserts every declared systemd unit is enabled + active. |
| `check-service-write-roundtrip.sh` | Per-deploy | POSTs a sentinel row to every write endpoint, queries Postgres directly, deletes — catches in-memory-fallback class of bug. |

**Where it runs:** systemd timers in production, on-demand
locally via the same scripts.

**Failure mode:** an integrity check anomaly is an **incident**,
not a flake to retry. The bulletin board surfaces it; the
operator triages it.

### Layer 7 — Formal proofs (selective)

**What it catches:** state-machine invariants whose violations
aren't observable through example-based testing — narrow window
races, ordering hazards, refinement of "no-resurrection" rules.

**Mechanism today:** two TLA+ specs under `docs/formal/` —
`LedgerPeriodLock.tla` proves the period-lock state machine and
`StepStatus.tla` proves the Step lifecycle state machine — though it is
currently **stale**: it still encodes the retired six-state lifecycle
(`Blocked`/`Aborted` dropped), while the shipped machine is the
five-state `Pending → Ready → Active → Completed` (+ `Skipped`). See the
banner atop that file. `cargo-kani`
proofs in `crates/modules/boss-ledger/src/kani_proofs.rs`
exhaustively explore the GL posting paths within a bounded model.

**Where it runs:** locally on demand (`infra/tla/run-tlc.sh`,
`cargo kani`). Not in CI today (TLC + kani both heavy).

**Failure mode:** a formal-spec violation requires a design
change, not a code patch. They're filed as ADRs-or-design-doc
revisions before the code changes.

## What goes where — decision rules

When a new feature lands, ask the layer questions in order. The
answer "yes" stops the search:

1. **Is this a structural rule about the codebase?** (e.g. "no
   crate in tier X imports from tier Y") → Layer 2 lint.
2. **Is this a per-function correctness claim with no external
   dependencies?** → Layer 3 unit test.
3. **Does it touch the database, an HTTP route, or a side-effect
   handler chain?** → Layer 4 integration test.
4. **Is this an invariant on the audit log itself or a projection
   it derives?** → Layer 5 (replay-rebuild) plus Layer 6
   (continuous integrity in prod).
5. **Is this an ordering/concurrency claim that example tests
   can't see?** → Layer 7 formal spec.

## Anti-patterns

- **Mock the database in tests where the schema is the contract.**
  Adapters get tested against real Postgres. A passing mocked
  test that fails against `PgInventory` has bought zero
  confidence.
- **Add a flaky-test retry.** If a test is flaky, it's revealing
  non-determinism the system promised it didn't have. Find the
  source.
- **Skip a layer because "Layer N caught a similar bug last week."**
  Each layer covers a different failure shape. A postgres-feature
  crashloop slips past Layer 6's drift check until restart day;
  Layer 2 catches it at build time via
  `check-binary-build-coverage.sh`. Different layers, different
  catch points.
- **Wait to write the test until after the fix.** Per CLAUDE.md
  TDD discipline: write the failing test that reproduces the
  bug *first*, then the minimal code that makes it pass.

## What CI actually runs today

`.github/workflows/ci.yml`:

```yaml
- Apply schema (infra/postgres/migrate.sh — the schema/ manifest as an ordered migration list)
- Clippy: cargo clippy --workspace --all-features --tests -- -D warnings
- Test:   cargo test --all-features
- Format: cargo fmt -- --check
- Lint:   infra/lint/seed-bypass-smell.sh
- Lint:   infra/lint/no-todo-citation.sh
- Lint:   infra/lint/no-step-kind-match.sh
- Lint:   infra/lint/api-path-bypass-smell.sh
- Lint:   infra/lint/sim-boundary-audit.sh
- Lint:   infra/lint/no-wallclock.sh
- Web:    bun install + bun run typecheck + bun run build + bun run test:mocked
```

`.github/workflows/release.yml` cuts cross-platform `boss` CLI
binaries on tag push.

Drift / build-coverage / replay-rebuild / integrity-check
timers run continuously in production but are NOT yet wired into
CI on every push (replay-rebuild specifically has a wall-time
cost that doesn't fit in a per-PR job; queued as a nightly run).

## Communication

This doc is linked from:
- `README.md` — "Building & quality" section (operator-facing).
- `CLAUDE.md` — under the testing rules (contributor-facing).

When a new layer or check lands, update both the table above
*and* the README's "Building & quality" table in the same commit.
The two should never disagree.
