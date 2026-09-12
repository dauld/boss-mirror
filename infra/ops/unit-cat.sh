#!/usr/bin/env bash
# unit-cat <unit> — `systemctl cat` of one unit, with any line that
# assigns a secret-shaped variable masked before it leaves the host.
#
# WHY A SCRIPT AND NOT `systemctl cat` IN THE ALLOWLIST. A unit file is
# where a hand-installed daemon's whole configuration lives — User=,
# WorkingDirectory=, ExecStart= and its flags — and that is exactly what
# declaring the daemon in the tree needs to read first (the forge's
# forgejo-runner, 2026-09-12: `daemon` with no -c, so every default,
# capacity 1 and no valid_volumes, and nothing in the tree says so).
# But Environment= lines can carry credentials, and an ops-request's
# output is a packet everyone with the queue can read. So the value of
# any Environment assignment whose NAME looks like a secret is replaced
# before printing; the name stays, so the reader knows it is set.
# Everything else is printed as systemd holds it, drop-ins included.
#
# READ-ONLY: `systemctl cat` reads unit files; nothing is written.
# exit 0 — printed; exit 4 — systemctl says the unit does not exist
# (an honest "no such unit", not an empty file); exit 1 — usage.
set -uo pipefail

ME="unit-cat"
unit="${1:-}"
case "$unit" in
    ''|*[!A-Za-z0-9@._-]*) echo "$ME: usage: unit-cat <unit> (systemd unit name)" >&2; exit 1 ;;
esac

mask() {
    # Environment=NAME=value  |  Environment="NAME=value NAME2=value2"
    # A name containing TOKEN / SECRET / PASSWORD / PASSWD / API_KEY /
    # PRIVATE_KEY / CREDENTIAL (any case) has its value replaced.
    sed -E 's/((TOKEN|SECRET|PASSWORD|PASSWD|API_KEY|PRIVATE_KEY|CREDENTIAL)[A-Za-z0-9_]*=)[^" ]*/\1<masked by unit-cat>/Ig'
}

out=$(systemctl cat --no-pager -- "$unit" 2>&1)
rc=$?
if [ "$rc" -ne 0 ]; then
    printf '%s\n' "$out" >&2
    case "$out" in
        *"could not be found"*|*"No files found"*) exit 4 ;;
        *) exit "$rc" ;;
    esac
fi
printf '%s\n' "$out" | mask
