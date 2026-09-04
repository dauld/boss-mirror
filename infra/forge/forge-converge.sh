#!/usr/bin/env bash
#
# forge-converge — the forge host adopts its OWN units from forge main,
# the way cluster-deploy-runner rolls the CLUSTER onto forge main.
#
# WHY THIS EXISTS. The cluster converges every ten minutes; the forge
# host had no equivalent loop for its own infra/forge + infra/ops
# units, so every unit authored after the last hand-run of install.sh
# sat inert on main: reap-dead-ci-jobs through the 2026-08-17 fill,
# disk-floor-sweep through the 2026-09-03 fill that blocked every
# train. "Landed but never installed" was a recurring, expensive class
# of defect, and its single cause was that nothing ran install.sh. This
# closes the class: main moves, the next converge installs whatever is
# new. (docs/design/the-build-plane-manages-itself.md, car 4 / keystone.)
#
# It runs install.sh, which is idempotent — installs+enables every unit
# in its UNITS list and restarts only what changed. forge-converge is
# itself in that list, so after the ONE bootstrap `sudo install.sh` it
# reinstalls itself and no unit ever again needs a hand-install.
#
# Install (forge host — the one surviving hand action, the bootstrap
# that installs the loop that ends the bootstraps):
#   sudo cp infra/forge/forge-converge.{service,timer} /etc/systemd/system/
#   sudo systemctl daemon-reload && sudo systemctl enable --now forge-converge.timer
# or simply `sudo infra/forge/install.sh`, which now covers it.
set -euo pipefail

# SNAPSHOT-EXEC before touching git — the identical hazard
# cluster-deploy-runner.sh documents at length: the git checkout below
# rewrites THIS file's bytes while bash is still reading it by offset,
# so it resumes mid-token in the new contents — a silent, unrepeatable
# failure. exec into a copy first, so the bytes bash executes are
# unreachable from the repo git is about to move.
if [ -z "${BOSS_CONVERGE_SNAPSHOT:-}" ]; then
    snap="$(mktemp -t forge-converge.XXXXXX)"
    cat "$0" > "$snap"
    BOSS_CONVERGE_SNAPSHOT="$snap" exec bash "$snap" "$@"
fi
trap 'rm -f "$BOSS_CONVERGE_SNAPSHOT"' EXIT

REPO="${BOSS_FORGE_REPO_DIR:-/home/david/boss}"
OWNER="${BOSS_FORGE_REPO_OWNER:-david}"

# Fetch and check out forge main as the checkout's OWNER, never as root
# — a root `git` in a david-owned clone leaves root-owned objects that
# break the owner's later pulls. `-l` gives the owner's login env so the
# credential helper that carries the forge token is found. Detached at
# the sha (like cluster-deploy-runner) rather than a tracking branch, so
# the host holds no branch state of its own to diverge. --ff-only is
# implicit in a detached checkout of the fetched sha: no merge is made.
runuser -l "$OWNER" -c "cd '$REPO' && git fetch -q forgejo main && git checkout -qf \"\$(git rev-parse forgejo/main)\""

# install.sh needs root (writes /etc/systemd/system). This script runs
# as root; git already finished above, so install.sh's bytes are stable
# for the duration of its run and it needs no snapshot of its own.
"$REPO/infra/forge/install.sh"
