# OSS quickstart — single image carrying every BOSS service binary
# + both built SPAs. Ships from `docker compose up` in
# `infra/oss-quickstart/docker-compose.yml`.
#
# Multi-stage:
#   1. rust-base   — toolchain + build deps + cargo-chef.
#   2. planner     — cargo-chef recipe (the dependency build plan).
#   3. rust-build  — cook the dep tree (a cacheable layer), then
#                    compile the workspace in release mode.
#   4. spa-build   — bun install + bun run build for BOTH SPAs
#                    (apps/web and apps/simulator).
#   5. runtime     — debian:slim with the binaries + SPAs copied in.
#
# Image size: ~250 MB (binaries are stripped via `-C strip=symbols`
# in the workspace Cargo.toml; SPA dist is gzipped by Caddy).
#
# Build context: the repo root. Compose mounts this Dockerfile via
# `build.context: ../..` so the entire workspace is available.

# ---------------------------------------------------------------------------
# Stages 1–3 — Rust build, split so the dependency tree is a real,
# cacheable image LAYER.
#
# cargo-chef separates "compile the ~400-crate dependency tree"
# (changes only when Cargo.lock / crate manifests change) from
# "compile our own crates" (changes every commit). CI's remote layer
# cache (--cache-to type=gha) can persist and restore LAYERS across
# runs — but never RUN cache mounts, which live in local BuildKit
# state and die with the runner. The previous cache-mount pattern was
# therefore warm for local rebuilds and permanently cold in CI (~20
# min of dep compilation every run). Baking the cook step into a
# layer serves both: local rebuilds hit the local layer cache, CI
# restores the same layer from the Actions cache.
# ---------------------------------------------------------------------------
FROM rust:1.96-slim-bookworm@sha256:c8a94a78f67ec8c4d474ec7f71e0720f21eb7e584e158daec0874cafa7c30e4d AS rust-base

# sqlx + reqwest need pkg-config + libssl-dev for the rustls-native
# bridge; the rest of the workspace is pure Rust.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Version-pinned + --locked, same discipline as the digest-pinned
# base images. Compiling chef costs ~1 min once; the layer keys only
# on the base image + apt layer above, so every rebuild — local or
# CI — reuses it until the toolchain image bumps.
RUN cargo install cargo-chef --version 0.1.77 --locked

WORKDIR /build

# ---------------------------------------------------------------------------
# Planner — compute recipe.json, the dependency build plan. Re-runs on
# ANY source change (cheap: manifest analysis only), but its output is
# byte-identical unless Cargo.toml / Cargo.lock content actually
# changed — and the expensive cook layer below keys on recipe.json
# CONTENT, so source-only commits leave the cooked deps cached.
# ---------------------------------------------------------------------------
FROM rust-base AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# ---------------------------------------------------------------------------
# Rust build — cook the dependency tree, then compile the workspace.
# ---------------------------------------------------------------------------
FROM rust-base AS rust-build

COPY --from=planner /build/recipe.json recipe.json

# Compile ONLY dependencies, against chef's stub skeleton of the
# workspace. Flags must mirror the real build below — a mismatch
# changes cargo's fingerprints and the deps silently recompile. The
# two umbrella-feature bins built explicitly below (accounts-api /
# events-api) may pull a few deps this pass misses; those compile in
# the final step at seconds of cost, not the ~20-min tree.
RUN cargo chef cook --release --workspace --features postgres --recipe-path recipe.json

# The real source. .dockerignore excludes target/ and node_modules/
# so only source goes into the build context.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY examples ./examples
COPY infra/postgres ./infra/postgres

# The commit this image is built from, exported to the cargo build so
# `Capabilities` self-reports it from every service's health endpoint
# (option_env! in boss-core). Declared AFTER the chef cook layer on
# purpose: the arg changes every build, and placing it above would
# invalidate the cached dependency tree each time. The train's
# `converged` step reads this back from the RUNNING pod — the proof of
# installation the arrival report now requires (fdff316c / 7e5ee013).
ARG BOSS_BUILD_COMMIT=
ENV BOSS_BUILD_COMMIT=$BOSS_BUILD_COMMIT

# Build every binary the runtime needs. --features postgres pulls in
# the sqlx-backed adapters; the workspace builds in one cargo
# invocation so dep compilation is shared across crates. The cooked
# target/ from the layer above carries the finished dep artifacts, so
# this step compiles workspace crates only.
RUN set -eu && \
    # Single cargo invocation across the whole workspace —
    # boss-brewery-sim builds by default (no feature gate), so it
    # rides the same --workspace --bins pass, sharing dep compilation.
    # The prior two-cargo-build pattern invalidated cache between
    # passes (different feature set) and recompiled boss-brewery-
    # engine + dep tree twice; one pass halves the cold-build
    # wall-clock on cache-cold builds.
    cargo build --release --workspace \
        --features postgres \
        --bins && \
    # boss-accounts-api + boss-events-api gate on umbrella features
    # (accounts-api / events-api), not `postgres`, so the workspace
    # build above silently skips them. Build explicitly — else the
    # gateway 502s on /api/accounts and the brewery data-seed can't
    # land accounts/vendors.
    cargo build --release -p boss-accounts --bin boss-accounts-api --features accounts-api && \
    cargo build --release -p boss-events --bin boss-events-api --features events-api && \
    mkdir -p /out && \
    # Copy every workspace binary to /out — the runtime stage's COPY
    # source. Strict mode (set -e) means the build fails if the
    # cargo step didn't produce binaries — silent failure used to
    # mask "image came up with no bins inside" bugs.
    find target/release -maxdepth 1 -type f \
        \( -name 'boss' -o -name 'boss-*' \) ! -name '*.d' \
        -exec cp {} /out/ \; && \
    test -x /out/boss-gateway || (echo "FATAL: boss-gateway missing in /out" >&2; ls /out >&2; exit 1) && \
    test -x /out/boss-brewery-sim || (echo "FATAL: boss-brewery-sim missing in /out" >&2; ls /out >&2; exit 1)

# ---------------------------------------------------------------------------
# Stage 4 — SPA build.
# ---------------------------------------------------------------------------
FROM oven/bun:1.3-debian@sha256:9dba1a1b43ce28c9d7931bfc4eb00feb63b0114720a0277a8f939ae4dfc9db6f AS spa-build

# Skip puppeteer's Chrome download in this build stage — puppeteer
# is a devDependency for browser-based smoke tests, not needed by
# the SPA build itself. Without this, `bun install` calls
# puppeteer's postinstall script which tries to extract Chrome via
# `unzip` and fails because the base image doesn't include it.
ENV PUPPETEER_SKIP_DOWNLOAD=true

WORKDIR /spa
# The frontend is a Bun WORKSPACE (root package.json `workspaces`
# [apps/*, libs/*]); the lockfile + `patchedDependencies` patch files
# live at the workspace ROOT, and apps/web depends on @boss/web-kit
# (workspace:*). Install from the root with every member's package.json
# present so the dependency layer stays cacheable + the workspace symlinks
# resolve.
COPY package.json bun.lock ./
COPY patches ./patches
COPY apps/web/package.json ./apps/web/package.json
COPY apps/simulator/package.json ./apps/simulator/package.json
COPY libs/web-kit/package.json ./libs/web-kit/package.json
RUN bun install --frozen-lockfile

# Source for the members the SPA build needs — apps/web, apps/simulator,
# and the @boss/web-kit workspace dependency they share.
COPY apps/web ./apps/web
COPY apps/simulator ./apps/simulator
COPY libs/web-kit ./libs/web-kit

# Both `src/build.ts` scripts shell out to `boss-ports-list` for the
# canonical service-port table (single-source-of-truth shared with
# the Rust binaries). The binary comes from stage 1; copy it onto
# PATH so the SPA build resolves it. Bare-metal `quickstart.sh`
# gets this for free because the Rust workspace builds first into
# target/release/, which the SPA's build.ts probes via
# `find_boss_bin`; in docker the stages are isolated so we have to
# stage the binary explicitly.
COPY --from=rust-build /out/boss-ports-list /usr/local/bin/boss-ports-list
RUN cd apps/web && bun run build
# The Simulator UX is a SECOND SPA — its own bundle, served by
# boss-simulator (not the gateway) under /simulator. It was never
# built here, so every image shipped without it: the service logged
# "no SPA bundle found — serving stub page" and the visitor got
# whatever the gateway's fallback handed back instead of the
# simulator. Build it in the same stage; the runtime stage below
# copies it to the dir the service reads.
RUN cd apps/simulator && bun run build && \
    test -f dist/index.html || \
    (echo "FATAL: apps/simulator built no dist/index.html" >&2; exit 1)

# ---------------------------------------------------------------------------
# Stage 5 — runtime.
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim@sha256:96e378d7e6531ac9a15ad505478fcc2e69f371b10f5cdf87857c4b8188404716 AS runtime

# Runtime deps: ca-certs for outbound HTTPS (the gateway), libssl
# for rustls's native bridge, postgresql-client for the init
# script's psql calls, tini as PID 1 so signal forwarding works.
# git: the cluster-resident train conductor (`boss train cadence`)
# clones the repo and asks `git merge-base --is-ancestor` to verify a
# train's merge is in the running commit. It used to run on boss-gcp,
# which had host git; moved into this image by the 2026-09-04 cutover,
# it silently lost git — exact-match convergence still worked (a string
# compare) but ancestry did not, so every superseded train wedged at
# `converged`. Named by the conductor's own log once ensure_clone
# stopped swallowing the error.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    postgresql-client \
    tini \
    bash \
    wget \
    curl \
    jq \
    git \
    && rm -rf /var/lib/apt/lists/*

# BOSS runs as a non-root user. UID 1500 matches the systemd-deploy
# convention so volume mounts behave the same in compose + bare-metal.
RUN groupadd --gid 1500 boss && \
    useradd --uid 1500 --gid boss --create-home --shell /bin/bash boss

# Pre-create the auth-volume mount point with boss ownership.
# Without this, docker volume init creates /var/lib/boss/auth as
# root:root and `boss-auth set` (run as the boss user from init.sh)
# silently fails to write credentials.toml. Same pattern for the
# sim-state dir (/var/lib/boss-sim) the brewery sim checkpoints its
# counterparty queue into — without it the sim (UID 1500) logs
# "could not create sim-state dir; Permission denied" and the
# collections queue can't survive a container restart.
RUN mkdir -p /var/lib/boss/auth /var/lib/boss-sim /etc/boss-gateway && \
    chown -R 1500:1500 /var/lib/boss /var/lib/boss-sim /etc/boss-gateway

# Binaries.
COPY --from=rust-build /out/ /usr/local/bin/

# SPA static files — Caddy serves these directly. A separate Caddy
# container in compose mounts /opt/boss/spa as a read-only volume.
COPY --from=spa-build /spa/apps/web/dist /opt/boss/spa

# Simulator UX bundle. Lands on boss-simulator's OWN default static
# dir rather than under /opt/boss like the web SPA: nothing mounts or
# re-serves this one, and the default is the only path the service
# reads when BOSS_SIM_STATIC_DIR is unset — which it is here, since
# services-launcher.sh execs the binary with the container's
# environment and compose sets no override. Putting it anywhere else
# would mean a second copy of the path in docker-compose.yml, free to
# drift from the binary (CLAUDE.md §9a). Read-only for the service,
# so root-owned is correct.
COPY --from=spa-build /spa/apps/simulator/dist /var/lib/boss-simulator/dist

# Brewery seeds + reference data — the `init.sh` first-run path
# applies these against the freshly-bootstrapped DB.
COPY infra/postgres /opt/boss/infra/postgres
# operator-baseline hires (emp-audit + the injected bootstrap-admin)
# MUST be available — seed-operator-baseline.sh reads this dir to POST
# them to /api/people once the stack is up.
COPY infra/operator-baseline /opt/boss/infra/operator-baseline
# The platform Workflow bundle — protocols shipped as DATA
# (protocols-as-data step 1). bootstrap-db.sh seeds it insert-if-
# missing at ../platform/workflows.toml relative to itself, and until
# this COPY existed that path was absent from the image, so the seed
# SILENTLY SKIPPED on every cluster boot: ten bundle kinds (the eight
# timer-maintenance protocols among them) never reached the SoR, and
# every boss-gcp maintenance unit died on a 400 posting a packet of an
# unknown kind — deploy-confirm and the conservation checks included,
# red on every tick since the cutover, invisible because a failed
# oneshot notifies nobody (found 2026-08-19).
COPY infra/platform /opt/boss/infra/platform
# Dispatcher rule registry — boss-dispatcher's side-effect runner reads
# BOSS_DISPATCHER_RULES (set in docker-compose.yml) which points at
# /opt/boss/infra/dispatcher/rules.toml. Without this dir the rules runner
# can't start and Jobs get assigned + worked but never close.
COPY infra/dispatcher /opt/boss/infra/dispatcher
COPY examples /opt/boss/examples
# The design decision tracker (boss-docs-api, repo_root=/opt/boss)
# reads docs/design/*.md from the image. Without this COPY the
# tracker serves only DB-cached content, reindex finds zero docs,
# and design reviews never spawn on a container deploy - exactly
# how the cluster SoR went review-blind on 2026-08-12.
COPY docs /opt/boss/docs

# Post-API seed scripts boss-launch (services-launcher) runs once the
# API stack is up: the platform operator-baseline, then the brewery
# tenant (classes/JobKinds/policy/data via `boss-brewery-sim prepare`).
# boss-init runs PRE-API and can't seed through the API, so these live
# here and run from boss-services. Both are invoked by absolute path
# (/opt/boss/infra/...), so they need the execute bit.
COPY infra/seed-operator-baseline.sh /opt/boss/infra/seed-operator-baseline.sh
COPY infra/seed-brewery-tenant.sh /opt/boss/infra/seed-brewery-tenant.sh

# Quickstart launcher + init scripts.
COPY infra/oss-quickstart/init.sh /usr/local/bin/boss-init
COPY infra/oss-quickstart/services-launcher.sh /usr/local/bin/boss-launch
# The maintenance-packet wrap scripts, so in-cluster CronJob chores
# (boss-search-reindex first) open and complete their packets the same
# way the boss-gcp timers do — one script pair, both hosts (jq above
# is theirs).
COPY infra/boss-maintenance-wrap.sh infra/boss-step.sh infra/boss-api-curl.sh /usr/local/bin/
COPY infra/oss-quickstart/generate-configs.sh /usr/local/bin/boss-generate-configs
RUN chmod +x /usr/local/bin/boss-init /usr/local/bin/boss-launch \
    /usr/local/bin/boss-generate-configs \
    /opt/boss/infra/seed-operator-baseline.sh \
    /opt/boss/infra/seed-brewery-tenant.sh

# /etc must be writable by the boss user so generate-configs can
# write the per-service TOML files at container start. Default
# debian permissions on /etc are root:root 0755, which the boss
# user can't write to. Open it up; the container is ephemeral.
RUN chmod 0777 /etc

USER boss
WORKDIR /home/boss

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/boss-launch"]
