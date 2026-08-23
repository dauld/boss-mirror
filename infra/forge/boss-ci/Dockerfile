# CI runner image for the BOSS gate (internal-forge Q2's recorded
# debt: this Dockerfile lived off-repo on the forge host; now the
# image and the workflow that runs in it share one tree).
#
# Forgejo Actions runs JavaScript actions (actions/checkout) inside
# the job container, so node must be present or every job dies at
# checkout with exitcode 127. python3 used to be here for one reason
# only: boss-testing's conductor_preflight tests drove
# infra/train/conductor.py directly, and their absence-of-python
# failure mode was a panic that read as "3 tests failed" while the
# real story was "the container can't run what the suite runs"
# (forge train #1's red signal, 2026-08-12). Directive 26d61c97 ported
# the conductor to `boss train` and the tests now drive the compiled
# boss binary, so the interpreter — only ever there for the conductor
# tests — is out. Carrying rustfmt/clippy and the postgres client
# means a CI run does no toolchain installation at all.
#
# Build + push (forge host):
#   docker build -t 10.20.0.15:3000/david/boss-ci:rust1.96 infra/forge/boss-ci
#   docker push 10.20.0.15:3000/david/boss-ci:rust1.96
# bun is the workspace's package manager and script runner (root
# bun.lock, `bun run typecheck` in apps/web). The gate runs
# svelte-check, and without bun in the image there is no way to install
# the workspace's node_modules or invoke the script that drives it — so
# the web app's types went unchecked by CI entirely while every Rust
# lint was enforced. Copied from the official image rather than curl'd
# into a shell: a pinned tag is reproducible and reviewable, and the
# installer script wants unzip and writes to $HOME, neither of which
# this image should acquire for one binary.
FROM oven/bun:1.3-slim AS bun

FROM rust:1.96-slim-bookworm

# NODE COMES FROM NODESOURCE, NOT DEBIAN. bookworm ships Node 18, and
# Playwright 1.61 refuses to start on anything below 20 — which is how
# train 52's `web` job died a second time in one evening after the
# first fix. Measured in the dev pod (`node --version` -> v18.20.4)
# rather than discovered in CI, which is the whole argument for having
# a dev pod that runs this image.
#
# tmux is here for the durable session: `kubectl exec` dies with the
# laptop that opened it, so a long agent run needs a multiplexer INSIDE
# the pod to attach to and detach from. David, 2026-08-16: "My laptop
# is really meant to just be a thin terminal to interact with a durable
# session somewhere in the BossInfra layer."
RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends \
      git ca-certificates postgresql-client pkg-config libssl-dev curl gnupg tmux \
 && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/*

# bunx, not just bun. The old single-file COPY gave the image `bun`
# without the `bunx` symlink beside it, so `bunx playwright ...` exited
# 127 in CI while passing on every developer's Mac — train 52, round
# one. Copying both is the fix; `bun x` also works and the workflow
# uses it, but an image that carries half a tool is a trap for the next
# person.
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
RUN ln -sf /usr/local/bin/bun /usr/local/bin/bunx

RUN rustup component add rustfmt clippy

# THE BROWSER AND ITS LIBRARIES LIVE IN THE IMAGE.
#
# Two recurring costs, one root cause: the image knew about the mocked
# Playwright suite and carried nothing it runs on.
#
#   - CI installed the browser per job — `playwright install --with-deps
#     chromium`, ~1 min of every train, re-fetching the same build and
#     apt-getting the same sixteen libraries on each run.
#   - The dev pod keeps its browsers on a PVC
#     (PLAYWRIGHT_BROWSERS_PATH=/work/.ms-playwright) because the image
#     had none. But a browser's SYSTEM libraries cannot live on a PVC —
#     they are container-layer files — so every pod restart wiped them
#     and a human reinstalled them by hand through an apt-sandbox shim
#     (docs/runbooks/dev-pod-access.md, which names baking them here as
#     the durable fix). That happened repeatedly the week of 2026-08-17.
#
# THE PACKAGE NAMES ARE BOOKWORM'S. On trixie the same three are
# libasound2t64 / libcups2t64 / libatk1.0-0t64 (the time_t transition),
# so a list copied from a newer distro's docs fails this apt-get
# outright. Each of the sixteen was verified present in bookworm before
# this landed; if the base image's release ever moves, re-verify rather
# than assume.
RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends \
      libnss3 libnspr4 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2 \
      libxkbcommon0 libatspi2.0-0 libxcomposite1 libxdamage1 libxfixes3 \
      libxrandr2 libgbm1 libpango-1.0-0 libcairo2 libasound2 \
 && rm -rf /var/lib/apt/lists/*

# A BAKED DEFAULT, NOT A MANDATE. A container env var beats an image
# ENV, so the dev pod's existing `export
# PLAYWRIGHT_BROWSERS_PATH=/work/.ms-playwright` keeps winning and its
# PVC browsers keep being used when this image rolls — nothing on the
# pod side has to change on the same day. Once a pod runs this image
# that export is redundant rather than required, and dropping it is how
# the PVC copy eventually goes away.
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright

# THE VERSION IS THE REPO'S, AND IT IS CHECKED, NOT TRUSTED.
# Playwright resolves a browser by a revision the DRIVER picks, so a
# browser installed by 1.61.0 is invisible to a 1.62 driver: the suite
# dies with "Executable doesn't exist at …" and every spec fails at
# once. The driver version is `@playwright/test` in apps/web/package.json
# and kaniko builds this file with `--context-sub-path
# infra/forge/boss-ci`, so the build genuinely cannot read it — the fact
# has to live twice, which per CLAUDE.md §9a means it gets an equality
# test. locomotive.sh is that test: it compares this value against
# apps/web/package.json and reds in seconds with the bump to make.
#
# Deliberately not an ARG. A build-arg is a way for a caller to pass it
# wrong, and this file already retired one of those (the stamp below).
ENV BOSS_CI_PLAYWRIGHT_VERSION=1.61.0

# `bunx`, exercising the symlink made above — the image's own package
# runner, no second toolchain. Only chromium: the mocked suite declares
# one project and firefox/webkit would be ~500 MB nobody launches.
# The bun install cache goes with the layer for the same reason apt's
# lists do.
RUN bunx "playwright@${BOSS_CI_PLAYWRIGHT_VERSION}" install chromium \
 && rm -rf /root/.bun/install/cache

# Provenance stamp: sha256(Dockerfile + required-tools.txt). The
# locomotive CI job recomputes the same hash from its checkout and goes
# red on a mismatch — the equality test (CLAUDE.md 9a) between the image
# a runner cached and the image this tree describes.
#
# THE IMAGE COMPUTES ITS OWN STAMP. It used to arrive as a build-arg,
# which meant every caller had to derive it correctly and one of them
# did not: the kaniko CI job ran `cat Dockerfile required-tools.txt`
# in a container that had no checkout, so it would have stamped the
# image with the sha256 of empty input and locomotive would have gone
# on reporting a mismatch with no clue why. Hashing the two files from
# the build context removes the caller from the loop entirely — there
# is no longer a way to pass it wrong, because it is not passed.
COPY Dockerfile required-tools.txt /tmp/stamp/
RUN cat /tmp/stamp/Dockerfile /tmp/stamp/required-tools.txt \
      | sha256sum | cut -d' ' -f1 > /etc/boss-ci-stamp \
 && rm -rf /tmp/stamp \
 && cat /etc/boss-ci-stamp
