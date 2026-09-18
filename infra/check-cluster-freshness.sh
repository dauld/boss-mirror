#!/usr/bin/env bash
# Cluster freshness check — is the cluster running what main says?
#
# THE INCIDENT
# ------------
# 2026-08-13. cluster-deploy-runner.sh applies the tree's manifests and
# THEN rolls the image, because boss.yaml carries a placeholder tag. A
# bug between those two steps aborted the run under `set -e`, so the
# cluster landed on the placeholder — an image several trains old — and
# stayed there. Nothing said so for hours:
#
#   * a failed systemd oneshot notifies nobody;
#   * the old image serves fine, so every health probe stayed green
#     (jobs door 200, playground 200, pod 1/1 Running);
#   * it is DRIFT, not an outage, and every alarm we had was shaped
#     for outages.
#
# It was found by accident, while reading an image tag for an unrelated
# reason. The fact was cheaply available the whole time and nobody was
# asking for it.
#
# THIS IS THE BINARY-FRESHNESS CHECK, ONE LAYER UP
# ------------------------------------------------
# check-binary-freshness.sh asked "is the deployed binary the thing we
# built" of the bare-metal fleet, until that fleet and the script left
# on 2026-09-18 (train #443, backlog ed64f852). This asks the same
# question of the cluster: is the running image the one built from the
# head of the trunk. Same shape, same reason, the one substrate left.
#
# WHAT IT PROVES, AND WHAT IT DOES NOT
# ------------------------------------
# DECISIVE: the deployment's container image tag against the trunk's
# short sha. Equal means the cluster is running main. Unequal means it
# is not, whatever the health probes say.
#
# NOT PROVEN by the default reading: that the rollout finished, or that
# a pod is actually serving that image. The deployment spec reports the
# tag it WANTS. `--strict` additionally requires a pod's running image
# to match, which is the honest reading and needs a live pod — that is
# the gap `Recreate` opened during the same day's outage, where the
# spec moved and the pod could not start.
#
# Usage: infra/check-cluster-freshness.sh [--strict] [--self-test]
# Env:   KUBECONFIG, BOSS_TRUNK_REF (default: forge/main, then origin/main)
# Exit:  0 fresh / 1 stale or unreadable / 2 self-test failure

set -uo pipefail

NS="${BOSS_CLUSTER_NS:-boss}"
DEPLOY="${BOSS_CLUSTER_DEPLOY:-boss}"

# Pure: the comparison, split out so it has a test that needs no
# cluster. Takes the deployed image ref and the trunk sha.
#
# Compared by PREFIX in both directions because the runner tags images
# with a short sha while git may hand back either length; a 7-char tag
# must match a 40-char trunk and vice versa. Both directions are
# checked so neither side's length is assumed.
verdict() {
    local deployed="$1" trunk="$2"
    if [ -z "$deployed" ] || [ -z "$trunk" ]; then
        echo "unreadable"; return
    fi
    local d="${deployed##*:}"
    if [ -z "$d" ]; then echo "unreadable"; return; fi
    if [ "${trunk#"$d"}" != "$trunk" ] || [ "${d#"$trunk"}" != "$d" ]; then
        echo "fresh"
    else
        echo "stale"
    fi
}

self_test() {
    local fails=0
    check() {
        local got want
        got=$(verdict "$2" "$3"); want="$1"
        if [ "$got" != "$want" ]; then
            echo "SELF-TEST FAIL: verdict('$2','$3') = $got, expected $want"
            fails=$((fails+1))
        fi
    }
    check fresh      "reg/david/boss:8b7fb3a"  "8b7fb3aa1234"
    check fresh      "reg/david/boss:8b7fb3aa" "8b7fb3aa"
    check stale      "reg/david/boss:b2814ef"  "8b7fb3aa1234"
    check unreadable ""                        "8b7fb3aa"
    check unreadable "reg/david/boss:8b7fb3a"  ""
    # Unrelated shas must not match on a coincidental prefix either way.
    check stale      "reg/david/boss:aaaaaaa"  "bbbbbbbb"
    if [ "$fails" -eq 0 ]; then echo "self-test: 6/6 cases correct"; return 0; fi
    return 1
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test >/dev/null || { echo "check-cluster-freshness: SELF-TEST FAILED — refusing to report"; exit 2; }

STRICT=0
[ "${1:-}" = "--strict" ] && STRICT=1

TRUNK_REF="${BOSS_TRUNK_REF:-}"
if [ -z "$TRUNK_REF" ]; then
    for r in forge/main origin/main main; do
        if git rev-parse --verify --quiet "$r" >/dev/null 2>&1; then TRUNK_REF="$r"; break; fi
    done
fi
if [ -z "$TRUNK_REF" ]; then
    echo "check-cluster-freshness: no trunk ref (tried forge/main, origin/main, main)" >&2
    exit 1
fi
TRUNK=$(git rev-parse --short "$TRUNK_REF" 2>/dev/null || true)

DEPLOYED=$(kubectl -n "$NS" get deploy "$DEPLOY" \
    -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null || true)

V=$(verdict "$DEPLOYED" "$TRUNK")
case "$V" in
    fresh)
        echo "check-cluster-freshness: ok — $NS/$DEPLOY runs $DEPLOYED, trunk $TRUNK_REF is $TRUNK"
        ;;
    stale)
        echo "check-cluster-freshness: STALE — $NS/$DEPLOY runs $DEPLOYED but $TRUNK_REF is $TRUNK" >&2
        echo "  The cluster is not running the trunk. Health probes will still be green:" >&2
        echo "  an old image serves fine. Check cluster-deploy-runner.service." >&2
        exit 1
        ;;
    *)
        echo "check-cluster-freshness: UNREADABLE — deployed='$DEPLOYED' trunk='$TRUNK'" >&2
        echo "  Cannot answer the question, which is not the same as a green answer." >&2
        exit 1
        ;;
esac

if [ "$STRICT" -eq 1 ]; then
    RUNNING=$(kubectl -n "$NS" get pods -l app="$DEPLOY" \
        -o jsonpath='{.items[0].spec.containers[0].image}' 2>/dev/null || true)
    if [ "$RUNNING" != "$DEPLOYED" ]; then
        echo "check-cluster-freshness: STRICT — the deployment wants $DEPLOYED but a pod runs $RUNNING" >&2
        echo "  The rollout has not landed. This is the gap the deployment spec cannot show." >&2
        exit 1
    fi
    echo "check-cluster-freshness: strict ok — a pod is actually running $RUNNING"
fi
