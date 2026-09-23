#!/usr/bin/env bash
# install-ops-runner.sh — ONE definition of how a host gets an ops
# runner. Usage: install-ops-runner.sh <estate node id>
#                install-ops-runner.sh --in-role
#
# Two hosts answer ops-request packets and both install the runner from
# here: the forge, through infra/forge/install.sh (forge-converge, every
# ten minutes), and boss-gcp, through `install-units.sh units` (its
# self-converge, every half hour).
#
# WHY IT IS ONE SCRIPT. Until 2026-09-11 only the forge installed a
# runner, in a block of its own, and boss-gcp had none at all — so an
# ops-request filed against the WireGuard bastion sat at `ready` with
# nothing behind it and a human went back to being the transport
# (backlog c3d06016; David, design 9e3e093f: boss-gcp "isn't part of
# the kubernetes cluster... I still want it fully managed and
# maintained by BOSS and protocol"). Giving boss-gcp a second,
# near-identical block would have been the §9a defect — this is the
# collapse instead, so "how a host gets an ops runner" has one answer
# and a widening lands once.
#
# WHAT A HOST GETS
#   * the unit pair from infra/ops, BYTE-IDENTICAL on every host: the
#     file is the definition, and everything host-specific is the
#     drop-in's business
#   * a drop-in <host>.conf naming THIS host (HOST_ID, which the runner
#     refuses to run without) and running the runner from THIS
#     checkout
#   * the timer enabled
#
# WHERE THE SYSTEM OF RECORD COMES FROM. The unit reads
# /etc/boss/sor.env (EnvironmentFile=), which each host's converge
# renders from infra/estate/estate.toml before it calls this script —
# ONE spelling for every host (backlog 5222163e). Until 2026-09-18 the
# drop-in pinned it INLINE with env(1) on the Exec line, because
# deploy-services.sh wrote a shared `jobs-url.conf` drop-in pointing the
# timer fleet at `127.0.0.1` — which on boss-gcp was the LEGACY second
# stack, not the system of record (91ddebfb) — and a drop-in's
# `Environment=` outranks the unit's. EnvironmentFile= outranks any
# Environment=, drop-in or not, so the file does what the pin did. This
# matters more here than almost anywhere: a runner pointed at a wrong
# instance finds no ops-request packets, exits 0 every minute, and
# looks perfectly healthy forever — a wrong target answers instead of
# erroring (CLAUDE.md §Doors). So the address is CHECKED here, from the
# same file the unit will read, and a host without it gets no runner
# rather than a runner with no target. The empty `ExecStart=` clears
# the unit's own command before the override, which is how a drop-in
# replaces rather than appends one.
#
# AND IT ONLY CLAIMS WHAT HAPPENED. Until 2026-09-11 the last line here
# was an unconditional "boss-ops-runner installed for HOST_ID=…": with no
# `set -e`, a failed `install` or a refused `enable --now` printed its
# error and the script carried on to print that line and exit 0, so the
# caller — and the converge's packet — read success either way. Every step
# below is checked, each failure NAMES itself, and a runner that did not
# install exits non-zero, which reddens the unit and lands its packet on
# `failed`. Deliberately not the journal door's treatment: the door is
# visibility, this is the host's ability to ACT on a packet at all. It is
# also the last thing either installer does, so failing here cannot stop a
# single unit file from converging.
#
# ENV: INSTALL_ETC / INSTALL_SYSTEMCTL, the same two knobs both
# installers take, so a lint can drive this into a scratch root with a
# stub systemctl and assert what a host would actually get; and
# BOSS_NODE_ROLES, the host's live roles, which decide whether it gets a
# runner at all — see the role block below.
set -uo pipefail

# The verdict rides the run summary onto the converge's packet when the
# caller set BOSS_RUN_SUMMARY_FILE; a no-op otherwise.
# shellcheck source=infra/run-summary.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/run-summary.sh"

# shellcheck source=infra/estate/node-roles.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../estate" && pwd)/node-roles.sh"

# THE ONE PREDICATE: does a host with these roles get a runner? Read by
# the install below and, through `--in-role`, by anything that must
# agree with it without installing anything. EMPTY roles install as
# before roles existed; the `registry-unread` sentinel names no role, so
# a dark read never widens what a host runs. See the role block below
# for why the declaration is the cause.
wants_runner() {
    [ -z "${BOSS_NODE_ROLES:-}" ] || has_role ops-runner
}

# `--in-role`: exit 0 if BOSS_NODE_ROLES gets a runner, 1 if not, and
# nothing else — no root, no systemctl, no SoR, nothing written. It
# exists for infra/estate/observe-units.sh (backlog bf362f25): the
# runner is not a roles.toml row, so the observer's row-derived roster
# never watched it, and on 2026-09-22/23 boss-ops-runner went red every
# minute on a host whose observer posted a clean reading every five —
# post-mortem 3c3b202c found no estate record of it at all. The observer
# asks HERE rather than restating the predicate, so what a host installs
# and what its observer watches stay one answer (CLAUDE.md §9a).
if [ "${1:-}" = "--in-role" ]; then
    wants_runner
    exit $?
fi

HOST="${1:?usage: install-ops-runner.sh <estate node id>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
ETC="${INSTALL_ETC:-/etc/systemd/system}"
SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"

refuse() { # <what failed>
    echo "install-ops-runner: FAILED — $1" >&2
    echo "    HOST_ID=$HOST has no working ops-request runner, so a packet filed for this" >&2
    echo "    host waits at 'ready' with nothing behind it." >&2
    run_summary_field ops_runner "failed: $1"
    run_summary_note "install-ops-runner: FAILED — $1"
    exit 1
}

# WHETHER THIS HOST GETS ONE AT ALL: the `ops-runner` role decides, and
# it decides HERE (backlog cb9eb0f2, 2026-09-22). Until this line both
# callers ran this script unconditionally and the role — declared on the
# forge and boss-gcp in infra/estate/estate.toml, mapped in
# infra/estate/roles.toml — was read only by the world map. Three
# statements of "which hosts run a runner", the third added AS the
# authority and the other two not knowing it existed, so the authority
# could say one thing while the estate did another and nothing
# disagreed out loud: remove the role and the map stopped drawing a
# runner that kept running (CLAUDE.md §9a, with the extra turn). The
# declaration is now the CAUSE, read once in the one definition of how a
# host gets a runner, so neither caller carries a copy of the predicate.
#
# BOSS_NODE_ROLES is what each converge already read off the estate
# registry and exported before calling (infra/estate/node-roles.sh);
# this script never reads the registry itself, so what it installs and
# what install-units.sh installs answer to ONE read. Its conventions are
# that read's: EMPTY means no roles declared and installs as before
# roles existed, and the `registry-unread` sentinel of a dark registry
# names no role — so a failed read never WIDENS what a host runs, and a
# runner already installed keeps running until a tick that can read the
# registry converges it.
#
# SKIPPING IS EXIT 0, not a refusal. Not being an ops-runner is a
# correct outcome, and infra/gcp/install-units.sh exits with this
# script's code — a refusal here would red the whole converge of every
# host that is not one.
#
# AND SKIPPING IS NOT UNINSTALLING (backlog 1c7f8f23, the other half of
# cb9eb0f2). Drop the role from a host that already has a runner and
# this branch declines to install one — while the runner already there
# keeps running, keeps claiming ops-request packets filed for this
# host, and the estate's map, which draws the DECLARATION, shows no
# runner there at all. A second executor on one queue that nothing
# reports is this repo's recurring shape: a wrong target answers
# instead of erroring.
#
# SO THIS CONVERGE REPORTS IT AND DOES NOT REMOVE IT, deliberately.
# Removing would make the declaration authoritative in both directions
# in one step, and it is the wrong step: the runner is the DOOR every
# bounded verb arrives through, which is why infra/gcp/uninstall-not-in-
# role.sh hard-keeps `boss-ops-runner` whatever the roster says ("the
# door and the converge are never in this set") and why
# infra/lint/boss-gcp-converges-itself.sh refuses to let it become a
# roles.toml row at all. A converge that stops an executor because a
# TOML line changed is destructive-by-policy, which CLAUDE.md puts on
# the far side of the line from the mechanical work a converge may do
# — and it would take the host's only protocol door down with it,
# leaving a hand SSH as the way back. Stopping one is a deliberate
# bounded step; SAYING SO is this script's, every half hour, until
# someone acts.
#
# THE LOUDNESS IS THE POINT, and it is graded. A host that never had a
# runner records the skip as a FIELD only: that is the ordinary state
# of most of the estate, and an anomaly filed every half hour by every
# non-runner host is the permanently-warning channel nobody reads by
# the time it matters (CLAUDE.md §Diagnosis). A host that HAS one and
# no longer declares it files an ANOMALY, because the packet is what a
# reader without host access sees and a report that reaches only the
# journal is one they never see (the same reading
# infra/lint/boss-gcp-converges-itself.sh already enforces for a
# not-in-role unit).
#
# The FILE is the fact, not `systemctl list-units`: the 2026-09-15
# retire's after-snapshot showed disabled units gone from the listing
# with their files still on disk (infra/gcp/uninstall-not-in-role.sh,
# bound 5).
if ! wants_runner; then
    declared="$HOST declares $BOSS_NODE_ROLES (source: ${BOSS_NODE_ROLES_SOURCE:-preset})"
    present=""
    for ext in service timer; do
        [ -e "$ETC/boss-ops-runner.$ext" ] && present="${present:+$present }boss-ops-runner.$ext"
    done
    if [ -z "$present" ]; then
        echo "install-ops-runner: $HOST does not declare the ops-runner role (roles: $BOSS_NODE_ROLES, source: ${BOSS_NODE_ROLES_SOURCE:-preset}) — no runner installed here. An ops-request filed for this host waits at 'ready'; naming the role in infra/estate/estate.toml is what installs one on the next converge."
        run_summary_field ops_runner "not in role: $declared"
        exit 0
    fi
    echo "install-ops-runner: $HOST does not declare the ops-runner role (roles: $BOSS_NODE_ROLES, source: ${BOSS_NODE_ROLES_SOURCE:-preset}) and a runner is STILL INSTALLED here: $present (under $ETC)." >&2
    echo "    It keeps claiming the ops-request packets filed for $HOST while the estate map, which draws the" >&2
    echo "    declaration, shows no runner here — a second executor on one queue that nothing else reports." >&2
    echo "    This converge does not stop it: an executor is not something a TOML edit may take down, and it is" >&2
    echo "    this host's only protocol door. Declare ops-runner again in infra/estate/estate.toml if it should" >&2
    echo "    stay; otherwise retire it as a deliberate bounded step, naming $HOST." >&2
    run_summary_field ops_runner "not in role but still installed: $declared; present: $present"
    run_summary_note "install-ops-runner: $HOST no longer declares the ops-runner role and STILL RUNS one ($present). Not removed by this converge — it is the host's protocol door and an executor mid-claim; declare the role again or retire the runner deliberately. Roles: $BOSS_NODE_ROLES (source: ${BOSS_NODE_ROLES_SOURCE:-preset})."
    exit 0
fi

# The address the unit will read, checked from the same file — see
# above. sor_require exits by itself; `refuse` puts the verdict on the
# packet first.
# shellcheck source=infra/lib/sor.sh
. "$REPO/infra/lib/sor.sh"
[ -n "${BOSS_JOBS_URL:-}" ] \
    || refuse "no system of record: ${BOSS_SOR_ENV:-/etc/boss/sor.env} carries no BOSS_JOBS_URL (the converge renders it from infra/estate/estate.toml)"
JOBS_URL="$BOSS_JOBS_URL"

for ext in service timer; do
    src="$HERE/boss-ops-runner.$ext"
    [ -f "$src" ] || refuse "$src is missing from the tree"
    install -m 0644 "$src" "$ETC/boss-ops-runner.$ext" \
        || refuse "could not install $src to $ETC/boss-ops-runner.$ext"
done

install -d -m 0755 "$ETC/boss-ops-runner.service.d" \
    || refuse "could not create $ETC/boss-ops-runner.service.d"
printf '[Service]\nEnvironment=HOST_ID=%s\nExecStart=\nExecStart=%s/infra/ops/ops-runner.sh\n' \
    "$HOST" "$REPO" >"$ETC/boss-ops-runner.service.d/$HOST.conf" \
    || refuse "could not write the $HOST drop-in, without which the runner has no HOST_ID and refuses every tick"

"$SYSTEMCTL" daemon-reload || refuse "systemctl daemon-reload failed"
"$SYSTEMCTL" enable --now boss-ops-runner.timer \
    || refuse "systemctl enable --now boss-ops-runner.timer failed — the unit files are in place but nothing fires them"
echo "install-ops-runner: boss-ops-runner installed for HOST_ID=$HOST from $REPO, reporting to $JOBS_URL"
run_summary_field ops_runner installed
