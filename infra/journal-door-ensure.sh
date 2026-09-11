#!/usr/bin/env bash
# journal-door-ensure — make sure THIS host serves its own journal over
# HTTP on :19531 (systemd-journal-gatewayd), and say so either way.
#
# WHY THERE IS A DOOR AT ALL. It is the read path that still works when
# the system of record is dark, and the only one that does not need ssh:
# any unit's journal, plus anything a script logs with `| systemd-cat -t
# <tag>`, readable from the pod. Read it through
# infra/forge/journal-read.sh, never raw curl — that script asserts the
# door's FRESHNESS first, because on 2026-09-10 the forge's gateway
# served a seven-hour-stale journal while answering HTTP 200 to
# everything, and a unit-filtered query for a unit that HAD run came back
# with zero rows (packet 8bea0c9c).
#
# WHY THIS FILE EXISTS, rather than the block it was. The forge's door
# was hand-enabled on 2026-09-03 and written into the tree nowhere, so a
# rebuild would have lost it silently; infra/forge/install.sh closed that
# by enabling it on every converge tick. boss-gcp then needed the same
# thing and had nothing: backlog 68757702 measured that host with NO read
# path for either logs or unit state — `http://10.99.0.1:19531/machine`
# did not answer while the forge's returned 200 — so a failing timer
# there could only be diagnosed by a human on the box, and on 2026-09-10
# that cost a WRONG conclusion about a nightly unit, reasoned from the
# tree because the host could not be read.
#
# Two hosts needing the same six lines is where a copy would have gone,
# and the copy is what drifts (CLAUDE.md §9a). So: ONE definition, two
# callers — infra/forge/install.sh (the forge's converge) and
# infra/deploy-services.sh `units` (boss-gcp's, driven every half hour by
# infra/gcp/boss-gcp-converge.sh).
#
# NOTHING OF OURS IS SHIPPED HERE. The socket and service are the
# distro's, from the `systemd-journal-remote` package, so one definition
# means ENABLING theirs and never installing a copy that can drift from
# it. For the same reason the gateway process is deliberately given no
# periodic restart: `RuntimeMaxSec=` would record the unit as `failed`,
# and infra/estate/observe-units.sh reads ActiveState=failed as
# unhealthy — an hourly red nobody would read, which CLAUDE.md
# §Diagnosis names as the same defect as no check at all.
#
# EVERY FAILURE HERE IS NON-FATAL AND THIS SCRIPT ALWAYS EXITS 0.
# This is a visibility door, and §Diagnosis' rule is that a loop which
# can ACT must owe nothing to what it watches: a missing package, a
# package manager that will not run, a socket that will not enable —
# none of them may abort the converge that installs the units keeping
# the host alive. Each one warns in a line an operator can act on, and
# the caller carries on.
#
# Env (test hooks; on a host every one of them is the default):
#   INSTALL_SYSTEMCTL  systemctl to call
#   INSTALL_UNIT_LIB   where the distro's unit files live
#   INSTALL_APT_GET    package manager to call
#   JOURNAL_DOOR_URL   the address to name in a warning, for the operator
set -uo pipefail

SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"
UNIT_LIB="${INSTALL_UNIT_LIB:-/usr/lib/systemd/system}"
APT_GET="${INSTALL_APT_GET:-apt-get}"
DOOR_URL="${JOURNAL_DOOR_URL:-http://$(hostname -I 2>/dev/null | awk '{print $1}'):19531}"
GATEWAY_SOCKET="systemd-journal-gatewayd.socket"

if [ ! -f "${UNIT_LIB}/${GATEWAY_SOCKET}" ]; then
    echo "journal-door: ${GATEWAY_SOCKET} is absent — installing systemd-journal-remote" >&2
    if ! DEBIAN_FRONTEND=noninteractive timeout 300 "$APT_GET" install -y \
        -o DPkg::Lock::Timeout=60 --no-install-recommends systemd-journal-remote >/dev/null 2>&1; then
        echo "journal-door: could not install systemd-journal-remote — the journal read door" >&2
        echo "            (${DOOR_URL}) stays DOWN until a human runs:" >&2
        echo "              sudo apt-get install -y systemd-journal-remote" >&2
    fi
fi

if [ -f "${UNIT_LIB}/${GATEWAY_SOCKET}" ]; then
    if "$SYSTEMCTL" enable --now "$GATEWAY_SOCKET"; then
        printf '  %-24s %s\n' "journal-gateway" "$("$SYSTEMCTL" is-active "$GATEWAY_SOCKET")"
    else
        echo "journal-door: ${GATEWAY_SOCKET} would not enable — the journal read door is DOWN" >&2
    fi
else
    echo "journal-door: ${GATEWAY_SOCKET} still absent — the journal read door is DOWN" >&2
fi

exit 0
