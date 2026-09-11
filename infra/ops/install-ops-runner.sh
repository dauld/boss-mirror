#!/usr/bin/env bash
# install-ops-runner.sh — ONE definition of how a host gets an ops
# runner. Usage: install-ops-runner.sh <estate node id>
#
# Two hosts answer ops-request packets and both install the runner from
# here: the forge, through infra/forge/install.sh (forge-converge, every
# ten minutes), and boss-gcp, through `deploy-services.sh units` (its
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
#     checkout, with the system of record pinned INLINE by env(1)
#   * the timer enabled
#
# WHY THE SYSTEM OF RECORD IS PINNED INLINE, EVERY TIME.
# deploy-services.sh writes a shared `jobs-url.conf` drop-in pointing
# the timer fleet at `127.0.0.1` — which on boss-gcp is the LEGACY
# second stack, not the system of record (91ddebfb). A drop-in's
# `Environment=` outranks the unit's; `env(1)` on the Exec line
# outranks both, which is how the estate observers and
# boss-gcp-converge.service do it. This matters more here than almost
# anywhere: a runner pointed at the legacy instance finds no
# ops-request packets, exits 0 every minute, and looks perfectly
# healthy forever — a wrong target answers instead of erroring
# (CLAUDE.md §Doors). The empty `ExecStart=` clears the unit's own
# command before the override, which is how a drop-in replaces rather
# than appends one.
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
# stub systemctl and assert what a host would actually get.
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
# The cluster. Not a default and not a knob: see above.
JOBS_URL="http://10.20.0.34:7900"

refuse() { # <what failed>
    echo "install-ops-runner: FAILED — $1" >&2
    echo "    HOST_ID=$HOST has no working ops-request runner, so a packet filed for this" >&2
    echo "    host waits at 'ready' with nothing behind it." >&2
    run_summary_field ops_runner "failed: $1"
    run_summary_note "install-ops-runner: FAILED — $1"
    exit 1
}

for ext in service timer; do
    src="$HERE/boss-ops-runner.$ext"
    [ -f "$src" ] || refuse "$src is missing from the tree"
    install -m 0644 "$src" "$ETC/boss-ops-runner.$ext" \
        || refuse "could not install $src to $ETC/boss-ops-runner.$ext"
done

install -d -m 0755 "$ETC/boss-ops-runner.service.d" \
    || refuse "could not create $ETC/boss-ops-runner.service.d"
printf '[Service]\nEnvironment=HOST_ID=%s\nExecStart=\nExecStart=/usr/bin/env BOSS_JOBS_URL=%s %s/infra/ops/ops-runner.sh\n' \
    "$HOST" "$JOBS_URL" "$REPO" >"$ETC/boss-ops-runner.service.d/$HOST.conf" \
    || refuse "could not write the $HOST drop-in, without which the runner has no HOST_ID and refuses every tick"

"$SYSTEMCTL" daemon-reload || refuse "systemctl daemon-reload failed"
"$SYSTEMCTL" enable --now boss-ops-runner.timer \
    || refuse "systemctl enable --now boss-ops-runner.timer failed — the unit files are in place but nothing fires them"
echo "install-ops-runner: boss-ops-runner installed for HOST_ID=$HOST from $REPO, reporting to $JOBS_URL"
run_summary_field ops_runner installed
