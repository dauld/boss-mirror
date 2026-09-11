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
#   ssh 10.20.0.15 'cd /home/david/boss && git fetch forgejo main \
#     && git checkout -qf FETCH_HEAD && sudo infra/forge/install.sh'
#
# (NOT `git pull` — the checkout tracks no upstream branch; it is driven
# by cluster-deploy-runner's detached checkouts of forgejo/main, so a
# bare pull has nothing to merge and stops to ask. This is the same
# fetch+checkout forge-converge.sh runs unattended.)
#
# It is idempotent — re-running installs the same files and restarts
# nothing that has not changed.
set -euo pipefail

cd "$(dirname "$0")" || exit 1
HERE="$(pwd)"

# What this run leaves for forge-converge's own packet — counts, each
# sub-installer's verdict, every anomaly verbatim. A no-op unless the
# caller set BOSS_RUN_SUMMARY_FILE; forge-converge.service does. The forge
# has a readable journal door, unlike boss-gcp, but a door that answered
# 200 with a seven-hour-stale journal is already on the record
# (2026-09-10), and two converges reporting differently about what they
# installed is the §9a shape. One definition: infra/run-summary.sh.
# shellcheck source=infra/run-summary.sh
. "${HERE}/../run-summary.sh"

# Where units land and who reloads them. Overridable so the installer
# can be exercised into a scratch directory with a stub systemctl —
# infra/lint/forge-install-covers-the-ops-runner.sh runs it on every
# gate and asserts what it would install. On the host both are the
# defaults, and root is required as before.
ETC="${INSTALL_ETC:-/etc/systemd/system}"
SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"

if [ "$ETC" = "/etc/systemd/system" ] && [ "$(id -u)" -ne 0 ]; then
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
# forge-converge: runs THIS script from forge main on a timer, so a
#   unit that lands on main installs itself on the next tick instead of
#   waiting for someone to remember to ssh in. It is the fix for the
#   whole class this file's header describes; disk-floor-sweep sitting
#   uninstalled through the 2026-09-03 fill is the most recent instance.
#   The bootstrap that installs forge-converge is the one surviving hand
#   action — after it, the host converges like the cluster.
# estate-observe-host: the forge observes itself every 15 minutes —
#   the estate loop's tightest disk was the one box with no observer
#   (49a8d842), and the boarding host check (BOSS_TRAIN_CI_HOST) can
#   only read a host that reports. Same script as boss-gcp's observer,
#   HOST_ID=forge.
# cluster-watchdog: the loop that knows the cluster is working from
#   OUTSIDE it — reads the API, compares what serves with what the
#   converge last stamped, rolls to that build by name when the API
#   has been dark longer than a deploy, and says so every 5 minutes.
#   No maintenance wrap, by design: the 2026-09-05 outage lasted four
#   hours because every loop that could act needed the API it watched.
UNITS=(
    reap-dead-ci-jobs
    cluster-deploy-runner
    disk-floor-sweep
    forge-converge
    estate-observe-host
    cluster-watchdog
)

installed=0
for u in "${UNITS[@]}"; do
    for ext in service timer; do
        src="${HERE}/${u}.${ext}"
        if [ ! -f "$src" ]; then
            echo "install.sh: ${u}.${ext} is listed here but missing from ${HERE}" >&2
            exit 1
        fi
        install -m 0644 "$src" "${ETC}/${u}.${ext}"
    done
    installed=$((installed + 1))
done

# kubectl ON THE HOST, for the observer that closes the converge loop.
# cluster-deploy-runner drives kubectl through the alpine/k8s image
# (no host binary needed to APPLY), but check-manifests-applied.sh —
# the "is what's in the tree what's running?" read that 60690755 found
# running nowhere with a real credential — calls plain `kubectl` over
# every manifest and cannot be fed through a container mount cleanly.
# One binary, pinned by sha so CDN weather cannot ship a different one
# (the a700d3a4 lesson), the same version as the image and the cluster
# line (1.33). Downloads once; a later run finds it and moves on.
KUBECTL_VERSION="v1.33.3"
KUBECTL_SHA256="2fcf65c64f352742dc253a25a7c95617c2aba79843d1b74e585c69fe4884afb0"
if [ "${INSTALL_KUBECTL:-1}" = "1" ] && [ ! -x /usr/local/bin/kubectl ]; then
    tmp="$(mktemp)"
    if curl -sfL -o "$tmp" "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl" \
        && echo "${KUBECTL_SHA256}  ${tmp}" | sha256sum -c - >/dev/null; then
        install -m 0755 "$tmp" /usr/local/bin/kubectl
        echo "install.sh: kubectl ${KUBECTL_VERSION} installed"
    else
        echo "install.sh: kubectl download or checksum failed — the manifests check will report 'cannot verify' until it is present" >&2
    fi
    rm -f "$tmp"
fi

# The system of record for the maintenance packets these units open.
# ONE definition (§9a), written as a per-unit drop-in so
# boss-maintenance-wrap.sh — which has NO localhost default and REFUSES
# without it — reaches the cluster jobs API. The forge host carries no
# deploy-services jobs-url.conf drop-in, which is why reap-dead-ci-jobs
# failed every run on 2026-09-03 until the URL was hand-authored. Now it
# is installed from the tree, so it converges instead of drifting.
JOBS_URL="http://10.20.0.34:7900"
for u in "${UNITS[@]}"; do
    mkdir -p "${ETC}/${u}.service.d"
    printf '[Service]\nEnvironment=BOSS_JOBS_URL=%s\n' "$JOBS_URL" \
        > "${ETC}/${u}.service.d/jobs-url.conf"
done

# The ops-request runner is the same unit boss-gcp runs, installed the
# same way: infra/ops/install-ops-runner.sh is the ONE definition of how
# a host gets one — the unit pair byte-identical from infra/ops, plus a
# drop-in carrying THIS host's identity and checkout and the system of
# record pinned inline. Until 2026-09-05 this host answered packets only
# because someone had installed it by hand; a rebuild would have lost
# the read door (packet 4d5f158a, infra/forge/OPERATIONS.md §Residue).
# The block that used to sit here was copied for boss-gcp on 2026-09-11
# and collapsed into that script the same day rather than living twice
# (CLAUDE.md §9a). It enables the timer itself, which is why the loop
# below no longer appends it.
#
# ITS FAILURE IS LOUD BUT LATE, and that is why the rc is carried instead
# of letting `set -e` act here: a runner that did not install deserves a
# red unit and a packet on the `failed` terminal, but not at the price of
# leaving every timer below installed-and-not-enabled.
ops_runner_rc=0
INSTALL_ETC="$ETC" INSTALL_SYSTEMCTL="$SYSTEMCTL" \
    bash "${HERE}/../ops/install-ops-runner.sh" forge || ops_runner_rc=$?
installed=$((installed + 1))

"$SYSTEMCTL" daemon-reload
for u in "${UNITS[@]}"; do
    "$SYSTEMCTL" enable --now "${u}.timer"
    printf '  %-24s %s\n' "$u" "$("$SYSTEMCTL" is-active "${u}.timer")"
done

# The journal-over-HTTP read door (:19531). Hand-enabled once on
# 2026-09-03 and in the tree nowhere, which is the Residue item this
# closed: a host rebuild silently loses the one door that still works
# when the API is dark.
#
# ONE DEFINITION, SHARED WITH boss-gcp's CONVERGE. The six lines that
# did this used to live here, and then backlog 68757702 found boss-gcp
# with no read path at all and needing exactly the same six. A copy is
# what drifts (CLAUDE.md §9a), so the how moved to
# infra/journal-door-ensure.sh and both converges call it: that file
# carries why the door exists, why nothing of ours is shipped for it, and
# why every failure in it is non-fatal.
JOURNAL_DOOR_URL="http://10.20.0.15:19531" bash "${HERE}/../journal-door-ensure.sh"

echo "install.sh: ${installed} unit pair(s) installed and enabled"
run_summary_field units_installed "$installed"
run_summary_field units_skipped 0
run_summary_field summary "installed $installed unit pair(s) and enabled their timers"
if [ "$ops_runner_rc" -ne 0 ]; then
    echo "install.sh: the ops-request runner did NOT install (exit $ops_runner_rc) — it named" >&2
    echo "    what failed above, and the run summary carries it. Every other unit converged;" >&2
    echo "    this host cannot answer an ops-request until that is fixed." >&2
    exit "$ops_runner_rc"
fi
