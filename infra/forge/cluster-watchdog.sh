#!/usr/bin/env bash
# cluster-watchdog — the loop that knows whether the cluster is working,
# from outside it, and rolls it to the last converged build when it is
# not. Every 5 minutes (cluster-watchdog.timer), no maintenance wrap on
# purpose: a watchdog that must open a packet through the API it
# watches is the 2026-09-05 deadlock again. Its record is its journal
# (readable over the journal gateway with the API dark) and, when the
# API answers, the estate observation series through the normal path.
#
# Env: JOBS_API (the system of record), KUBECONFIG_PATH, REGISTRY,
#      BOSS_FORGE_LAST_BUILT (the converge's stamp file),
#      WATCHDOG_STATE (dark-count file), WATCHDOG_DARK_LIMIT (checks).
set -uo pipefail
. "$(dirname "$0")/cluster-watchdog-lib.sh"
. "$(dirname "$0")/alert-lib.sh"
JOBS_API="${JOBS_API:-http://10.20.0.34:7900}"
KUBECONFIG_PATH="${KUBECONFIG_PATH:-$HOME/kc.yaml}"
REGISTRY="${REGISTRY:-10.20.0.15:3000/david/boss}"
STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-$HOME/.boss-last-built}"
STATE="${WATCHDOG_STATE:-$HOME/.boss-watchdog-dark}"
LIMIT="${WATCHDOG_DARK_LIMIT:-3}"
K="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"

live_commit=$(curl -s --max-time 8 "$JOBS_API/api/jobs/health" | sed -n 's/.*"commit" *: *"\([0-9a-f]\{7,\}\)".*/\1/p' | head -n 1 | cut -c1-7)
if [ -n "$live_commit" ]; then live=up; else live=down; fi
image=$($K get deploy boss -n boss -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null | sed 's/.*://')
stamp=$(cat "$STAMP_FILE" 2>/dev/null || echo none)
dark=0
if [ "$live" = "down" ]; then dark=$(( $(cat "$STATE" 2>/dev/null || echo 0) + 1 )); fi
echo "$dark" > "$STATE"

decision=$(watchdog_decision "$live" "$image" "$stamp" "$dark" "$LIMIT")
case "$decision" in
    ok)
        echo "cluster ok: api answers on $live_commit, deployment serves $image, last converged $stamp"
        alert_replay || true
        ;;
    wait)
        echo "cluster DARK ($dark of $LIMIT checks): api silent, deployment serves $image, last converged $stamp — waiting one more check before acting" >&2
        ;;
    roll-to-stamp)
        echo "cluster DARK past $LIMIT checks on $image — rolling deploy/boss to the last converged build $REGISTRY:$stamp by name" >&2
        if $K patch deploy boss -n boss --type=json \
            -p "[{\"op\":\"replace\",\"path\":\"/spec/template/spec/containers/0/image\",\"value\":\"$REGISTRY:$stamp\"},{\"op\":\"replace\",\"path\":\"/spec/template/spec/initContainers/0/image\",\"value\":\"$REGISTRY:$stamp\"}]" \
            && $K rollout status deploy/boss -n boss --timeout=420s; then
            echo "cluster RESTORED on the last converged build $stamp — the head that was serving ($image) needs a fix on main" >&2
            echo 0 > "$STATE"
            alert "cluster restored by the watchdog: rolled to the last converged build $stamp" "The API was dark for $dark checks while the deployment served $image; the watchdog rolled deploy/boss to $REGISTRY:$stamp by name and it went Ready. The head $image needs a fix on main before the converge rolls it again."
        else
            echo "cluster STILL DARK after rolling to $stamp — hands needed" >&2
            alert "cluster DARK: hands needed — the rollback to $stamp did not restore it" "The API was dark for $dark checks; the deployment served $image; rolling to $REGISTRY:$stamp did not go Ready. Read the forge journal for cluster-deploy-runner and cluster-watchdog."
            exit 1
        fi
        ;;
    hands)
        echo "cluster DARK past $LIMIT checks on the last converged build itself ($image, stamp $stamp) — nothing this loop can roll to; hands needed" >&2
        alert "cluster DARK: hands needed — the last converged build itself is dark" "The API has been dark for $dark checks while the deployment serves $image, which is the last converged build ($stamp); nothing safe to roll to. Read the forge journal for cluster-deploy-runner and cluster-watchdog."
        exit 1
        ;;
esac
