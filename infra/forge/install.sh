#!/usr/bin/env bash
# Install the forge host's systemd units from this checkout.
#
# WHY THIS EXISTS. boss-gcp has had `deploy-services.sh` with a TIMERS
# array since the day its own comment was written: "Adding a new timer
# = author the .service + .timer in the right place under infra/, then
# add a row here. New timers land via `sudo ./infra/deploy-services.sh
# prod` instead of a `sudo install` treadmill that's been the source of
# every 'this timer was authored but never installed' gap so far
# (audit-integrity, ml-inference-batch, ledger-recognize,
# conservation-invariants — all caught by hand)."
#
# The forge host had no equivalent, so its units went on by hand, and
# one of the two was forgotten. On 2026-08-17 the CI runner's disk
# defect (feedback 1b63456b) was still open because
# `reap-dead-ci-jobs` — script, service and timer, all committed —
# had never been installed. `cluster-deploy-runner` had been. Nothing
# in the tree could tell the difference, and nothing was going to.
#
# WHERE IT RUNS. On the forge host (10.20.0.15), from its checkout at
# /home/david/boss, which is where the installed units already point:
#
#   ssh 10.20.0.15 'cd /home/david/boss && git pull && sudo infra/forge/install.sh'
#
# It is idempotent — re-running installs the same files and restarts
# nothing that has not changed.
set -euo pipefail

cd "$(dirname "$0")" || exit 1
HERE="$(pwd)"

if [ "$(id -u)" -ne 0 ]; then
    echo "install.sh: needs root to write /etc/systemd/system — re-run with sudo." >&2
    exit 1
fi

# Every unit this host runs. A unit absent from this list is a unit
# nobody installs, which is the entire defect above.
#
# reap-dead-ci-jobs: removes the corpses of crashed CI jobs and the
#   named volumes they hold. A crashed job's volume is NAMED, so
#   `docker volume prune` skips it — on 2026-08-14 one held 63GB and
#   left the next run 74GB, less than a cold `cargo test` needs, and
#   the symptom was four unrelated boss-ledger tests failing on
#   "could not extend file".
# cluster-deploy-runner: builds forge main and rolls the cluster onto
#   it every ten minutes. This is the SECOND deploy path — the
#   conductor deploys boss-gcp — and the reason "the train deployed"
#   and "the cluster is current" can differ by ten minutes.
# disk-floor-sweep: below BOSS_DISK_FLOOR_GB free on the root volume,
#   reclaims regenerable docker caches in a fixed order and stops at
#   the floor; an unmet floor is a failed unit, which is the alarm.
#   Exists because cluster-deploy-runner's cleanup only runs when main
#   moves — which needs CI — which needs disk. Circular exactly when
#   the disk fills, which it did on 2026-09-02, blocking every train.
UNITS=(
    reap-dead-ci-jobs
    cluster-deploy-runner
    disk-floor-sweep
)

installed=0
for u in "${UNITS[@]}"; do
    for ext in service timer; do
        src="${HERE}/${u}.${ext}"
        if [ ! -f "$src" ]; then
            echo "install.sh: ${u}.${ext} is listed here but missing from ${HERE}" >&2
            exit 1
        fi
        install -m 0644 "$src" "/etc/systemd/system/${u}.${ext}"
    done
    installed=$((installed + 1))
done

systemctl daemon-reload
for u in "${UNITS[@]}"; do
    systemctl enable --now "${u}.timer"
    printf '  %-24s %s\n' "$u" "$(systemctl is-active "${u}.timer")"
done

echo "install.sh: ${installed} unit pair(s) installed and enabled"
