#!/usr/bin/env bash
# a-cluster-node-reports-its-headroom.sh — the cluster observer records
# FREE space per node, not only capacity, and losing that read costs the
# free figure alone rather than the whole observation.
#
# WHY. Measured 2026-09-10 (packet a520737f): the kubernetes-nodes
# observation carried `{disk_gb: 929}` for w-1 and no `disk_free_gb` on
# any node, so `estate.compare`'s disk floor — free below 16 GiB or
# below 35% of capacity — could never fire for cp-1, cp-2, cp-3 or w-1.
# w-1 is THE BUILD NODE: every gate's warm target lives on its nodefs
# and every gate Job prefers it by affinity. Forge disk exhaustion has
# cost this pipeline four clean cars over five departures (2026-08-22)
# and a full hold day (2026-09-05); the same failure on w-1 would have
# had no alarm at all, because the floor's numerator was never recorded.
#
# WHAT IT CHECKS, by RUNNING the observer rather than reading it. The
# observer's shell lives inline in a Kubernetes manifest — it has to,
# because the alpine/k8s image it runs in carries none of this repo's
# scripts (wiring in the shared boss-api-curl.sh once broke it with exit
# 127 and no observation landed at all). Inline shell that nothing
# executes is untested shell, so this extracts the `args:` block scalar
# and runs it under stub `kubectl` and `curl`:
#
#   1. every observed node carries a `disk_free_gb`, computed from the
#      kubelet summary API's node.fs.availableBytes;
#   2. `disk_gb` still comes from status.capacity.ephemeral-storage, so
#      the floor's numerator and denominator describe ONE filesystem —
#      the kubelet root filesystem, "nodefs";
#   3. a node whose kubelet read FAILS still appears, with
#      `disk_free_gb: null` and every other field intact, and the run
#      still posts and exits 0. The rest of the observation
#      (declared_not_observed, not_ready, and the alarm's silence sweep)
#      rides the same POST, so losing the free figure must never cost
#      them — and `estate.compare` turns that null into
#      `disk_unmeasured` so the blindness is visible rather than
#      indistinguishable from a healthy estate;
#   4. the ClusterRole grants the subresource the script actually reads.
#      That pair is a fact living twice in one file (CLAUDE.md §9a) and
#      an RBAC denial is exactly the failure this test cannot otherwise
#      see from a namespace-scoped box.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/../cluster/manifests/boss-estate-observe.yaml"
[[ -f "$manifest" ]] || { echo "a-cluster-node-reports-its-headroom: missing $manifest" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "a-cluster-node-reports-its-headroom: no jq — the observer is sh + jq, so this check cannot run without it" >&2; exit 1; }

fail() { echo "a-cluster-node-reports-its-headroom: FAIL — $*" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# ----- 4: the RBAC the script depends on, read from the same file -----
# `kubectl get --raw /api/v1/nodes/<n>/proxy/...` is authorized by the
# API server as get on the `nodes/proxy` subresource. Without the rule
# the observer is denied at run time and the free figure silently
# disappears — the state this packet reports, restored.
grep -q 'nodes/proxy' "$manifest" \
    || fail "the manifest reads a node subresource but the ClusterRole does not grant nodes/proxy — the kubelet read would be denied and every node would go back to capacity-only"

# ----- extract the observer's shell out of the block scalar -----
# Depth + exact text, never a `{n}` interval: mawk (the awk in the CI
# image) does not support intervals and silently matches nothing, which
# is a scraper that reads as "the file changed".
awk '
    { match($0, /^ */); ind = RLENGTH; body = substr($0, ind + 1) }
    ind == 14 && body == "args:"          { inargs = 1; next }
    inargs && ind == 16 && body == "- |"  { inblock = 1; inargs = 0; next }
    inblock {
        if ($0 ~ /^[ \t]*$/) { print ""; next }
        if (ind < 18) { inblock = 0; next }
        print substr($0, 19)
    }
' "$manifest" >"$tmp/observe.sh"
[[ -s "$tmp/observe.sh" ]] || fail "could not extract the args: block scalar from $manifest (indentation changed?)"
grep -q 'estate/observation' "$tmp/observe.sh" \
    || fail "the extracted block does not post an observation — the scraper found the wrong block"

# ----- fixtures: the estate as `kubectl get nodes -o json` shows it ---
# w-1's capacity is the real reading (974168604Ki → 929 GiB, the figure
# the observation carried on the day the gap was measured). cp-9 stands
# for a node whose kubelet cannot be reached.
cat >"$tmp/nodes.json" <<'JSON'
{"items":[
 {"metadata":{"name":"w-1","labels":{"boss.dev/purpose":"build"}},
  "status":{"addresses":[{"type":"InternalIP","address":"10.20.0.21"}],
            "capacity":{"cpu":"32","memory":"131497404Ki","ephemeral-storage":"974168604Ki"},
            "conditions":[{"type":"Ready","status":"True"}]}},
 {"metadata":{"name":"cp-9","labels":{"node-role.kubernetes.io/control-plane":""}},
  "status":{"addresses":[{"type":"InternalIP","address":"10.20.0.19"}],
            "capacity":{"cpu":"8","memory":"16273484Ki","ephemeral-storage":"247483648Ki"},
            "conditions":[{"type":"Ready","status":"True"}]}}
]}
JSON
# 390 GiB exactly, so the expected rounding is unambiguous.
printf '{"node":{"nodeName":"w-1","fs":{"availableBytes":418759086080,"capacityBytes":997807714304}}}\n' \
    >"$tmp/stats-w-1.json"

# ----- stubs -----
mkdir -p "$tmp/bin"
cat >"$tmp/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "get" && "${2:-}" == "nodes" ]]; then cat "$FIXTURES/nodes.json"; exit 0; fi
if [[ "${1:-}" == "get" && "${2:-}" == "--raw" ]]; then
    node="${3#/api/v1/nodes/}"; node="${node%%/*}"
    if [[ -f "$FIXTURES/stats-$node.json" ]]; then cat "$FIXTURES/stats-$node.json"; exit 0; fi
    echo "Error from server (Forbidden): nodes \"$node\" is forbidden" >&2
    exit 1
fi
echo "kubectl stub: unexpected args: $*" >&2
exit 99
STUB
cat >"$tmp/bin/curl" <<'STUB'
#!/usr/bin/env bash
prev=""
for a in "$@"; do
    [[ "$prev" == "--data-binary" ]] && printf '%s' "$(cat "${a#@}")" >"$CAPTURE"
    prev="$a"
done
printf '{"recorded":true}\n202'
STUB
chmod +x "$tmp/bin/kubectl" "$tmp/bin/curl"

export FIXTURES="$tmp" CAPTURE="$tmp/posted.json"
PATH="$tmp/bin:$PATH" JOBS_API="http://stub" bash "$tmp/observe.sh" >"$tmp/out" 2>&1
rc=$?
# ----- 3: losing one kubelet read must not cost the observation ------
[[ $rc -eq 0 ]] || { cat "$tmp/out" >&2; fail "the observer exited $rc although only ONE node's kubelet read failed — a best-effort figure took the whole observation with it"; }
[[ -s "$CAPTURE" ]] || { cat "$tmp/out" >&2; fail "nothing was posted"; }

n=$(jq '.nodes | length' "$CAPTURE")
[[ "$n" == "2" ]] || { cat "$tmp/out" >&2; fail "expected 2 observed nodes, got $n"; }

# ----- 1 + 2: the free figure, and the filesystem it describes -------
free=$(jq -r '.nodes[] | select(.id == "w-1") | .disk_free_gb' "$CAPTURE")
[[ "$free" == "390" ]] || { cat "$tmp/out" >&2; fail "w-1 reported disk_free_gb=$free, expected 390 (418759086080 bytes of node.fs, nearest GiB) — without it estate.compare's floor has no numerator and can never fire for a cluster node"; }
total=$(jq -r '.nodes[] | select(.id == "w-1") | .disk_gb' "$CAPTURE")
[[ "$total" == "929" ]] || { cat "$tmp/out" >&2; fail "w-1 reported disk_gb=$total, expected 929 (974168604Ki of ephemeral-storage, nearest GiB) — the floor's denominator must stay the same filesystem the free figure came from"; }

blind=$(jq -r '.nodes[] | select(.id == "cp-9") | .disk_free_gb' "$CAPTURE")
[[ "$blind" == "null" ]] || { cat "$tmp/out" >&2; fail "cp-9's kubelet read failed but it reported disk_free_gb=$blind — a failed read must be null, never a number and never a silent omission"; }
blind_cpu=$(jq -r '.nodes[] | select(.id == "cp-9") | .cpu' "$CAPTURE")
[[ "$blind_cpu" == "8" ]] || { cat "$tmp/out" >&2; fail "cp-9 lost its other fields (cpu=$blind_cpu) when its kubelet read failed"; }
grep -q 'cp-9' "$tmp/out" || { cat "$tmp/out" >&2; fail "the failed kubelet read was not reported on stdout — quiet is a loan against the next diagnosis (CLAUDE.md §Diagnosis)"; }

echo "a-cluster-node-reports-its-headroom: ok — the observer reports free space per node (w-1: 390 of 929 GiB, both of the kubelet's nodefs), a denied kubelet read degrades to disk_free_gb:null with the observation intact and the node named, and the ClusterRole grants the subresource the read needs"
exit 0
