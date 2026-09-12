# OSS quickstart — single image carrying every BOSS service binary
# + both built SPAs. Ships from `docker compose up` in
# `infra/oss-quickstart/docker-compose.yml`.
#
# Multi-stage:
#   1. rust-base   — toolchain + build deps.
#   2. rust-build  — compile the workspace in release mode, cargo's
#                    target and registry kept in BuildKit cache mounts
#                    between builds.
#   3. spa-build   — bun install + bun run build for BOTH SPAs
#                    (apps/web and apps/simulator).
#   4. runtime     — debian:slim with the binaries + SPAs copied in,
#                    and BOSS_BUILD_COMMIT set last.
#
# Image size: ~250 MB (binaries are stripped via `-C strip=symbols`
# in the workspace Cargo.toml; SPA dist is gzipped by Caddy).
#
# Build context: the repo root. Compose mounts this Dockerfile via
# `build.context: ../..` so the entire workspace is available.

# ---------------------------------------------------------------------------
# Stages 1–2 — Rust build, with the compiler's own cache kept between
# builds.
#
# THE BUILDER IS THE FORGE'S ROOTLESS BUILDKIT DAEMON, every train, and
# `docker compose` on an operator's machine; nothing else builds this
# file (CI's kaniko builds boss-ci, a different Dockerfile). Both keep
# local BuildKit state between builds, so `RUN --mount=type=cache` is
# warm where it runs — the reason it was once dropped for cargo-chef
# (a GitHub Actions layer cache that could not carry mounts) no longer
# applies, and the forge's converge stamps said what chef's layer was
# costing: 2026-09-12, `build_s=522` for a train that changed no Rust.
# Two causes, both fixed here:
#
#   - `ENV BOSS_BUILD_COMMIT` stood before `cargo build`, and
#     `option_env!` baked it into boss-core — so a value that changes
#     on every train recompiled every workspace crate on every train.
#     It now lives in the runtime stage (the last lines of this file),
#     read by the binaries from their environment: the image still
#     names the tree it was built from, and the compiled layer is
#     invalidated only by what it compiles.
#   - nothing of one build's `target/` survived into the next, so even
#     a one-crate change paid the whole workspace. The cache mounts
#     below keep cargo's incremental state and the registry between
#     builds; a cold or pruned cache costs one full build and nothing
#     else (`docker builder prune` is the reset).
#
# WHAT THE CACHE DOES NOT CHANGE: the binaries copied to /out are what
# `cargo build --release` produced for this tree, and the runtime stage
# is rebuilt for every commit. A train without a Rust change ships the
# identical binaries under a new commit, which is exactly what it is.
# ---------------------------------------------------------------------------
FROM rust:1.96-slim-bookworm@sha256:c8a94a78f67ec8c4d474ec7f71e0720f21eb7e584e158daec0874cafa7c30e4d AS rust-base

# sqlx + reqwest need pkg-config + libssl-dev for the rustls-native
# bridge; the rest of the workspace is pure Rust.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

FROM rust-base AS rust-build

# The real source. .dockerignore excludes target/ and node_modules/
# so only source goes into the build context.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY examples ./examples
# EVERY PATH THE CRATES READ AT BUILD TIME MUST BE HERE — through a
# build.rs, which RUNS in this stage, or an `include_str!`, which is
# read by rustc while compiling in it. This stage is a SUBSET of the
# repo, so a crate reaching outside `crates/` compiles on any full
# checkout and fails here, where nobody is looking. Both bit us on
# 2026-09-09, in consecutive trains:
# boss-dispatcher-handlers/build.rs derives the observer's spool
# directory and cap from infra/estate/observe-lib.sh so the shell and
# the Rust side cannot drift (CLAUDE.md §9a, a collapse rather than a
# pin). It landed green through a gate that builds from a full
# checkout, and then every converge from 04:30 failed on the build
# script's own `must exist relative to the crate root` panic. Before
# it, train 283 added infra/forge/host-absent-tools.txt, which
# boss-cli's gate.rs `include_str!`s in PRODUCTION code, and its
# converge failed four times from 03:46. Both were invisible, because
# the build was quiet.
# infra/lint/the-image-carries-what-build-scripts-read.sh now derives
# this list from the build scripts themselves and fails when they
# disagree.
COPY infra/postgres ./infra/postgres
# Named FILES, not their directories, on purpose: infra/forge changes
# several times a day and a directory COPY would invalidate the whole
# Rust build layer every time a shell script moved.
COPY infra/estate/observe-lib.sh ./infra/estate/observe-lib.sh
COPY infra/forge/host-absent-tools.txt ./infra/forge/host-absent-tools.txt
# `boss ops` compiles the ops allowlist in (include_str!), the one file
# the host runner enforces; the terminal applies the same rules first.
COPY infra/ops/verbs.json ./infra/ops/verbs.json
COPY infra/deploy-services.sh ./infra/deploy-services.sh
COPY infra/forge/run-car-probe.sh ./infra/forge/run-car-probe.sh

# NO `ENV BOSS_BUILD_COMMIT` HERE — see the stage header; it is set in
# the runtime stage so this layer keys on the sources alone.

# Build every binary the runtime needs. --features postgres pulls in
# the sqlx-backed adapters; the workspace builds in one cargo
# invocation so dep compilation is shared across crates. The three
# mounts are cargo's state between builds: the target dir (incremental
# compilation of deps and workspace alike), the registry (downloaded
# crates) and git checkouts. They are not part of the image; /out is.
RUN --mount=type=cache,id=boss-cargo-target,target=/build/target \
    --mount=type=cache,id=boss-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=boss-cargo-git,target=/usr/local/cargo/git \
    set -eu && \
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
# Stage 3 — SPA build.
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
# Stage 4 — runtime.
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
#
# python3: the SAME LOSS, found the same way one cutover later. The
# conductor runs the consist check — the 34 cheap lints, against the
# assembled tree — before it opens a PR, and
# `a-kind-bundle-does-not-tighten.sh` parses the platform manifest with
# a small inline python3 (deliberately hand-rolled rather than tomllib,
# which is 3.11+). On a box without python3 that lint cannot RUN, and
# the conductor reported the result as "1 of 34 checks disagree with
# the assembled tree" — refusing to board, striking no car, and
# blocking every train behind an infrastructure gap dressed as a
# consist failure (CLAUDE.md: an infrastructure refusal is not a
# consist failure).
#
# It hid until 2026-09-04 21:15 because every earlier board that day
# ran from the dev pod, which has python3; that firing was the first
# time the CONDUCTOR boarded from its own container since the cutover.
# The lesson both entries share: a component moved into this image
# loses every host tool it never declared, and it discovers which ones
# only when it next needs them.
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
    python3 \
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
# missing at ../platform/workflows/ relative to itself, and until
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
# /opt/boss/infra/dispatcher/rules/. Without this dir the rules runner
# can't start and Jobs get assigned + worked but never close.
COPY infra/dispatcher /opt/boss/infra/dispatcher
COPY examples /opt/boss/examples
# `COPY docs /opt/boss/docs` stood here until 2026-09-10, and it was
# load-bearing for one reader only: boss-docs-api ran with
# repo_root=/opt/boss and walked docs/design/*.md on every boot, so
# without the prose in the image a container deploy went review-blind
# (2026-08-12). That service, its corpus index and its parser were
# deleted with the rest of the markdown-corpus machinery (backlog
# f5da586c) — the packet is the doc — and nothing in the image reads
# /opt/boss/docs any more, so the image stops carrying the prose.

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
# What the launcher SOURCES must land beside it: services-launcher.sh
# reads tenant-launch.sh from its own directory, which in this image is
# /usr/local/bin, not infra/oss-quickstart. On 2026-09-05 the launcher
# landed without this line and the pod crash-looped on "No such file"
# before any API started — the system of record was dark for the
# duration of a rollback. The gate cannot see the image's file set
# (it never builds the image), so infra/lint/the-image-carries-what-
# the-launcher-sources.sh pins this pairing instead.
COPY infra/oss-quickstart/tenant-launch.sh /usr/local/bin/tenant-launch.sh
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

# The commit this image is built from, for every binary to read from
# its environment: `Capabilities.commit` on each service's /health (the
# train's `converged` step reads it back from the RUNNING pod — the
# proof of installation the arrival report requires, fdff316c /
# 7e5ee013) and `boss --version` / `boss orient` in the conductor.
# LAST, on purpose: it changes every build, and everything above it is
# cached by content.
ARG BOSS_BUILD_COMMIT=
ENV BOSS_BUILD_COMMIT=$BOSS_BUILD_COMMIT

USER boss
WORKDIR /home/boss

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/boss-launch"]
