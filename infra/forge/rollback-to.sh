#!/usr/bin/env bash
# rollback-to <sha> — roll deploy/boss to a NAMED build and verify it
# went Ready. The ops verb `rollback-to`; also runnable by hand.
#
# "Roll back" is a target, not a verb (CLAUDE.md §Diagnosis): the sha
# names an image the registry holds; the script never guesses "the
# previous one". Nothing else changes — the converge's stamp stays what
# it was, so the watchdog still knows which build was last converged,
# and main is untouched: a rollback buys time for a fix, it is not one.
set -uo pipefail
. "$(dirname "$0")/cluster-deploy-lib.sh"
sha="${1:?rollback-to needs the sha of the build to serve}"
# The converge's kubeconfig by its fixed path: this runs as root with no
# HOME under the ops runner, and as david by hand.
KUBECONFIG_PATH="${BOSS_FORGE_KUBECONFIG:-/home/david/kc.yaml}"
REGISTRY="${REGISTRY:-10.20.0.15:3000/david/boss}"
K="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
before=$($K get deploy boss -n boss -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null)
echo "rollback-to: deploy/boss serves $before — rolling to $REGISTRY:$sha"
if _patch_boss_image "$K" "$REGISTRY:$sha" && $K rollout status deploy/boss -n boss --timeout=420s; then
    echo "rollback-to: deploy/boss is Ready on $REGISTRY:$sha"
    exit 0
fi
echo "rollback-to: $REGISTRY:$sha never went Ready — deploy/boss is NOT restored; hands needed" >&2
exit 1
