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
# ENV: INSTALL_ETC / INSTALL_SYSTEMCTL, the same two knobs both
# installers take, so a lint can drive this into a scratch root with a
# stub systemctl and assert what a host would actually get.
set -uo pipefail

HOST="${1:?usage: install-ops-runner.sh <estate node id>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
ETC="${INSTALL_ETC:-/etc/systemd/system}"
SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"
# The cluster. Not a default and not a knob: see above.
JOBS_URL="http://10.20.0.34:7900"

for ext in service timer; do
    src="$HERE/boss-ops-runner.$ext"
    if [ ! -f "$src" ]; then
        echo "install-ops-runner: $src is missing from the tree" >&2
        exit 1
    fi
    install -m 0644 "$src" "$ETC/boss-ops-runner.$ext"
done

install -d -m 0755 "$ETC/boss-ops-runner.service.d"
printf '[Service]\nEnvironment=HOST_ID=%s\nExecStart=\nExecStart=/usr/bin/env BOSS_JOBS_URL=%s %s/infra/ops/ops-runner.sh\n' \
    "$HOST" "$JOBS_URL" "$REPO" >"$ETC/boss-ops-runner.service.d/$HOST.conf"

"$SYSTEMCTL" daemon-reload
"$SYSTEMCTL" enable --now boss-ops-runner.timer
echo "install-ops-runner: boss-ops-runner installed for HOST_ID=$HOST from $REPO, reporting to $JOBS_URL"
