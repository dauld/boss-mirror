#!/usr/bin/env bash
# a-dead-host-ends-its-runs.sh — an agent-run whose host pod is gone or
# terminal is ended `died` by the cluster observer on its next pass,
# from the pod state Kubernetes already holds, instead of four hours
# later by the silence clock.
#
# WHY (backlog 5d1b64b3). On 2026-09-23 two dev pod rolls, at 18:30Z and
# 18:52Z, killed eleven dispatched runs. All eleven still read
# `building` at 19:30Z and held eleven of the twelve run slots the claim
# door enforces, so dispatch was starved until the four-hour clock
# (agent-run-dies-when-building-is-silent) reached them. Every run
# records its `host` — the dev pod its builder ran on — and one of those
# pods, boss-dev-bc5b956bf-wvsrg, was still LISTED, Failed/Evicted, which
# is why "the pod object exists" is not the test: a terminal pod's
# containers never run again.
#
# The actor with both halves is the cluster observer (kubectl, a
# ServiceAccount, a signed write every fifteen minutes) — the same one
# that settles a dead gate runner (a-dead-runner-is-settled-by-its-
# observer.sh, whose extraction this reuses byte for byte). This check
# RUNS the observer's inline shell under stub `kubectl` and `curl` and
# asserts:
#
#   1. a run whose host pod is Failed (evicted) is ended: `building`
#      completed with `result = died`, the step's own metadata kept, and
#      a `host_gone` object naming the host and what was seen;
#   2. a run whose host pod is ABSENT, and whose name this namespace's
#      Deployment generates, is ended the same way;
#   3. a run on a Running pod is untouched, and so is a run whose host is
#      not a name this namespace could ever hold (a workstation) —
#      absence from boss-dev says nothing about a machine it never held;
#   4. a run with an OPEN or GREEN gate-run naming it is untouched, and
#      the gate-run is named on stdout: the runner Job is in the cluster,
#      not on the dead pod, so that verdict still ends the run. A run
#      whose only gate is red IS ended — nobody is left to repair it;
#   5. a run whose `building` already completed is untouched, and so is
#      one whose gate-run read answered NOTHING — silence is not "no gate";
#   6. BEST-EFFORT: when the pods read is refused the observation still
#      posts, the observer exits 0, nothing is written, and the refusal
#      is spoken with the server's reason;
#   7. the RBAC the read depends on is granted in the same file — a
#      namespaced Role on core pods in the dev namespace — because a read
#      the Role does not grant is denied at run time and reads as "no
#      dead hosts", the silent-absence class this exists to end.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/../cluster/manifests/boss-estate-observe.yaml"
[[ -f "$manifest" ]] || { echo "a-dead-host-ends-its-runs: missing $manifest" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "a-dead-host-ends-its-runs: no jq — the observer is sh + jq, so this check cannot run without it" >&2; exit 1; }

fail() { echo "a-dead-host-ends-its-runs: FAIL — $*" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# ----- 7: the grant, read from the same file -------------------------
grep -qE '^\s*resources:\s*\[pods\]' "$manifest" \
    || fail "no Role grants pods — the observer's read of the dev pods would be Forbidden and every dead host would read as none"

# ----- extract the observer's shell out of the block scalar -----
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

# ----- fixtures ------------------------------------------------------
cat >"$tmp/nodes.json" <<'JSON'
{"items":[
 {"metadata":{"name":"w-1","labels":{"boss.dev/purpose":"build"}},
  "status":{"addresses":[{"type":"InternalIP","address":"10.20.0.21"}],
            "capacity":{"cpu":"32","memory":"131497404Ki","ephemeral-storage":"974168604Ki"},
            "conditions":[{"type":"Ready","status":"True"}]}}
]}
JSON
printf '{"node":{"nodeName":"w-1","fs":{"availableBytes":418759086080,"capacityBytes":997807714304}}}\n' \
    >"$tmp/stats-w-1.json"
printf '{"items":[]}\n' >"$tmp/gate-jobs.json"

# The dev namespace as `kubectl get pods -n boss-dev -o json` showed it
# on 2026-09-23: the live pod, the evicted one still listed, and a gate
# runner's pod (no pod-template-hash — a Job's, not a Deployment's).
cat >"$tmp/pods.json" <<'JSON'
{"items":[
 {"metadata":{"name":"boss-dev-7889cd5f98-hnl2n","labels":{"app":"boss-dev","pod-template-hash":"7889cd5f98"}},
  "status":{"phase":"Running"}},
 {"metadata":{"name":"boss-dev-bc5b956bf-wvsrg","labels":{"app":"boss-dev","pod-template-hash":"bc5b956bf"}},
  "status":{"phase":"Failed","reason":"Evicted"}},
 {"metadata":{"name":"gate-feat-a-car-closes-ev-w5pcr-nhxrq","labels":{"app":"gate-runner","job-name":"gate-feat-a-car-closes-ev-w5pcr"}},
  "status":{"phase":"Succeeded"}}
]}
JSON

EVICTED=97574695-0000-4000-8000-000000000001
GONE=330c3f6b-0000-4000-8000-000000000002
LIVE=87a91232-0000-4000-8000-000000000003
ELSEWHERE=11111111-0000-4000-8000-000000000004
GATING=22222222-0000-4000-8000-000000000005
GREEN=33333333-0000-4000-8000-000000000006
RED=44444444-0000-4000-8000-000000000007
REPORTING=55555555-0000-4000-8000-000000000008
SILENT=66666666-0000-4000-8000-000000000009

# A run as GET /api/jobs?kind=agent-run&status=open lists it: its host,
# and the four steps that matter here. `building` carries the metadata a
# live run's step does, so the write can be checked for keeping it.
run() { # id host building-status
    printf '{"id":"%s","kind":"agent-run","status":"open","metadata":{"host":"%s","packet":"5d1b64b3"},
      "steps":[{"id":"%s-c","spec_slug":"claimed","status":"completed","metadata":{}},
               {"id":"%s-b","spec_slug":"briefed","status":"completed","metadata":{"prompt_bytes":"33171"}},
               {"id":"%s-u","spec_slug":"building","status":"%s","metadata":{"authority_role":"platform-admin"}},
               {"id":"%s-r","spec_slug":"reported","status":"%s","metadata":{"authority_role":"platform-admin"}}]}' \
        "$1" "$2" "${1:0:8}" "${1:0:8}" "${1:0:8}" "$3" "${1:0:8}" "$([[ $3 == completed ]] && echo ready || echo pending)"
}
{
    printf '{"total":9,"limit":200,"offset":0,"data":['
    run "$EVICTED"   boss-dev-bc5b956bf-wvsrg  ready;     printf ','
    run "$GONE"      boss-dev-bc5b956bf-lx2cw  ready;     printf ','
    run "$LIVE"      boss-dev-7889cd5f98-hnl2n active;    printf ','
    run "$ELSEWHERE" davids-mbp.local          ready;     printf ','
    run "$GATING"    boss-dev-859b7899cc-wf8hk ready;     printf ','
    run "$GREEN"     boss-dev-859b7899cc-wf8hk ready;     printf ','
    run "$RED"       boss-dev-859b7899cc-wf8hk ready;     printf ','
    run "$REPORTING" boss-dev-bc5b956bf-lx2cw  completed; printf ','
    run "$SILENT"    boss-dev-bc5b956bf-lx2cw  ready
    printf ']}\n'
} >"$tmp/runs.json"
[[ "$(jq -r '.data | length' "$tmp/runs.json" 2>/dev/null)" == 9 ]] || fail "the runs fixture does not parse"

gate() { # id status verdict
    printf '{"id":"%s","kind":"gate-run","status":"%s","metadata":{},
      "steps":[{"id":"%s-v","kind":"gate-verdict","status":"%s","metadata":{%s}}]}' \
        "$1" "$2" "${1:0:8}" "$([[ -n $3 ]] && echo completed || echo ready)" "$([[ -n $3 ]] && printf '"verdict":"%s"' "$3")"
}
printf '{"total":1,"data":[%s]}\n' "$(gate aaaaaaaa-0000-4000-8000-00000000000a open '')"     >"$tmp/gates-$GATING.json"
printf '{"total":1,"data":[%s]}\n' "$(gate bbbbbbbb-0000-4000-8000-00000000000b closed green)" >"$tmp/gates-$GREEN.json"
printf '{"total":1,"data":[%s]}\n' "$(gate cccccccc-0000-4000-8000-00000000000c closed failed)" >"$tmp/gates-$RED.json"
# A gate-run read that answers NOTHING — a 200 with no body. jq-1.6 reads
# silence as a pass under -e, and here that would mean "no gate owns this
# run" and end a live one (backlog d96e38ab).
: >"$tmp/gates-$SILENT.empty"

# ----- stubs -----
mkdir -p "$tmp/bin"
cat >"$tmp/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "get" && "${2:-}" == "nodes" ]]; then cat "$FIXTURES/nodes.json"; exit 0; fi
if [[ "${1:-}" == "get" && "${2:-}" == "--raw" ]]; then
    node="${3#/api/v1/nodes/}"; node="${node%%/*}"
    cat "$FIXTURES/stats-$node.json"; exit 0
fi
if [[ "${1:-}" == "get" && "${2:-}" == "jobs" ]]; then cat "$FIXTURES/gate-jobs.json"; exit 0; fi
if [[ "${1:-}" == "get" && "${2:-}" == "pods" ]]; then
    if [[ -n "${PODS_FORBIDDEN:-}" ]]; then
        echo 'Error from server (Forbidden): pods is forbidden: User "system:serviceaccount:boss:estate-observer" cannot list resource "pods" in API group "" in the namespace "boss-dev"' >&2
        exit 1
    fi
    # Scoped to the dev namespace: the grant is namespaced, and a read of
    # the observer's own namespace would find no dev pod at all.
    [[ " $* " == *" -n boss-dev "* ]] || { echo "kubectl stub: pods read is not in the dev namespace: $*" >&2; exit 98; }
    cat "$FIXTURES/pods.json"; exit 0
fi
echo "kubectl stub: unexpected args: $*" >&2
exit 99
STUB
cat >"$tmp/bin/curl" <<'STUB'
#!/usr/bin/env bash
method=GET; url=""; body=""; prev=""; want_code=""
for a in "$@"; do
    case "$prev" in
        -X) method="$a" ;;
        --data-binary) if [[ "$a" == @* ]]; then body="$(tr '\n' ' ' <"${a#@}")"; else body="${a//$'\n'/ }"; fi ;;
        -w) want_code=1 ;;
    esac
    [[ "$a" == http* ]] && url="$a"
    prev="$a"
done
printf '%s\t%s\t%s\n' "$method" "$url" "$body" >>"$LOG"
case "$method $url" in
    "POST "*/api/estate/observation) printf '{"recorded":true}\n202' ;;
    "GET "*"/api/jobs?kind=agent-run"*) cat "$FIXTURES/runs.json" ;;
    "GET "*"/api/jobs?kind=gate-run"*)
        id="${url##*agent_run%22%3A%22}"; id="${id%%%22*}"
        if [[ -f "$FIXTURES/gates-$id.empty" ]]; then :
        elif [[ -f "$FIXTURES/gates-$id.json" ]]; then cat "$FIXTURES/gates-$id.json"; else printf '{"total":0,"data":[]}'; fi ;;
    "PUT "*/steps/*) [[ -n "$want_code" ]] && printf '204' ;;
    *) printf '{"error":"stub has no answer for %s %s"}' "$method" "$url"; [[ -n "$want_code" ]] && printf '\n500' ;;
esac
exit 0
STUB
chmod +x "$tmp/bin/kubectl" "$tmp/bin/curl"

run_observer() {
    local work="$tmp/work-$1"
    mkdir -p "$work"
    : >"$tmp/log-$1"
    LOG="$tmp/log-$1" FIXTURES="$tmp" PATH="$tmp/bin:$PATH" JOBS_API="http://stub" \
        BOSS_OBSERVE_WORK="$work" BOSS_GATE_NAMESPACE="boss-dev" BOSS_DEV_NAMESPACE="boss-dev" \
        bash "$tmp/observe.sh" >"$tmp/out-$1" 2>&1
}

# ----- 1-5: the dead hosts' runs ended, nothing else touched ---------
run_observer end; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-end" >&2; fail "the observer exited $rc"; }
grep -q 'POST	http://stub/api/estate/observation' "$tmp/log-end" \
    || { cat "$tmp/out-end" >&2; fail "the node observation was not posted — ending runs must ride behind it, never replace it"; }
puts=$(grep -c '^PUT	' "$tmp/log-end")
[[ "$puts" -eq 3 ]] || { cat "$tmp/log-end" "$tmp/out-end" >&2; fail "expected exactly THREE step writes (evicted host, absent host, red-gated run), got $puts"; }
for r in "$EVICTED:boss-dev-bc5b956bf-wvsrg:Failed/Evicted" "$GONE:boss-dev-bc5b956bf-lx2cw:no longer exists" "$RED:boss-dev-859b7899cc-wf8hk:no longer exists"; do
    id="${r%%:*}"; rest="${r#*:}"; host="${rest%%:*}"; seen="${rest#*:}"
    line=$(grep "^PUT	http://stub/api/jobs/$id/steps/${id:0:8}-u	" "$tmp/log-end") \
        || { cat "$tmp/log-end" >&2; fail "run $id (host $host) was not ended on its building step"; }
    body=$(printf '%s' "$line" | cut -f3-)
    [[ "$(jq -r '.status' <<<"$body")" == completed ]] || fail "run $id: the write does not complete building (body: $body)"
    [[ "$(jq -r '.metadata.result' <<<"$body")" == died ]] || fail "run $id: the result written is not \`died\` (body: $body)"
    [[ "$(jq -r '.metadata.authority_role' <<<"$body")" == platform-admin ]] \
        || fail "run $id: the step's own metadata was not kept — PATCH-on-PUT replaces metadata wholesale (body: $body)"
    [[ "$(jq -r '.metadata.host_gone.host' <<<"$body")" == "$host" ]] || fail "run $id: host_gone does not name the host (body: $body)"
    grep -qF "$seen" <<<"$(jq -r '.metadata.host_gone.seen' <<<"$body")" \
        || fail "run $id: host_gone does not say what was seen ('$seen') — a verdict must name what failed (body: $body)"
done
for id in "$LIVE" "$ELSEWHERE" "$GATING" "$GREEN" "$REPORTING" "$SILENT"; do
    grep -q "^PUT	http://stub/api/jobs/$id/" "$tmp/log-end" \
        && { cat "$tmp/log-end" >&2; fail "run $id was written — a live host, a foreign host, a gate still owning the ending, or a finished build must be left alone"; }
done
grep -q 'aaaaaaaa-0000-4000-8000-00000000000a' "$tmp/out-end" \
    || { cat "$tmp/out-end" >&2; fail "the run left to its open gate did not name that gate-run on stdout — quiet is a loan against the next diagnosis"; }
grep -q 'building ended died' "$tmp/out-end" \
    || { cat "$tmp/out-end" >&2; fail "the ended runs were not spoken on stdout"; }

# ----- 6: a refused read costs nothing but a line --------------------
PODS_FORBIDDEN=1 run_observer refused; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-refused" >&2; fail "the observer exited $rc when the pods read was refused — a later duty took the first with it"; }
grep -q 'POST	http://stub/api/estate/observation' "$tmp/log-refused" \
    || fail "the observation was not posted when the pods read was refused"
grep -q '^PUT	' "$tmp/log-refused" && fail "a refused pods read still produced a step write"
grep -qi 'forbidden' "$tmp/out-refused" \
    || { cat "$tmp/out-refused" >&2; fail "the refused pods read was not spoken on stdout with the server's reason"; }

echo "a-dead-host-ends-its-runs: ok — runs on an evicted and an absent dev pod, and one whose only gate is red, ended died with the host named; a live host, a workstation, an open and a green gate, and a finished build untouched; a refused read costs one line"
