# Contributing to BOSS

Thanks for considering a contribution. This document captures the
load-bearing rules; for the architectural rationale start with
[`docs/architecture-decisions.md`](docs/architecture-decisions.md)
(the consolidated decision record — one thematic walk through every
load-bearing choice, written as current truth) and
[`CLAUDE.md`](CLAUDE.md) (the operating philosophy). Design docs
under [`docs/design/`](docs/design/) are living references plus
build-time prose for in-flight work; each release, settled material
folds into the decision record and the in-flight doc is deleted.

## Code of Conduct

Participation in this project is governed by
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md). By contributing, you
agree to uphold it.

## License

All contributions are licensed under the project's [Apache
License 2.0](LICENSE). By submitting a pull request, you certify
that:

- You have the right to submit the work under that license.
- You agree your contribution may be redistributed under those
  terms.

A separate CLA is not required.

## How to contribute

The contribution path depends on what you want to do.

### Filing an issue

- **Bugs:** include the version (`git rev-parse HEAD`), the
  command or HTTP request that triggered the failure, the actual
  vs. expected behavior, and any relevant logs. Reproductions
  using the brewery playground tenant are strongly preferred.
- **Feature ideas:** describe the use case before the
  implementation. BOSS is opinionated about the difference between
  *new work shipping as data* (registry rows, Workflows, plugins)
  and *new work requiring core code*; framing your ask in those
  terms gets you to a useful conversation faster. See
  [`docs/design/extending-boss.md`](docs/design/extending-boss.md).
- **Security issues:** see [`SECURITY.md`](SECURITY.md). Don't
  open a public issue.

### Sending a pull request

1. **Fork + branch.** `feat/<short>` for features, `fix/<short>`
   for bugs, `docs/<short>` for documentation. `main` is always
   deployable.
2. **Small PRs.** If a change is hard to review, it's too big.
   Split it.
3. **Tests first.** BOSS is TDD-by-default — write a failing
   test before the production code that makes it pass. See
   [`CLAUDE.md`](CLAUDE.md) § Testing.
4. **Both stacks.** Rust changes need `cargo fmt` + `cargo
   clippy -- -D warnings` clean and `cargo test -p <crate>`
   green. Frontend changes need `bun run gate` in `apps/web`
   clean (svelte-check, unit tests, the build and the mocked
   Playwright suite — the same checks the gate runs).
5. **Commit messages.** Imperative mood, short summary on the
   first line, body explaining the *why*. Follow the conventional
   prefixes already in `git log` (`feat(<area>):`, `fix(<area>):`,
   `docs(<area>):`, `test(<area>):`, `refactor(<area>):`).
6. **No silent scope creep.** Refactors don't ride along with
   feature commits; bug fixes don't ride along with refactors.
   Open a separate PR.

## Architecture rules of the road

The project has strong opinions worth knowing before a non-trivial
change:

- **Hexagonal.** Domain logic never imports infrastructure.
  Adapters implement domain traits; infrastructure swaps don't
  touch the domain.
- **Event-sourced projections.** Every state change emits an
  event into `audit_log` first; projection tables are *derived*
  from the log. Don't bypass the writer with a direct
  `INSERT`/`UPDATE` on a projection table.
- **Registries over hardcoded paths.** New work types, step UX,
  and posting rules ship as **rows in append-only registries**,
  not new branches in core code. If you find yourself adding a
  `match kind { … }` in core, there's a registry you should be
  using.
- **Seeds set initial conditions only.** Never seed downstream
  artifacts (invoices, journal entries, shipments) directly. If
  you're tempted, the Workflow is the gap. See
  [`docs/design/seed-vs-emergent-state.md`](docs/design/seed-vs-emergent-state.md).
- **Five-property correctness protocol.** Provenance,
  conservation, closure, idempotence, determinism. Every
  Workflow / projection / adapter must satisfy all five. See
  [`docs/design/correctness-protocol.md`](docs/design/correctness-protocol.md).

## What we don't accept

- New bespoke workflow code paths. New work ships as
  Workflow/StepPlugin rows, not new `match` branches.
- Tenant-specific assumptions in BOSS core. The brewery and
  used-device-shop crates are example tenants, not core.
- Code without tests. PRs that include "tests deferred" comments
  get closed.
- Dependencies added without justification — every new crate
  multiplies the supply-chain surface.

## Getting set up

The one install runbook is
[`infra/oss-quickstart/README.md`](infra/oss-quickstart/README.md)
— prereqs, expected timings, the init-chain log checkpoints, and
troubleshooting all live there. Two ways in:

**Docker compose** — the supported install; no local toolchain
needed.

```sh
cd infra/oss-quickstart && docker compose up
# open http://localhost:4443
```

**Working on the code** — the same image, rebuilt from your tree:

```sh
cd infra/oss-quickstart && docker compose up --build
```

There is no host-native service install (the bare-metal path was
deleted on 2026-09-18; design 42277636). Toolchains, the pre-push
hook, and a Postgres for the DB-backed tests are in
[docs/runbooks/dev-environment-bootstrap.md](docs/runbooks/dev-environment-bootstrap.md).

The demo builds itself live — the install starts the brewery sim
and it fills in as the sim ticks. If you're working on the sim
itself, the
[validation section in oss-quickstart's README](infra/oss-quickstart/README.md#validating-the-brewery-sim-maintainers)
covers `BOSS_REGEN_DAYS=14` for a ~5 min run that checks a clean
rebuild.

Dev loop once installed: the bare-metal Rust services run as
plain background processes (logs in `~/.boss-logs/`, PIDs in
`~/.boss-pids`); systemd is the production-deploy path only. The
SPA reloads via `cd apps/web && bun run dev`. Type-check with
`cd apps/web && bun run typecheck`; Rust suites with
`cargo test -p <crate>`.

If something doesn't work, file an issue.

## Reaching us

- GitHub issues for bugs, features, questions.
- Security disclosures: see [`SECURITY.md`](SECURITY.md) for
  the private reporting channel.

Thanks again for contributing.
