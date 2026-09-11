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
# to forge main and runs `deploy-services.sh units` — the EXISTING
# installer, in a mode that installs unit FILES and nothing else. It
# does not build, stage binaries, converge the schema, or restart a
# service. That restraint is the point: boss-gcp is the WireGuard
# bastion and still carries a second, older BOSS stack
# (boss-gcp-local), and a loop that bounced ~24 of its services every
# half hour would be a worse defect than the one it fixes. Code and
# schema on this host stay a deliberate, human-run `deploy-services.sh
# prod`; unit files converge.
#
# ONE-TIME BOOTSTRAP — the hand action that ends the hand actions.
# Nothing on boss-gcp installs the loop that does the installing, so
# somebody runs this once, from boss-gcp, as root. It is idempotent:
#
#   cd /opt/boss && git fetch forge main && git merge --ff-only FETCH_HEAD \
#     && sudo ./infra/deploy-services.sh units
#
# The `units` mode installs every TIMERS row, and `boss-gcp-converge` is
# one of them — so that command installs the loop that from then on
# installs everything, itself included. After it, no unit on this host
# needs a hand again.
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
        if printf '%s\n' "$remotes" | grep -qxF -- "$want"; then
            printf '%s\n' "$want"; return 0
        fi
        echo "boss-gcp-converge: BOSS_GCP_CONVERGE_REMOTE=$want names no remote of $REPO" >&2
        echo "    remotes: $(printf '%s' "$remotes" | tr '\n' ' ')" >&2
        return 1
    fi
    if printf '%s\n' "$remotes" | grep -qx forge; then
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

INSTALLER="${BOSS_GCP_CONVERGE_INSTALLER:-$REPO/infra/deploy-services.sh}"

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
    run_summary_note "deploy-services.sh units exited $rc — see this host's journal for every line"
    exit "$rc"
fi
echo "boss-gcp-converge: converged on ${after:0:8} ($REMOTE/main)"
