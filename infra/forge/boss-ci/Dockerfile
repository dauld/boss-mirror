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

# PATCH-EXACT, and paired with rust-toolchain.toml. A floating `1.96`
# resolves to whatever patch the tag points at today (1.96.1), which
# rustup then treats as a DIFFERENT toolchain name from the pin — so it
# reached out to static.rust-lang.org for a channel manifest before
# every job, for a toolchain already installed here. Naming the patch on
# both sides is what makes that resolve locally. Moving this line means
# moving rust-toolchain.toml in the same car.
FROM rust:1.96.1-slim-bookworm

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

# BAKE THE BROWSER. The mocked Playwright suite (apps/web test:mocked)
# needs chromium and its system libraries. Today the release CI's `web`
# job downloads them at run time — which redded train #162 when
# cdn.playwright.dev resolved to an IPv6 address the runner could not
# route (a700d3a4) — and the gate cannot run the suite at all because
# the deps are absent (ade5d82b). Baking both here removes the run-time
# download entirely and is the prerequisite that lets the gate mirror
# CI's web job.
#
# PINNED to the workspace's @playwright/test (apps/web/package.json:
# 1.61.0) so the baked browser is the exact build test:mocked drives; a
# floating version would bake a chromium the suite does not ask for.
# Validated by running `bunx playwright@1.61.0 install chromium` inside
# this very image in the dev pod before writing this line. `--with-deps`
# runs apt as root (this build is root; the pod is uid 1500, which is
# why the pod validation covered the browser and this covers the libs).
# PLAYWRIGHT_BROWSERS_PATH names an in-image location so the browser
# ships with the image; the gate-runner still overrides it to
# /gate-target today, so this is inert for gates until ade5d82b points
# the gate at the baked path — CI's web job, which sets no override,
# picks it up immediately and stops downloading.
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
RUN bunx playwright@1.61.0 install --with-deps chromium \
 && rm -rf /var/lib/apt/lists/*

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
