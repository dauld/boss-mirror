#!/usr/bin/env bash
# an-eviction-is-recorded-where-the-record-can-read-it.sh — every pod the
# kubelet evicts in the dev namespace is recorded once, as data the jobs
# API serves, by the cluster observer's next pass.
#
# WHY (backlog 48d8a3f3). Car 6b23d135 (fix/dev-pod-not-first-evicted)
# claims the next ephemeral-storage pressure on w-1 evicts a gate pod and
# not boss-dev. The priority half was measurable; the eviction half was
# not, from anywhere a proof can run: a kubelet eviction leaves an
# `Evicted` Event that Kubernetes keeps for an hour and a Failed pod a
# reaper removes, and neither the forge (no kubeconfig) nor the jobs API
# can read either. So no `--seen` check could observe the event, and the
# car sat `ours` in the shed with nothing that could ever change that.
#
# The actor with both halves is the cluster observer (kubectl, a
# ServiceAccount, a signed write every fifteen minutes) — the one that
# already carries a dead gate runner and a dead dev pod across. This
# check RUNS the observer's inline shell under stub `kubectl` and `curl`
# (the extraction is a-dead-host-ends-its-runs.sh's, byte for byte) and
# asserts:
#
#   1. each eviction the dev namespace shows — an `Evicted` Event, or a
#      pod whose status says Evicted after its Event has expired — is
#      POSTed to /api/estate/observation under scope
#      `kubernetes-evictions`, grouped under the node it happened on, with
#      pod, namespace, node, reason, message, priority class, priority
#      and time; priority read off the pod object, null (never guessed)
#      when the pod is already gone;
#   2. an eviction the record already holds (its key in a recorded
#      `kubernetes-evictions` observation) is not posted again, and an
#      Event that is not `Evicted` is not an eviction;
#   3. when every eviction listed is already recorded, nothing is posted
#      and that is said;
#   4. BEST-EFFORT: a refused events read costs one spoken line — the node
#      observation still posts, the observer exits 0, and an evicted pod
#      still listed is still recorded from the pod object;
#   5. when what is already recorded cannot be read, nothing is posted
#      and the reason is spoken — a duplicate is not guessed at, and the
#      next pass (the Event lives an hour, the pod longer) tries again;
#   6. the RBAC the read depends on is granted in the same file — a
#      namespaced Role on core events in the dev namespace — and every
#      verb the observer holds, in any rule, is a read.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/../cluster/manifests/boss-estate-observe.yaml"
name=an-eviction-is-recorded-where-the-record-can-read-it
[[ -f "$manifest" ]] || { echo "$name: missing $manifest" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "$name: no jq — the observer is sh + jq, so this check cannot run without it" >&2; exit 1; }

fail() { echo "$name: FAIL — $*" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# ----- 6: the grant, read from the same file -------------------------
# Split the manifest into its YAML documents and find the one granting
# events: it must be a namespaced Role in the dev namespace, never a
# ClusterRole — an observer that can read every namespace's events can
# read what every workload says about itself.
awk -v dir="$tmp" 'BEGIN { n = 0 } /^---/ { n++; next } { print > (dir "/doc-" n ".yaml") }' "$manifest"
events_doc=$(grep -lE '^\s*resources:\s*\[events\]' "$tmp"/doc-*.yaml | sed -n 1p)
[[ -n "$events_doc" ]] \
    || fail "no rule grants events — the observer's read of the Evicted events would be Forbidden and every eviction would read as none"
grep -qE '^kind:\s*Role$' "$events_doc" \
    || fail "the events grant is not a namespaced Role ($(grep -E '^kind:' "$events_doc")) — scope it to the dev namespace"
grep -qE '^\s*namespace:\s*boss-dev$' "$events_doc" \
    || fail "the events Role is not in the boss-dev namespace"
grep -E '^\s*verbs:' "$manifest" | while IFS= read -r line; do
    verbs=$(printf '%s' "$line" | sed -E 's/^\s*verbs:\s*\[(.*)\]\s*$/\1/' | tr -d ' ')
    for v in $(printf '%s' "$verbs" | tr ',' ' '); do
        case "$v" in
            get|list|watch) ;;
            *) echo "$v"; ;;
        esac
    done
done >"$tmp/write-verbs"
[[ ! -s "$tmp/write-verbs" ]] \
    || fail "the observer's RBAC grants a verb that is not a read: $(tr '\n' ' ' <"$tmp/write-verbs")— an observer that can change what it watches is a different kind of thing"

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
printf '{"total":0,"limit":200,"offset":0,"data":[]}\n' >"$tmp/runs.json"

# The dev namespace: the live dev pod at its PriorityClass, a gate pod
# the kubelet evicted (its Event is below), and an older dev pod evicted
# before the Events window — still listed, its Event long expired, which
# is the shape boss-dev-bc5b956bf-wvsrg had on 2026-09-23.
cat >"$tmp/pods.json" <<'JSON'
{"items":[
 {"metadata":{"name":"boss-dev-7889cd5f98-hnl2n","namespace":"boss-dev","uid":"uid-live",
              "labels":{"app":"boss-dev","pod-template-hash":"7889cd5f98"}},
  "spec":{"nodeName":"w-1","priorityClassName":"boss-dev-session","priority":1000},
  "status":{"phase":"Running"}},
 {"metadata":{"name":"gate-fix-a-car-xk2lp-q8z7w","namespace":"boss-dev","uid":"uid-gate",
              "labels":{"app":"gate-runner","job-name":"gate-fix-a-car-xk2lp"}},
  "spec":{"nodeName":"w-1","priority":0},
  "status":{"phase":"Failed","reason":"Evicted",
            "message":"The node was low on resource: ephemeral-storage. Threshold quantity: 97416860Ki, available: 91233012Ki.",
            "conditions":[{"type":"DisruptionTarget","status":"True","reason":"TerminationByKubelet",
                           "lastTransitionTime":"2026-09-25T03:10:02Z"}]}},
 {"metadata":{"name":"boss-dev-bc5b956bf-wvsrg","namespace":"boss-dev","uid":"uid-old-dev",
              "labels":{"app":"boss-dev","pod-template-hash":"bc5b956bf"}},
  "spec":{"nodeName":"w-1","priority":0},
  "status":{"phase":"Failed","reason":"Evicted",
            "message":"The node was low on resource: ephemeral-storage. Container dev was using 88Gi, request is 0.",
            "conditions":[{"type":"DisruptionTarget","status":"True","reason":"TerminationByKubelet",
                           "lastTransitionTime":"2026-09-23T14:02:11Z"}]}}
]}
JSON

# Events as `kubectl get events -n boss-dev -o json` lists them: the gate
# pod's eviction; one for a pod whose object is already gone (its Job was
# reaped); one the record already holds; and an Event that is not an
# eviction at all.
cat >"$tmp/events.json" <<'JSON'
{"items":[
 {"metadata":{"name":"gate-fix-a-car-xk2lp-q8z7w.1a2b","namespace":"boss-dev","uid":"ev-1"},
  "involvedObject":{"kind":"Pod","name":"gate-fix-a-car-xk2lp-q8z7w","namespace":"boss-dev","uid":"uid-gate"},
  "reason":"Evicted","type":"Warning","source":{"component":"kubelet","host":"w-1"},
  "message":"The node was low on resource: ephemeral-storage. Threshold quantity: 97416860Ki, available: 91233012Ki.",
  "firstTimestamp":"2026-09-25T03:10:01Z","lastTimestamp":"2026-09-25T03:10:01Z"},
 {"metadata":{"name":"gate-feat-gone-p4n2v-7hh2k.3c4d","namespace":"boss-dev","uid":"ev-2"},
  "involvedObject":{"kind":"Pod","name":"gate-feat-gone-p4n2v-7hh2k","namespace":"boss-dev","uid":"uid-gone"},
  "reason":"Evicted","type":"Warning","source":{"component":"kubelet","host":"w-1"},
  "message":"The node was low on resource: ephemeral-storage.",
  "firstTimestamp":null,"lastTimestamp":null,"eventTime":"2026-09-25T02:55:40.123456Z"},
 {"metadata":{"name":"gate-old-recorded-aaaaa-bbbbb.5e6f","namespace":"boss-dev","uid":"ev-3"},
  "involvedObject":{"kind":"Pod","name":"gate-old-recorded-aaaaa-bbbbb","namespace":"boss-dev","uid":"uid-recorded"},
  "reason":"Evicted","type":"Warning","source":{"component":"kubelet","host":"w-1"},
  "message":"The node was low on resource: ephemeral-storage.",
  "firstTimestamp":"2026-09-25T02:40:00Z","lastTimestamp":"2026-09-25T02:40:00Z"},
 {"metadata":{"name":"boss-dev-7889cd5f98-hnl2n.7a8b","namespace":"boss-dev","uid":"ev-4"},
  "involvedObject":{"kind":"Pod","name":"boss-dev-7889cd5f98-hnl2n","namespace":"boss-dev","uid":"uid-live"},
  "reason":"Killing","type":"Normal","source":{"component":"kubelet","host":"w-1"},
  "message":"Stopping container dev",
  "firstTimestamp":"2026-09-25T02:00:00Z","lastTimestamp":"2026-09-25T02:00:00Z"}
]}
JSON

# What the record already holds, as GET /api/estate/observations serves
# it: one earlier kubernetes-evictions observation carrying uid-recorded.
recorded() { # keys...
    local evs="" k
    for k in "$@"; do evs="$evs${evs:+,}{\"key\":\"$k\",\"pod\":\"p\",\"namespace\":\"boss-dev\",\"node\":\"w-1\",\"reason\":\"Evicted\"}"; done
    printf '{"total":1,"data":[{"kind":"jobs.estate.observed","timestamp":"2026-09-25T02:45:00Z","payload":{"observed_at":"2026-09-25T02:45:00Z","observer":"boss-estate-observe","scope":"kubernetes-evictions","nodes":[{"id":"w-1","evictions":[%s]}]}}]}\n' "$evs"
}
recorded uid-recorded >"$tmp/recorded-some.json"
recorded uid-recorded uid-gate uid-gone uid-old-dev >"$tmp/recorded-all.json"

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
    [[ " $* " == *" -n boss-dev "* ]] || { echo "kubectl stub: pods read is not in the dev namespace: $*" >&2; exit 98; }
    cat "$FIXTURES/pods.json"; exit 0
fi
if [[ "${1:-}" == "get" && "${2:-}" == "events" ]]; then
    if [[ -n "${EVENTS_FORBIDDEN:-}" ]]; then
        echo 'Error from server (Forbidden): events is forbidden: User "system:serviceaccount:boss:estate-observer" cannot list resource "events" in API group "" in the namespace "boss-dev"' >&2
        exit 1
    fi
    # Namespaced, like the grant: a read anywhere else would be refused.
    [[ " $* " == *" -n boss-dev "* ]] || { echo "kubectl stub: events read is not in the dev namespace: $*" >&2; exit 98; }
    # The stub ignores any field selector and answers every Event, so the
    # observer's own filter is what keeps a non-eviction out.
    cat "$FIXTURES/events.json"; exit 0
fi
echo "kubectl stub: unexpected args: $*" >&2
exit 99
STUB
cat >"$tmp/bin/curl" <<'STUB'
#!/usr/bin/env bash
method=GET; url=""; body=""; prev=""; want_code=""; discard=""
for a in "$@"; do
    case "$prev" in
        -X) method="$a" ;;
        --data-binary) if [[ "$a" == @* ]]; then body="$(tr '\n' ' ' <"${a#@}")"; else body="${a//$'\n'/ }"; fi ;;
        -w) want_code=1 ;;
        -o) discard=1 ;;
    esac
    [[ "$a" == http* ]] && url="$a"
    prev="$a"
done
printf '%s\t%s\t%s\n' "$method" "$url" "$body" >>"$LOG"
case "$method $url" in
    "POST "*/api/estate/observation) if [[ -n "$discard" ]]; then printf '202'; else printf '{"recorded":true}\n202'; fi ;;
    "GET "*"/api/estate/observations?scope=kubernetes-evictions"*)
        if [[ -n "${RECORDED_FAILS:-}" ]]; then echo "curl: (22) The requested URL returned error: 500" >&2; exit 22; fi
        cat "$FIXTURES/$RECORDED" ;;
    "GET "*"/api/jobs?kind=agent-run"*) cat "$FIXTURES/runs.json" ;;
    *) printf '{"error":"stub has no answer for %s %s"}' "$method" "$url"; [[ -n "$want_code" ]] && printf '\n500' ;;
esac
exit 0
STUB
chmod +x "$tmp/bin/kubectl" "$tmp/bin/curl"

run_observer() { # tag recorded-fixture
    local work="$tmp/work-$1"
    mkdir -p "$work"
    : >"$tmp/log-$1"
    LOG="$tmp/log-$1" FIXTURES="$tmp" RECORDED="$2" PATH="$tmp/bin:$PATH" JOBS_API="http://stub" \
        BOSS_OBSERVE_WORK="$work" BOSS_GATE_NAMESPACE="boss-dev" BOSS_DEV_NAMESPACE="boss-dev" \
        bash "$tmp/observe.sh" >"$tmp/out-$1" 2>&1
}
# The evictions POST, as the body the observer sent.
evictions_posted() { # tag
    grep '^POST	http://stub/api/estate/observation	' "$tmp/log-$1" | cut -f3- \
        | jq -c 'select(.scope == "kubernetes-evictions")'
}

# ----- 1-2: the new evictions recorded, once, with what they were -----
run_observer new recorded-some.json; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-new" >&2; fail "the observer exited $rc"; }
first=$(grep -m1 '^POST	' "$tmp/log-new" | cut -f3- | jq -r '.scope' 2>/dev/null)
[[ "$first" == kubernetes-nodes ]] \
    || { cat "$tmp/log-new" >&2; fail "the first POST was scope '$first' — the node observation is the observer's first duty and evictions must ride behind it, never replace it"; }
grep -q '^GET	http://stub/api/estate/observations?scope=kubernetes-evictions' "$tmp/log-new" \
    || { cat "$tmp/log-new" >&2; fail "the observer never read what the record already holds — every pass would re-record the same eviction"; }
posted=$(evictions_posted new)
[[ $(printf '%s\n' "$posted" | grep -c .) -eq 1 ]] \
    || { cat "$tmp/log-new" "$tmp/out-new" >&2; fail "expected exactly ONE kubernetes-evictions POST, got: $posted"; }
printf '%s' "$posted" >"$tmp/posted.json"
[[ "$(jq -r '.observer' "$tmp/posted.json")" == boss-estate-observe ]] || fail "the evictions observation does not name its observer"
[[ "$(jq -r '.observed_at | type' "$tmp/posted.json")" == string ]] || fail "the evictions observation carries no observed_at"
[[ "$(jq -r '[.nodes[].id] | join(",")' "$tmp/posted.json")" == w-1 ]] \
    || fail "the evictions are not grouped under the node they happened on: $(jq -c '[.nodes[].id]' "$tmp/posted.json")"
keys=$(jq -r '[.nodes[].evictions[].key] | sort | join(",")' "$tmp/posted.json")
[[ "$keys" == "uid-gate,uid-gone,uid-old-dev" ]] \
    || fail "recorded keys '$keys', expected uid-gate,uid-gone,uid-old-dev — one already recorded must not repeat, a Killing event is not an eviction, and a listed Evicted pod whose Event expired still counts"
ev() { jq -c --arg k "$1" '.nodes[].evictions[] | select(.key == $k)' "$tmp/posted.json"; }
want() { # key field expected
    local got; got=$(ev "$1" | jq -r --arg f "$2" '.[$f] | if . == null then "null" else tostring end')
    [[ "$got" == "$3" ]] || fail "eviction $1: $2 is '$got', expected '$3' (record: $(ev "$1"))"
}
want uid-gate pod gate-fix-a-car-xk2lp-q8z7w
want uid-gate namespace boss-dev
want uid-gate node w-1
want uid-gate reason Evicted
want uid-gate priority 0
want uid-gate priority_class null
want uid-gate at 2026-09-25T03:10:01Z
want uid-gate source event
grep -q 'ephemeral-storage' <<<"$(ev uid-gate | jq -r '.message')" \
    || fail "eviction uid-gate does not carry the kubelet's message — the resource it was evicted for is the whole claim"
# The pod is gone: its priority is unknown, and says so.
want uid-gone pod gate-feat-gone-p4n2v-7hh2k
want uid-gone priority null
want uid-gone priority_class null
want uid-gone at 2026-09-25T02:55:40.123456Z
# Its Event expired; the pod object still says what happened and when.
want uid-old-dev pod boss-dev-bc5b956bf-wvsrg
want uid-old-dev node w-1
want uid-old-dev source pod
want uid-old-dev at 2026-09-23T14:02:11Z
grep -q 'ephemeral-storage' <<<"$(ev uid-old-dev | jq -r '.message')" \
    || fail "eviction uid-old-dev (pod-sourced) does not carry the pod's status message"
grep -q 'evictions: 3 new in boss-dev recorded -> 202' "$tmp/out-new" \
    || { cat "$tmp/out-new" >&2; fail "the recorded evictions, and the door's answer, were not spoken on stdout"; }
grep -q 'evicted: boss-dev/gate-fix-a-car-xk2lp-q8z7w on w-1' "$tmp/out-new" \
    || { cat "$tmp/out-new" >&2; fail "each eviction was not named on stdout — whoever is next in front of a disk failure should read who went, not re-derive it"; }

# ----- 3: nothing new, nothing posted --------------------------------
run_observer none recorded-all.json; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-none" >&2; fail "the observer exited $rc with every eviction already recorded"; }
[[ -z "$(evictions_posted none)" ]] || fail "evictions the record already holds were posted again: $(evictions_posted none)"
grep -q 'already recorded' "$tmp/out-none" \
    || { cat "$tmp/out-none" >&2; fail "nothing-new was not said — silence reads the same as an observer that never looked"; }

# ----- 4: a refused events read costs one line -----------------------
EVENTS_FORBIDDEN=1 run_observer refused recorded-some.json; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-refused" >&2; fail "the observer exited $rc when the events read was refused — a later duty took the first with it"; }
[[ "$(grep -m1 '^POST	' "$tmp/log-refused" | cut -f3- | jq -r '.scope' 2>/dev/null)" == kubernetes-nodes ]] \
    || fail "the node observation was not posted when the events read was refused"
grep -qi 'forbidden' "$tmp/out-refused" \
    || { cat "$tmp/out-refused" >&2; fail "the refused events read was not spoken with the server's reason"; }
refused_keys=$(evictions_posted refused | jq -r '[.nodes[].evictions[].key] | sort | join(",")')
[[ "$refused_keys" == "uid-gate,uid-old-dev" ]] \
    || fail "with the events unread the pod objects still say who was evicted; recorded '$refused_keys', expected uid-gate,uid-old-dev"

# ----- 5: the record unreadable, nothing guessed ---------------------
RECORDED_FAILS=1 run_observer blind recorded-some.json; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-blind" >&2; fail "the observer exited $rc when the recorded evictions could not be read"; }
[[ -z "$(evictions_posted blind)" ]] \
    || fail "evictions were posted without knowing what was already recorded — a duplicate guessed at: $(evictions_posted blind)"
grep -q 'not recorded this pass' "$tmp/out-blind" \
    || { cat "$tmp/out-blind" >&2; fail "the skipped record was not spoken"; }

echo "$name: ok — a new eviction recorded once under its node with pod, priority, reason and time; a recorded one, a non-eviction and an already-recorded pass post nothing; a refused events read and an unreadable record each cost one spoken line"
