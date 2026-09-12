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
    # Where this script's own library lives, captured while $0 still
    # points into the checkout: after the exec it points at the snapshot
    # in /tmp, so `dirname $0` finds nothing of ours.
    BOSS_FORGE_CONVERGE_INFRA="${BOSS_FORGE_CONVERGE_INFRA:-$(cd "$(dirname "$0")/.." && pwd)}" \
        BOSS_CONVERGE_SNAPSHOT="$snap" exec bash "$snap" "$@"
fi
trap 'rm -f "$BOSS_CONVERGE_SNAPSHOT"' EXIT

REPO="${BOSS_FORGE_REPO_DIR:-/home/david/boss}"
OWNER="${BOSS_FORGE_REPO_OWNER:-david}"

# WHAT THIS RUN LEAVES FOR ITS OWN PACKET. maintenance-forge-converge
# closed `result=ok` carrying nothing else — the same silence as its
# boss-gcp sibling, which on 2026-09-11 was enough to support a wrong
# conclusion about what a converge had installed (infra/run-summary.sh
# carries the measurement). This host does have a readable journal door,
# so the stakes are lower; it is also the host whose door once answered
# 200 with a seven-hour-stale journal, and two converges that report
# differently about the same question are the §9a shape. install.sh below
# records what it installed; this records which commit from.
#
# CLEARED FIRST, BEFORE ANYTHING CAN FAIL. boss-step.sh reads the file
# from ExecStopPost — a different process — so a run that dies early must
# leave nothing, or the last run's success is read as this run's.
# shellcheck source=infra/run-summary.sh
. "${BOSS_FORGE_CONVERGE_INFRA:-$(dirname "$0")/..}/run-summary.sh"
run_summary_reset

# Fetch and check out forge main as the checkout's OWNER, never as root
# — a root `git` in a david-owned clone leaves root-owned objects that
# break the owner's later pulls. `-l` gives the owner's login env so the
# credential helper that carries the forge token is found. Detached at
# the sha (like cluster-deploy-runner) rather than a tracking branch, so
# the host holds no branch state of its own to diverge. --ff-only is
# implicit in a detached checkout of the fetched sha: no merge is made.
#
# Through the checkout's ONE lock (checkout-lock.sh, sourced inside the
# owner's shell so the lock file is the owner's): cluster-deploy-runner
# fetches this same checkout on this same tick, and its merge-triggered
# start lands a second after any merge. On 2026-09-07 22:01 this fetch
# held `refs/remotes/forgejo/main` while the runner's fetch died on it
# (backlog d66f92b2). The helper is read whole at source time, so the
# checkout it then performs cannot rewrite the code running it.
runuser -l "$OWNER" -c "cd '$REPO' && . infra/forge/checkout-lock.sh && checkout_git '$REPO' fetch -q forgejo main && checkout_git '$REPO' checkout -qf \"\$(git rev-parse forgejo/main)\""

# WHICH COMMIT THIS HOST'S UNITS NOW COME FROM, read as the checkout's
# owner for the same reason every git call above is: root cannot even READ
# a clone it does not own.
run_summary_field converge_sha "$(runuser -l "$OWNER" -c "git -C '$REPO' rev-parse HEAD")"

# WHAT THIS HOST IS FOR, read off the system of record the same way
# boss-gcp reads it (infra/estate/node-roles.sh, one definition). The
# forge's id is declared on the unit (BOSS_NODE_ID=forge), never guessed
# from a hostname. install.sh inherits BOSS_NODE_ROLES and installs what
# the roles bring — today `cluster-operator` brings talosctl and the
# credential check (design 1bc4b4ed).
NODE_ID="${BOSS_NODE_ID:-forge}"
. "${BOSS_FORGE_CONVERGE_INFRA:-$(dirname "$0")/..}/estate/node-roles.sh"
BOSS_CONVERGE_NAME="forge-converge" read_node_roles "$NODE_ID"
run_summary_field node_id "$NODE_ID"
run_summary_field node_roles "${BOSS_NODE_ROLES:-}"

# install.sh needs root (writes /etc/systemd/system). This script runs
# as root; git already finished above, so install.sh's bytes are stable
# for the duration of its run and it needs no snapshot of its own.
"$REPO/infra/forge/install.sh"
