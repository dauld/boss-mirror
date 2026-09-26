#!/usr/bin/env bash
#
# boss-gcp-converge — boss-gcp adopts its OWN systemd units from forge
# main, the way forge-converge does on the build host.
#
# WHY THIS EXISTS. On 2026-09-04 the train conductor moved into the
# cluster (`feat/conductor-cutover`) and the boss-gcp deploy hop was
# retired with it. That hop was `sudo /opt/boss/infra/deploy-services.sh
# prod`, run by the train's `deployed` step, and it was the ONLY thing
# that installed systemd units on this host. Nothing replaced it, so
# every unit authored or FIXED after the cutover sat inert on main.
# Measured 2026-09-10 (backlog 408c81f6):
#
#   * maintenance-estate-observe-units — last packet 2026-09-04, the
#     cutover day, still OPEN. A five-minute observer.
#   * maintenance-conservation-invariants — last packet 2026-08-19,
#     while its timer fires hourly on the host.
#   * maintenance-ml-inference-batch — no packet has EVER been filed.
#
# Three urgent CADENCE SILENT alarms were auto-filed for exactly those,
# and the fix for the 400s behind them (4ef79606) had already LANDED —
# its own summary says why that changed nothing: "Nothing installs this
# unit on boss-gcp unattended today: landing this car changes the tree,
# not the host, until someone runs the deploy there." This is the
# "landed but never installed" class, and the forge host closed it with
# exactly this shape (infra/forge/forge-converge.sh, car 4 of
# docs/design/the-build-plane-manages-itself.md).
#
# WHAT IT DOES, AND DELIBERATELY DOES NOT. It fast-forwards the checkout
# to forge main and runs `infra/gcp/install-units.sh units` — the
# installer, in a mode that installs unit FILES and nothing else. It
# does not build, stage binaries, converge the schema, or restart a
# service. That restraint is the point: boss-gcp is the WireGuard
# bastion, and a loop that bounced services every half hour would be a
# worse defect than the one it fixes. (Until 2026-09-18 the installer
# was the `units` mode of infra/deploy-services.sh, the bare-metal
# deploy this host's second, older stack ran on; the stack was retired
# on 2026-09-15 and the deploy path deleted with backlog e109bd71 — the
# container launcher is the one way to run BOSS, and this host installs
# unit files only.)
#
# ONE BINARY IS THE EXCEPTION, since 2026-09-15: the `boss` CLI. After
# the units, the converge runs infra/estate/install-cli-from-image.sh
# (host-neutral since 2026-09-18, backlog 9f00a805: the forge's
# install.sh runs the same file for its cluster-operator role) with
# the sha it just converged to, which takes /usr/local/bin/boss out of
# the cluster image built for that commit (backlog 6f58e9a1, David's
# option (b)). Nothing else refreshed that binary — `prod` is a deploy of
# the stack 45641c91 retires — so it printed `boss 0.1.0` with no commit
# and every host verb that shells to it (publish-workflow, and the Drift
# tab's Approve through the same door) refused 78 by name (ops-request
# 20ba7cdf). Now the CLI on this host is the tree's CLI by construction,
# and the packet says so: `cli_sha` beside `converge_sha`, `cli_result`
# the verdict. Nothing is restarted by it; no service execs the CLI.
#
# ONE-TIME BOOTSTRAP — the hand action that ends the hand actions.
# Nothing on boss-gcp installs the loop that does the installing, so
# somebody runs this once, from boss-gcp, as root. It is idempotent:
#
#   cd /opt/boss && git fetch forge main && git merge --ff-only FETCH_HEAD \
#     && sudo ./infra/gcp/install-units.sh units
#
# The `units` mode installs every row infra/estate/roles.toml names for
# this host, and `boss-gcp-converge` is one of them — so that command
# installs the loop that from then on installs everything, itself
# included. After it, no unit on this host needs a hand again.
#
# Exercised on every gate by infra/lint/boss-gcp-converges-itself.sh,
# which runs the whole loop against a scratch checkout and a stub
# installer. The env overrides below exist for it; on the host every
# one of them is the default.
set -euo pipefail

REPO="${BOSS_GCP_REPO_DIR:-/opt/boss}"

# --resolve-remote: print the remote this loop would converge from and
# stop. No git writes, so it runs before the snapshot-exec below — it is
# the one question worth asking a host without changing it.
RESOLVE_ONLY=0
case "${1:-}" in
    --resolve-remote) RESOLVE_ONLY=1 ;;
    "") ;;
    *) echo "usage: $(basename "$0") [--resolve-remote]" >&2; exit 2 ;;
esac

[ -d "$REPO/.git" ] || { echo "boss-gcp-converge: $REPO is not a git checkout" >&2; exit 1; }

# EVERY git CALL RUNS AS THE CHECKOUT'S OWNER, READS INCLUDED.
#
# Two reasons, and the second is the one that bites. A root `git` that
# WRITES in a user-owned clone leaves root-owned objects that break the
# owner's later pulls — the forge loop's reason. But git also REFUSES TO
# READ across an ownership boundary: since 2.35.2 a repository whose
# owner is not the calling user is "dubious ownership" and every command
# fails, so a root-run `git remote` in david's /opt/boss would fail every
# tick. Loudly, which is the good half — but a loop that cannot read its
# own remote never gets as far as converging anything.
#
# The owner is read off the directory rather than hardcoded, so a rebuild
# that lands the checkout under a different account does not silently
# start corrupting it. `runuser -l` for the owner's LOGIN environment:
# that is where the credential helper carrying the forge token lives, and
# a fetch without it fails on a private forge. Skipped when we already
# ARE the owner, which is how the lint drives the loop without root.
OWNER="${BOSS_GCP_CONVERGE_OWNER:-$(stat -c %U "$REPO")}"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -l "$OWNER" -c "$1"
    fi
}

# WHICH REMOTE IS THE SOURCE OF TRUTH. /opt/boss on boss-gcp carries
# BOTH the internal forge and the public GitHub mirror, under names that
# are per-clone and have surprised this pipeline before. GitHub is the
# mirror, never the source (27ab7680): it lags the forge by a publish,
# so a converge that read it would install yesterday's units and report
# success. So: an explicit override wins, then a remote named `forge`,
# then the single non-github remote. Anything else REFUSES and names
# what it saw — a wrong target answers instead of erroring, and this is
# the one decision in the loop where a plausible guess is expensive.
resolve_remote() {
    local remotes want url
    remotes=$(as_owner "git -C '$REPO' remote")
    if [ -n "${BOSS_GCP_CONVERGE_REMOTE:-}" ]; then
        want="$BOSS_GCP_CONVERGE_REMOTE"
        if grep -qxF -- "$want" <<< "$remotes"; then
            printf '%s\n' "$want"; return 0
        fi
        echo "boss-gcp-converge: BOSS_GCP_CONVERGE_REMOTE=$want names no remote of $REPO" >&2
        echo "    remotes: $(printf '%s' "$remotes" | tr '\n' ' ')" >&2
        return 1
    fi
    if grep -qx forge <<< "$remotes"; then
        echo forge; return 0
    fi
    local candidates=""
    for want in $remotes; do
        url=$(as_owner "git -C '$REPO' remote get-url '$want'" 2>/dev/null || true)
        case "$url" in
            *github.com*) continue ;;
            "") continue ;;
        esac
        candidates="$candidates $want"
    done
    set -- $candidates
    if [ "$#" -eq 1 ]; then
        printf '%s\n' "$1"; return 0
    fi
    echo "boss-gcp-converge: cannot tell which remote of $REPO is the internal forge." >&2
    if [ "$#" -eq 0 ]; then
        echo "    Every remote it has is the GitHub mirror, which lags the forge by a" >&2
        echo "    publish and is never the source of truth — converging from it would" >&2
        echo "    install yesterday's units and report success." >&2
    else
        echo "    More than one candidate ($*) and none named 'forge'." >&2
    fi
    echo "    Remotes: $(for r in $remotes; do printf '%s=%s ' "$r" "$(as_owner "git -C '$REPO' remote get-url '$r'" 2>/dev/null)"; done)" >&2
    echo "    Name it: BOSS_GCP_CONVERGE_REMOTE=<remote>, or add a remote called 'forge'." >&2
    return 1
}

if [ "$RESOLVE_ONLY" = 1 ]; then
    resolve_remote
    exit $?
fi

# SNAPSHOT-EXEC before touching git — the hazard forge-converge.sh and
# cluster-deploy-runner.sh both document at length: the fast-forward
# below rewrites THIS file's bytes while bash is still reading it by
# offset, so it resumes mid-token in the new contents. A silent,
# unrepeatable failure. exec into a copy first, so the bytes bash
# executes are unreachable from the repo git is about to move.
if [ -z "${BOSS_GCP_CONVERGE_SNAPSHOT:-}" ]; then
    snap="$(mktemp -t boss-gcp-converge.XXXXXX)"
    cat "$0" > "$snap"
    # WHERE THIS SCRIPT'S OWN LIBRARY LIVES, captured while $0 still
    # points into the checkout. After the exec it points at the snapshot
    # in /tmp, so `dirname $0` can no longer find anything of ours — and
    # the library must come from the same tree as the script, not from
    # $REPO, which may be a different checkout entirely under test.
    BOSS_GCP_CONVERGE_INFRA="${BOSS_GCP_CONVERGE_INFRA:-$(cd "$(dirname "$0")/.." && pwd)}" \
        BOSS_GCP_CONVERGE_SNAPSHOT="$snap" exec bash "$snap" "$@"
fi
trap 'rm -f "$BOSS_GCP_CONVERGE_SNAPSHOT"' EXIT

INSTALLER="${BOSS_GCP_CONVERGE_INSTALLER:-$REPO/infra/gcp/install-units.sh}"
# Read from $REPO AFTER the fast-forward below, like the installer: the
# step that runs is the one the converged tree carries.
CLI_INSTALLER="${BOSS_GCP_CONVERGE_CLI_INSTALLER:-$REPO/infra/estate/install-cli-from-image.sh}"

# WHAT THIS RUN LEAVES FOR ITS OWN PACKET.
#
# The packet carried `result=ok` and nothing else until 2026-09-11, and
# that one field cost a wrong diagnosis the day the ops-runner block
# landed: both converges installed it, both said only "ok", and the
# question "did it?" took an ops-request plus 200 journal lines off the
# host to answer (infra/run-summary.sh carries the measurement). The
# installer's every line still goes to the journal, in full; the counted
# shape and every anomaly now also ride the packet, which is the record a
# reader with no host access has.
#
# SOURCED BEFORE THE FAST-FORWARD, like infra/forge/checkout-lock.sh: the
# merge below rewrites these bytes, and bash reads a sourced file the same
# way it reads this one.
#
# CLEARED FIRST, BEFORE ANY REFUSAL. boss-step.sh reads this file in
# ExecStopPost — a different process, linked to this one by nothing but
# the path — so a run that dies or refuses early must leave NOTHING
# behind, or the previous run's success gets stamped on this run's
# packet. That is "a wrong target answers instead of erroring" one layer
# in, and it is the failure this ordering exists to make impossible.
# shellcheck source=infra/run-summary.sh
. "${BOSS_GCP_CONVERGE_INFRA:-$(dirname "$0")}/run-summary.sh"
run_summary_reset

# A BUSY TREE IS REFUSED, NEVER CLOBBERED. The retired deploy hop this
# replaces (boss-cli train.rs `deploy`) read exactly these two facts and
# declined — "deploy tree busy (branch=…, dirty=…)" — rather than
# discarding whatever a human was in the middle of. `git checkout -qf`,
# which the forge loop can afford on a machine nobody edits, would throw
# that work away. boss-gcp is hand-operated, so the refusal stands and
# systemd records a failed unit, which is the alarm.
dirty=$(as_owner "git -C '$REPO' status --porcelain")
branch=$(as_owner "git -C '$REPO' rev-parse --abbrev-ref HEAD")
if [ -n "$dirty" ] || [ "$branch" != "main" ]; then
    # NAME WHAT FAILED, not that something did: which of the two facts
    # refused, and for a dirty tree every path, so the next reader does
    # not have to go and ask the host.
    echo "boss-gcp-converge: REFUSING — the checkout at $REPO is busy" >&2
    if [ "$branch" != "main" ]; then
        echo "    HEAD is on '$branch', not main." >&2
    fi
    if [ -n "$dirty" ]; then
        echo "    uncommitted paths:" >&2
        printf '%s\n' "$dirty" | sed 's/^/      /' >&2
    fi
    echo "    Nothing was fetched, moved or installed. Units on this host stay as they" >&2
    echo "    are until the tree is clean and on main — a converge that discarded" >&2
    echo "    somebody's work in progress would be a worse defect than a stale unit." >&2
    exit 1
fi

REMOTE=$(resolve_remote)
before=$(as_owner "git -C '$REPO' rev-parse HEAD")
as_owner "git -C '$REPO' fetch '$REMOTE' main"
target=$(as_owner "git -C '$REPO' rev-parse FETCH_HEAD")
# --ff-only so a checkout that has diverged from forge main fails loudly
# instead of growing a merge commit nobody authored.
as_owner "git -C '$REPO' merge --ff-only '$target'"
after=$(as_owner "git -C '$REPO' rev-parse HEAD")

if [ "$before" = "$after" ]; then
    echo "boss-gcp-converge: $REMOTE/main unchanged at ${after:0:8} — converging units anyway"
else
    echo "boss-gcp-converge: ${before:0:8} -> ${after:0:8} from $REMOTE/main"
fi
# Each fact on the packet as soon as it is true, never assembled at the
# end: the installer below can fail, and a summary built after it would
# be a summary the failure took with it.
run_summary_field converge_remote "$REMOTE"
run_summary_field converge_from "$before"
run_summary_field converge_sha "$after"

# CONVERGE, EVERY TICK, WHETHER OR NOT MAIN MOVED. The installer is
# idempotent and cheap (file copies + daemon-reload), and a unit removed
# or edited by hand has to come back on the next tick: that is what
# converge means. Gating the install on a moved sha would mean the host
# only self-heals when somebody happens to merge something.
#
# CAPTURED, THEN PRINTED — never reduced before it is stored. CLAUDE.md
# §Diagnosis: `-q`, a tail and a digest suppress OUTPUT, not work, and
# the cost is paid by whoever is next in front of the failure. So the
# installer's every line is kept and every line is printed, with a loud
# banner when it failed.
# WHAT THIS HOST IS FOR, read off the system of record — one definition
# for every managed host, in infra/estate/node-roles.sh (the forge reads
# its roles the same way). Best-effort: an unreachable registry leaves
# the roles empty and every row installs, as before roles existed.
NODE_ID="${BOSS_NODE_ID:-$(hostname -s)}"
# THE ADDRESS FILE, BEFORE ANYTHING THAT READS IT. /etc/boss/sor.env is
# the one place on this host that spells the system of record; every
# unit the installer below puts down reads it with EnvironmentFile=, the
# ops-runner installer checks it, and the roles read next uses it.
# Rendered from infra/estate/estate.toml on every tick — the one tree
# source (backlog 5222163e, audit H10) — so a moved address reaches this
# host by a merge and a tick, not an ssh. Written first so a unit
# installed on this tick never starts without it. The renderer is read
# from this script's own infra directory (on the host, $REPO/infra
# after the fast-forward above, so the converged tree's source; under
# test, the tree the script came from — a fixture checkout carries no
# estate.toml). The path is a knob only so a test never writes /etc.
SOR_ENV="${BOSS_GCP_CONVERGE_SOR_ENV:-/etc/boss/sor.env}"
if ! bash "${BOSS_GCP_CONVERGE_INFRA:-$(dirname "$0")/..}/estate/render-sor-env.sh" --to "$SOR_ENV"; then
    echo "boss-gcp-converge: could not render $SOR_ENV from infra/estate/estate.toml —" >&2
    echo "    nothing installed: every unit reads that file, and a unit without its address" >&2
    echo "    would answer a wrong target instead of erroring." >&2
    run_summary_field sor_env "not rendered"
    exit 1
fi
export BOSS_SOR_ENV="$SOR_ENV"
run_summary_field sor_env "$SOR_ENV"
. "${BOSS_GCP_CONVERGE_INFRA:-$(dirname "$0")}/estate/node-roles.sh"
BOSS_CONVERGE_NAME="boss-gcp-converge" read_node_roles "$NODE_ID"
run_summary_field node_id "$NODE_ID"

# WHAT THE cluster-operator ROLE BRINGS, when this host holds it. Added
# 2026-09-20: the forge was the estate's ONLY cluster-operator, so a
# forge window — planned or not — left nobody holding talosctl, kubectl
# or the cluster credentials, and boss-gcp is the one estate node that
# is neither the forge nor inside the cluster it would be operating.
# The installer is the same file the forge runs (infra/estate/
# install-cluster-operator.sh): one talosctl pin, one credential check,
# reported here through run_summary_field the way the forge reports it.
# Sourced AFTER read_node_roles above, because has_role is what decides.
. "${BOSS_GCP_CONVERGE_INFRA:-$(dirname "$0")/..}/estate/install-cluster-operator.sh"
if has_role cluster-operator; then
    install_cluster_operator
fi

log="$(mktemp -t boss-gcp-converge-install.XXXXXX)"
rc=0
"$INSTALLER" units >"$log" 2>&1 || rc=$?
sed 's/^/  install: /' "$log"
rm -f "$log"
if [ "$rc" -ne 0 ]; then
    echo "boss-gcp-converge: the installer FAILED (exit $rc) at ${after:0:8} — its complete" >&2
    echo "    output is above, every line of it. Units on this host are whatever the" >&2
    echo "    previous converge left; nothing was removed." >&2
    # systemd's own verdict says the run died; this says WHERE, on the
    # packet, beside whatever the installer had already recorded.
    run_summary_field installer_exit "$rc"
    run_summary_note "install-units.sh units exited $rc — see this host's journal for every line"
    exit "$rc"
fi
echo "boss-gcp-converge: units converged on ${after:0:8} ($REMOTE/main)"

# THE CLI, FROM THE IMAGE AT THE SHA JUST CONVERGED TO. After the units
# and never instead of them: a CLI step that cannot pull (the forge or
# the LAN dark, a tag the deploy runner has not built yet) must leave
# the units converged and REPORTED, which they are by now — its own
# facts (cli_sha, cli_result, cli_action, cli_image) it records itself
# through the same summary file. Its output is captured and printed
# whole under its own prefix, like the installer's. A failure here is
# still a failed converge: the host has not converged on the tree until
# its CLI is the tree's, and a packet that read `ok` over a stale CLI
# would be the 2026-09-15 defect with a green light on it.
#
# EXCEPT A WAIT, since 2026-09-26 (backlog f15ff5f2). Exit 75 is the
# installer's `not yet`: the registry answered and has no image for this
# commit's tag, because the deploy runner builds it after each train —
# measured on #713, landed 16:14Z, this tick 404'd at 16:41:58Z, the
# image rolled at 16:43:49Z. Redding on it made the tick after nearly
# every train a failed unit, and the unit observer an URGENT alarm the
# next tick closed by itself: 7 between 2026-09-23 and 2026-09-26, none
# a fault — an alarm nobody could read (CLAUDE.md §Diagnosis). The forge
# made this choice first (infra/forge/install.sh). So a wait is exit 0,
# on the packet as the installer's `cli_result` plus `cli_wait_minutes`
# here, and the next tick retries.
#
# THE WAIT HAS A LIMIT, so the stale-CLI concern above stays loud: the
# age is measured from the OLDEST commit on main the host's CLI lacks
# (the first after `current` in the installer's store; the converged
# commit itself when there is no generation to measure from), so a
# train landing every half hour cannot keep resetting it. Past
# BOSS_GCP_CONVERGE_CLI_WAIT_MAX_MIN (90: three ticks, against a build
# measured at ~30 min after landing) the image is not coming and the
# wait reds like any other failure. The alarm is the age of the wait,
# not the first tick after every train.
CLI_WAIT_MAX_MIN="${BOSS_GCP_CONVERGE_CLI_WAIT_MAX_MIN:-90}"
log="$(mktemp -t boss-gcp-converge-cli.XXXXXX)"
cli_rc=0
"$CLI_INSTALLER" "$after" >"$log" 2>&1 || cli_rc=$?
sed 's/^/  cli: /' "$log"
rm -f "$log"
if [ "$cli_rc" -eq 75 ]; then
    have=$(readlink "${BOSS_CLI_STORE:-/opt/boss-cli}/current" 2>/dev/null || true)
    missing=""
    if [ -n "$have" ]; then
        missing=$(as_owner "git -C '$REPO' rev-list --reverse '$have..$after'" 2>/dev/null || true)
    fi
    oldest="${missing%%$'\n'*}"
    oldest="${oldest:-$after}"
    landed=$(as_owner "git -C '$REPO' log -1 --format=%ct '$oldest'" 2>/dev/null || true)
    case "${landed:-empty}" in
        empty|*[!0-9]*)
            # An age that cannot be read is not a wait that can be
            # bounded: red, naming why, rather than wait forever.
            echo "boss-gcp-converge: cannot read when ${oldest:0:8} landed — the CLI wait cannot be bounded" >&2
            run_summary_field cli_exit "$cli_rc"
            exit "$cli_rc" ;;
    esac
    wait_min=$(( ($(date +%s) - landed) / 60 ))
    [ "$wait_min" -ge 0 ] || wait_min=0
    run_summary_field cli_wait_minutes "$wait_min"
    if [ "$wait_min" -lt "$CLI_WAIT_MAX_MIN" ]; then
        echo "boss-gcp-converge: units converged on ${after:0:8}; the CLI image is not built yet —"
        echo "    ${wait_min} min since ${oldest:0:8} landed (limit ${CLI_WAIT_MAX_MIN} min). The deploy runner"
        echo "    builds it after each train; the next tick retries, and /usr/local/bin/boss stays"
        echo "    whatever the previous converge confirmed (cli_result on the packet)."
        exit 0
    fi
    run_summary_note "the CLI image is not built ${wait_min} min after ${oldest:0:8} landed (limit ${CLI_WAIT_MAX_MIN} min) — the host's boss is falling behind the tree"
    echo "boss-gcp-converge: the CLI image is still not built ${wait_min} min after ${oldest:0:8} landed" >&2
    echo "    (limit ${CLI_WAIT_MAX_MIN} min) — not a wait any longer: the deploy runner is not building it." >&2
fi
if [ "$cli_rc" -ne 0 ]; then
    echo "boss-gcp-converge: the CLI step FAILED (exit $cli_rc) at ${after:0:8} — its complete" >&2
    echo "    output is above. Units on this host are converged; /usr/local/bin/boss is" >&2
    echo "    whatever the previous converge confirmed (cli_result on the packet says why)." >&2
    # The step records cli_result itself when it can; a step that died
    # before it could still leaves the exit on the packet.
    run_summary_field cli_exit "$cli_rc"
    exit "$cli_rc"
fi
echo "boss-gcp-converge: converged on ${after:0:8} ($REMOTE/main) — units and CLI"
