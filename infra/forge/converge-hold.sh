#!/usr/bin/env bash
# converge-hold hold <reason> | release — an operator's hand on the
# converge, as a file the runner reads before it rolls anything.
#
# While a hold stands, cluster-deploy-runner builds nothing and rolls
# nothing; it says so on every tick and exits 0 (a hold is a decision,
# not a failure). The ops verbs `hold-converge` / `release-converge`
# call this; the reason rides on the packet and in the file.
set -uo pipefail
HOLD_FILE="${BOSS_CONVERGE_HOLD:-$HOME/.boss-converge-hold}"
case "${1:-}" in
    hold)
        reason="${2:?hold needs a reason}"
        printf '%s\n' "$reason" > "$HOLD_FILE"
        echo "converge-hold: HELD — $reason ($HOLD_FILE)"
        ;;
    release)
        if [ -f "$HOLD_FILE" ]; then
            echo "converge-hold: released (was: $(cat "$HOLD_FILE"))"; rm -f "$HOLD_FILE"
        else
            echo "converge-hold: no hold was standing"
        fi
        ;;
    *) echo "usage: converge-hold.sh hold <reason> | release" >&2; exit 2 ;;
esac
