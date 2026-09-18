# Runbook: Dev environment bootstrap (fresh box)

What a fresh machine needs to **work on BOSS itself**: the
toolchains, the pre-push hook, a Postgres for the DB-backed tests,
and the one way to run what you built — the container image.
Captures the sharp edges of a fresh-box bring-up so the next person
hits them as text on a page, not as crashes.

Audience: whoever is setting up a laptop or a VM to build and test
BOSS. Assumes Ubuntu 24.04 (macOS notes inline) and the repo cloned.

**BOSS's own development runs on the cluster dev pod**
([`docs/design/dev-cluster.md`](../design/dev-cluster.md),
[`dev-pod-access.md`](dev-pod-access.md)), which ships every tool
below and builds through `wt-cargo`. This runbook is for a box that
is not the pod.

**There is no host-native service install.** Until 2026-09-18 this
runbook went on to create databases, build and install binaries to
`/usr/local/bin`, write systemd units, and deploy a per-service
stack on the box (`deploy-services.sh`, `bootstrap-vm.sh`). That path
had no caller since the 2026-09-04 conductor cutover and had drifted
from the image three ways; it was deleted (design 42277636, backlog
e109bd71). Running BOSS is §3.

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

(macOS: `unzip` ships in the base system; `xcode-select --install`
for the C toolchain; `brew install postgresql@16`.)

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

## 1. A Postgres for the tests

The DB-backed tests (`boss_testing::TestDb`, behind the `postgres`
feature) create a fresh, randomly-named database per test and drop
it after. They need a role that can `CREATEDB`, reachable at the
default admin URL `postgres://boss:boss@127.0.0.1/postgres` (or
`BOSS_TEST_POSTGRES_ADMIN_URL`):

```sh
sudo -u postgres psql -d postgres -c \
    "CREATE ROLE boss WITH LOGIN SUPERUSER PASSWORD 'boss'"
```

**Never point that URL at a production database through a
port-forward** — the tests answer instead of erroring, and on
2026-08-14 that crashed the cluster's Postgres (CLAUDE.md, the
database invariant).

## 2. Build and test

```sh
cargo build --workspace                       # ~15–20 min cold, ~30 s warm
cargo test --all-features                     # the Rust suites, DB-backed ones included
( cd apps/web && bun install && bun run typecheck && bun test )
bash infra/gate.sh --quick                    # what the hook runs
```

A few `*-api` bins declare `required-features = ["postgres"]`, so a
plain `cargo build --workspace` skips them; the image build
(`infra/oss-quickstart/Dockerfile`) builds every binary with the
features it needs, which is why the image is the artifact and a
host's `target/release` is not.

The gate that judges a car runs the same checks on the cluster
(`boss gate <branch> --wait`); `infra/gate.sh` is the one definition.

## 3. Run what you built

The container image, under the same compose file the OSS quickstart
uses:

```sh
cd infra/oss-quickstart
cp .env.example .env            # set BOSS_BOOTSTRAP_ADMIN_EMAIL
docker compose up --build       # builds boss:latest from your tree
```

Open `http://localhost:4443`. Timings, log checkpoints, the guest
button and troubleshooting are in
[`infra/oss-quickstart/README.md`](../../infra/oss-quickstart/README.md);
seeding a different tenant is
[`examples/used-device-shop/DOMAIN.md`](../../examples/used-device-shop/DOMAIN.md)
§Install. The brewery tenant seeds itself through the public API on
first start and the sim builds the demo live from an empty
`audit_log`.

To validate a full sim-year reconstructs and reconciles cleanly (the
maintainers' correctness check before a release cut):

```sh
sudo ./infra/postgres/validate-brewery-sim.sh
```
