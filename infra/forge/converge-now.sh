#!/usr/bin/env bash
# converge-now — the `converge` ops verb: start ONE cluster converge
# now, and leave the runner a note saying which packet asked.
#
# Until 2026-09-08 the verb was the bare `systemctl start --no-block
# cluster-deploy-runner.service`. --no-block is right (the ops-runner
# polls every minute and must not sit on a build), but it returns 0 the
# instant systemd ACCEPTS the job — so the ops-request closed `answered`
# even when the unit then died, as it did twice on 2026-09-07 22:01
# (backlog d66f92b2). The packet said answered; the converge had not
# happened; the conductor's 30-minute alarm was the only signal.
#
# So the verb now writes the requesting packet's id (OPS_REQUEST_ID,
# which ops-runner.sh puts in the verb's environment) to an inbox in
# the checkout, `.git/boss-converge-requests`, and the runner takes that
# inbox when it starts and PATCHes each request with how the run ended
# (`converged: <sha>`, `converge_held: <reason>`, or `converge_failed:
# <stage> (exit N)`) — see cluster-deploy-lib.sh answer_converge_requests.
# A request that started a run which then failed reads as failed on the
# packet that asked, with the reason, not as a clean `answered`.
#
# Runs as root under the ops-runner (no HOME); the inbox is in the
# checkout owner's .git, world-readable, so the runner (the owner) can
# read and remove it. A request that arrives while a run is already
# active is taken by the NEXT run, which then answers it honestly
# (usually `converged: <sha>` — main unchanged, the cluster is on it).
set -euo pipefail

REPO="${BOSS_FORGE_REPO_DIR:-/home/david/boss}"
INBOX="$REPO/.git/boss-converge-requests"

if [ -n "${OPS_REQUEST_ID:-}" ]; then
    case "$OPS_REQUEST_ID" in
        *[!0-9a-fA-F-]* | "")
            echo "converge-now: OPS_REQUEST_ID '$OPS_REQUEST_ID' is not a job id — starting the converge without a note" >&2 ;;
        *)
            if ( umask 022; printf '%s\n' "$OPS_REQUEST_ID" >> "$INBOX" ) 2>/dev/null; then
                chmod 0644 "$INBOX" 2>/dev/null || true
                echo "converge-now: noted request ${OPS_REQUEST_ID:0:8} in $INBOX — the runner answers it when the run ends"
            else
                echo "converge-now: could not write $INBOX — the run's outcome will reach only its maintenance packet" >&2
            fi ;;
    esac
else
    echo "converge-now: no OPS_REQUEST_ID in the environment — starting the converge without a note"
fi

"${BOSS_SYSTEMCTL:-systemctl}" start --no-block cluster-deploy-runner.service
echo "converge-now: cluster-deploy-runner.service started (--no-block); its verdict lands on the maintenance-cluster-converge packet and on this request"
