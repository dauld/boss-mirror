#!/usr/bin/env bash
# install-ops-runner.sh — ONE definition of how a host gets an ops
# runner. Usage: install-ops-runner.sh <estate node id>
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
# shellcheck source=infra/estate/node-roles.sh
. "$REPO/infra/estate/node-roles.sh"
if [ -n "${BOSS_NODE_ROLES:-}" ] && ! has_role ops-runner; then
    echo "install-ops-runner: $HOST does not declare the ops-runner role (roles: $BOSS_NODE_ROLES, source: ${BOSS_NODE_ROLES_SOURCE:-preset}) — no runner installed here. An ops-request filed for this host waits at 'ready'; naming the role in infra/estate/estate.toml is what installs one on the next converge."
    run_summary_field ops_runner "not in role: $HOST declares $BOSS_NODE_ROLES (source: ${BOSS_NODE_ROLES_SOURCE:-preset})"
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
