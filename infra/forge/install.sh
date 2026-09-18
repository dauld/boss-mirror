#!/usr/bin/env bash
# Install the forge host's systemd units from this checkout.
#
# WHY THIS EXISTS. boss-gcp has had an installer with a timer roster
# (today infra/gcp/install-units.sh reading infra/estate/roles.toml;
# until 2026-09-18 the TIMERS array of the deleted bare-metal deploy
# script) since the day its own comment was written: "Adding a new
# timer = author the .service + .timer in the right place under infra/,
# then add a row here" — instead of a `sudo install` treadmill that had
# been the source of every 'this timer was authored but never installed'
# gap so far (audit-integrity, ml-inference-batch, ledger-recognize,
# conservation-invariants — all caught by hand).
#
# The forge host had no equivalent, so its units went on by hand, and
# one of the two was forgotten. On 2026-08-17 the CI runner's disk
# defect (feedback 1b63456b) was still open because
# `reap-dead-ci-jobs` — script, service and timer, all committed —
# had never been installed. `cluster-deploy-runner` had been. Nothing
# in the tree could tell the difference, and nothing was going to.
#
# WHERE IT RUNS. On the forge host (infra/estate/estate.toml
# `forge_host`), from its checkout at /home/david/boss, which is where
# the installed units already point:
#
#   ssh <forge> 'cd /home/david/boss && git fetch forgejo main \
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

# THE ADDRESS FILE, FIRST. /etc/boss/sor.env is the one place on this
# host that spells the system of record and the forge's own addresses;
# every unit installed below reads it with EnvironmentFile= (no `-`: a
# unit that started without its address would answer a wrong target)
# and every script sources infra/lib/sor.sh. Rendered from the tree's
# ONE source, infra/estate/estate.toml, on every converge — so the
# boss.algedonic.dev cutover is an edit to that file and a tick of this
# timer (backlog 5222163e, audit H10). Before the units, deliberately:
# a daemon-reload that finds the file absent would leave every unit
# refusing to start until the next tick. Overridable for the scratch
# run the lints drive (a test never writes /etc).
SOR_ENV="${INSTALL_SOR_ENV:-/etc/boss/sor.env}"
bash "${HERE}/../estate/render-sor-env.sh" --to "$SOR_ENV"
run_summary_field sor_env "$SOR_ENV"
# Now this run itself has the addresses the rest of the install reads
# (the journal door below, the roles read, the ops-runner installer).
export BOSS_SOR_ENV="$SOR_ENV"
# shellcheck source=infra/lib/sor.sh
. "${HERE}/../lib/sor.sh"
sor_require BOSS_JOBS_URL BOSS_FORGE_JOURNAL_URL

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

# WHAT THE cluster-operator ROLE BRINGS (design 1bc4b4ed: cluster
# management runs on this host; the workstation is a terminal). Read
# off BOSS_NODE_ROLES, which forge-converge exports from the estate
# registry; a hand run with no roles set installs nothing here and says
# so. Two things, and a check:
#
#   * talosctl, pinned by sha like kubectl above — the Talos client is
#     the ONLY interface to the nodes (no ssh), and it must stay within
#     one minor of the cluster. v1.13.8 is the version David's own client
#     runs, so the forge answers exactly as the workstation did.
#   * The credentials the role needs — /etc/boss-ops/talosconfig and
#     /etc/boss-ops/kubeconfig — are CHECKED, never written. They are
#     placed once by David (token admin is his) and must be root:root
#     mode 0600; anything else is reported on the converge packet as
#     absent-or-wrong until fixed. The estate's own converge never
#     writes a credential.
#   * The tree's `boss` CLI, since 2026-09-18 (backlog 9f00a805,
#     consolidation H8, car 1) — taken out of the cluster image built
#     for the commit this converge checked out, by the same installer
#     boss-gcp has run since 2026-09-15 (infra/estate/
#     install-cli-from-image.sh; the store is /opt/boss-cli, the link
#     /usr/local/bin/boss, `cli_sha` beside `converge_sha` on the
#     packet). Measured on #448: infra/forge/*.sh was 32 scripts and
#     9,790 lines, the largest of them shell twins of CLI verbs
#     (run-car-probe.sh for `boss prove --from-car`, tenant-census.sh
#     for `boss tenant`, …) each with its own pin, because this host
#     had no binary to shell to. The role that brings it is the one
#     the verbs serve: cluster management runs here. The CLI step runs
#     below, after the credential check.
. "${HERE}/../estate/node-roles.sh"
cli_rc=0
if has_role cluster-operator; then
    TALOSCTL_VERSION="v1.13.8"
    TALOSCTL_SHA256="406b56f9e4ff03b1557cc941b1f163aec8a6ebb36e28f0bbbe6d083589529261"
    if [ "${INSTALL_TALOSCTL:-1}" = "1" ] && [ ! -x /usr/local/bin/talosctl ]; then
        tmp="$(mktemp)"
        if curl -sfL -o "$tmp" "https://github.com/siderolabs/talos/releases/download/${TALOSCTL_VERSION}/talosctl-linux-amd64" \
            && echo "${TALOSCTL_SHA256}  ${tmp}" | sha256sum -c - >/dev/null; then
            install -m 0755 "$tmp" /usr/local/bin/talosctl
            echo "install.sh: talosctl ${TALOSCTL_VERSION} installed (cluster-operator)"
        else
            echo "install.sh: talosctl download or checksum failed — the cluster-operator role has no Talos client until it is present" >&2
        fi
        rm -f "$tmp"
    fi
    ops_missing=""
    for cred in talosconfig kubeconfig; do
        f="/etc/boss-ops/$cred"
        if [ ! -f "$f" ]; then
            ops_missing="$ops_missing $cred:absent"
        elif [ "$(stat -c '%U:%G %a' "$f" 2>/dev/null)" != "root:root 600" ]; then
            ops_missing="$ops_missing $cred:$(stat -c '%U:%G %a' "$f")"
        fi
    done
    if [ -n "$ops_missing" ]; then
        echo "install.sh: cluster-operator credentials not ready —${ops_missing} (want root:root 600 under /etc/boss-ops; placed by hand, never by this script)"
        if declare -F run_summary_field >/dev/null; then run_summary_field ops_credentials "not ready:${ops_missing}"; fi
    else
        echo "install.sh: cluster-operator credentials present (root:root 600)"
        if declare -F run_summary_field >/dev/null; then run_summary_field ops_credentials "present"; fi
    fi

    # THE CLI, FROM THE IMAGE AT THE SHA THIS CONVERGE CHECKED OUT.
    # forge-converge.sh hands the sha over as BOSS_CONVERGE_SHA (root
    # cannot read the owner's clone); a hand run has none and installs
    # no CLI rather than guessing one. The installer records its own
    # facts (cli_sha, cli_result, cli_action, cli_image) through the
    # run summary; its output is captured and printed whole under its
    # own prefix, like boss-gcp's converge prints it.
    #
    # THREE VERDICTS, NOT TWO. Exit 0 is the CLI confirmed at the sha.
    # Exit 75 is `not yet`: the registry answered and has no image for
    # this commit's tag — the deploy runner on THIS host builds it a
    # few minutes after each train, and this converge fetched main ten
    # minutes after the last one, so the first tick after every train
    # lands here. That is a wait, recorded on the packet, retried next
    # tick, and NOT a red: a converge that failed on every train would
    # be an alarm nobody could read (CLAUDE.md §Diagnosis). Anything
    # else is a real refusal — the registry dark, a digest mismatch, a
    # binary that names another commit — and reds the run the way it
    # reds boss-gcp's: after the units below are installed, enabled and
    # reported, with the exit on the packet. The installer leaves
    # /usr/local/bin/boss at whatever the previous confirmed generation
    # was in every non-zero case.
    if [ "${INSTALL_CLI:-1}" = "1" ]; then
        if [ -z "${BOSS_CONVERGE_SHA:-}" ]; then
            echo "install.sh: no converged sha in the environment (BOSS_CONVERGE_SHA, set by forge-converge.sh) — a hand run installs no CLI; the next converge tick does"
            run_summary_field cli_result "skipped: no BOSS_CONVERGE_SHA (hand run)"
        else
            cli_log="$(mktemp -t forge-install-cli.XXXXXX)"
            bash "${HERE}/../estate/install-cli-from-image.sh" "$BOSS_CONVERGE_SHA" >"$cli_log" 2>&1 || cli_rc=$?
            sed 's/^/  cli: /' "$cli_log"
            rm -f "$cli_log"
            case "$cli_rc" in
                0) echo "install.sh: the CLI is the tree's at ${BOSS_CONVERGE_SHA:0:8} (cluster-operator)" ;;
                75)
                    echo "install.sh: the image for ${BOSS_CONVERGE_SHA:0:8} is not in the registry yet — the deploy runner builds it after each train; the next tick retries, and /usr/local/bin/boss stays whatever the previous converge confirmed (cli_result on the packet)"
                    cli_rc=0 ;;
                *)
                    echo "install.sh: the CLI step FAILED (exit $cli_rc) at ${BOSS_CONVERGE_SHA:0:8} — its complete" >&2
                    echo "    output is above. Every unit still converges below; /usr/local/bin/boss is" >&2
                    echo "    whatever the previous converge confirmed (cli_result on the packet says why)." >&2
                    run_summary_field cli_exit "$cli_rc" ;;
            esac
        fi
    fi
else
    echo "install.sh: cluster-operator not among this host's roles (${BOSS_NODE_ROLES:-none}) — no Talos client installed, no CLI"
fi

# The per-unit `jobs-url.conf` drop-in that used to carry the system of
# record (from 2026-09-03, when reap-dead-ci-jobs failed every run for
# want of it, to 2026-09-18) is RETIRED: every unit reads
# /etc/boss/sor.env itself. A drop-in left behind would carry a second
# copy of the address that nothing re-renders, so it is removed — the
# converge that stops writing a file must also stop the file standing.
for u in "${UNITS[@]}"; do
    rm -f "${ETC}/${u}.service.d/jobs-url.conf"
    rmdir "${ETC}/${u}.service.d" 2>/dev/null || true
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
JOURNAL_DOOR_URL="$BOSS_FORGE_JOURNAL_URL" bash "${HERE}/../journal-door-ensure.sh"

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
# The CLI verdict last, for the same reason the ops runner's is: a
# refused pull deserves a red unit and a packet on `failed` — the host
# has not converged on the tree until its CLI is the tree's — but never
# at the price of a unit left uninstalled or a timer left disabled.
if [ "$cli_rc" -ne 0 ]; then
    echo "install.sh: the CLI did NOT install (exit $cli_rc) — cli_result on the packet says why." >&2
    echo "    Every unit converged; /usr/local/bin/boss is whatever the previous converge confirmed." >&2
    exit "$cli_rc"
fi
