#!/usr/bin/env bash
# reap-terminated-pods — delete the Failed pods nothing else collects,
# through a rendered plan.
#
#   reap-terminated-pods.sh --plan
#   reap-terminated-pods.sh <plan-sha256>
#
# WHY IT EXISTS (backlog 85889a52). Measured 2026-09-23 18:36Z after
# train #587 rolled the dev pod: six terminated boss-dev pods (Error /
# ContainerStatusUnknown, 8h to 8d old, all on w-1) were still listed,
# every one left behind by an ephemeral-storage eviction — six in eight
# days (the_dev_pod_is_not_the_first_evicted.rs). Kubernetes does not
# collect them: the controller-manager's terminated-pod-gc-threshold is
# 12500. The dev session cannot delete pods, correctly, and
# undeclared-objects.sh leaves Pod out of scope on purpose, so the
# conformance sweep and delete-orphan-object never see them. A human
# with admin credentials was the only way to clear them. David,
# 2026-09-23: run it on a period regardless, and have an ad hoc trigger.
#
# REJECTED: setting --terminated-pod-gc-threshold on the
# controller-manager. It deletes by COUNT, cluster-wide and silently,
# with no packet and no capture — and a failed pod's status.reason and
# status.message ("Evicted: ... ephemeral-storage ...") are the only
# record of why it died.
#
# THE SHAPE is design 17835005's (David, 2026-09-21: "a rendered plan
# hash, signed with his passkey, single-use, verified before the argv is
# built"), third caller after commission-a-disk and merge-tenant-main.
# `plan-a-pod-reap` runs --plan; `reap-terminated-pods` runs the write,
# declares requires_approval, and is refused by the ops runner until the
# approval channel can verify one — the gate lands before the power.
#
# THE SCOPE IS DERIVED, NEVER PASSED. A pod is in the plan when ALL hold:
#   * it is in a namespace the tree OWNS — `undeclared-objects.sh
#     --namespaces`, the one definition of that set (a Namespace object
#     under infra/cluster/manifests), never a hand list and never a
#     parameter;
#   * its status.phase is Failed. Running and Pending are alive;
#     Succeeded pods belong to their CronJob / Job history limits;
#   * no Job owns it. A Job's failed pods are that Job's own retention
#     (backoffLimit, failedJobsHistoryLimit, ttlSecondsAfterFinished) and
#     go when the Job goes; deleting one under a live Job races the Job
#     controller's tracking finalizer. They are named on stderr, not
#     reaped;
#   * it is not already being deleted; and
#   * it FAILED more than WINDOW_S before the render, so a pod that
#     failed a minute ago is still there for whoever is looking at it.
#     "Failed at" is the latest time the pod records — creation, any
#     condition transition, any container finish — because creation
#     alone says nothing: boss-dev-bc5b956bf-wvsrg was created 10:08 and
#     evicted 17:42 on 2026-09-23, and its predecessors lived for days.
#
# CAPTURE BEFORE DELETE. The plan carries each pod's namespace, name,
# uid, node, creationTimestamp, when it failed, status.reason,
# status.message and every container's terminated reason and exit
# code — the diagnosis — and the
# write PRINTS THE PLAN before its first delete, so the reaped eviction
# can be read back off the packet afterwards.
#
# THE BYTES ARE DETERMINISTIC. No clock, no age, no run id, no scratch
# path; pods sorted by namespace then name. Two renders of one true
# state are byte-identical, and the sha256 of the bytes goes on stderr
# as `plan-sha256: <hex>` (a hash cannot be inside what it hashes).
#
# THE CLOCK AT THE BOUNDARY, stated rather than hoped about. The window
# makes WHICH pods are selected depend on when the render runs, even
# though no time is in the bytes. A pod that was 50 minutes old at plan
# time is not in the signed plan; at apply time it may be 70 minutes
# old. It is SAFE, because the write never deletes "what matches now":
# it deletes only the set whose rendering hashes to the approved hash.
# Candidates are ordered by when they FAILED, so the signed set is
# always one of the failure-time prefixes of today's candidates: a pod
# that had failed before the plan's cutoff was Failed when the plan was
# rendered (and was in it), a terminal phase does not change, and a pod
# that failed LATER — however old it is — sorts after that cutoff. The
# write re-renders each prefix, from none to all, and acts on the one
# that hashes to what was approved. The pod that crossed the boundary,
# or died while the plan waited for its passkey, is outside that
# prefix, so it is not deleted — it is tomorrow's plan. (Keyed on
# creation instead, a days-old dev pod evicted during the wait would sit
# inside every prefix and void the plan: the dev pod is evicted about
# daily, so a plan left a day for its tap would rarely still apply.)
# If NO prefix matches — a planned pod is gone, was replaced under its
# name, or its recorded facts moved — that is a refusal, exit 78,
# naming both hashes, and nothing is deleted. That is also what makes
# the write AT MOST ONCE: a second run of an applied plan finds its pods
# gone and refuses, so no approval is spent twice.
#
# EACH DELETE CARRIES A UID PRECONDITION. The re-render proves the plan
# still holds; a same-named pod created in the seconds between the
# re-render and the delete (a StatefulSet's ordinal pod comes back under
# the same name) would still be a different object. So the delete is the
# API's own DeleteOptions.preconditions.uid, sent through `kubectl delete
# --raw` with the body on stdin, and the API server refuses a mismatch
# with 409 — the check is at the server, not a read-then-delete here.
# Then every pod is read back: gone, or its name held by a different
# uid, is reaped; the same uid still present is a failure. (A pod in a
# terminal phase is deleted with no grace period by the API server's
# own pod strategy, so there is nothing to wait out.)
#
# ENV (test seams; the ops runner passes a verb no environment from the
# packet, only an argv built from the allowlist): BOSS_CLUSTER_TREE and
# BOSS_KUBECTL, read by undeclared-objects.sh; BOSS_REAP_NOW, the render
# time as epoch seconds (default: the clock).
#
# Exit 78 is a refusal (the request was wrong or no longer true); exit 1
# is a step that genuinely failed, including a read that could not look.
set -uo pipefail

ME=reap-terminated-pods
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DERIVE="$HERE/../cluster/undeclared-objects.sh"
# One hour: long enough that a pod which just failed is still there for
# whoever is diagnosing it, short enough that a daily plan sees the rest.
WINDOW_S=3600

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
say() { echo "$ME: $*" >&2; }

PLAN=0
if [ "${1-}" = "--plan" ]; then PLAN=1; shift; fi
if [ "$PLAN" -eq 1 ]; then
    [ $# -eq 0 ] || refuse "usage: $ME --plan — the scope is derived from the tree, so the plan takes no argument"
else
    [ $# -eq 1 ] || refuse "usage: $ME <plan-sha256> — this deletes only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-pod-reap verb)"
    [[ "$1" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$1'"
    APPROVED="$1"
fi

command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — the pods cannot be read, and no evidence is not a plan"
NOW="${BOSS_REAP_NOW:-$(date -u +%s)}"
case "${NOW:-empty}" in
    empty | *[!0-9]*) fail "the render time '$NOW' is not epoch seconds" ;;
esac
CUTOFF=$((NOW - WINDOW_S))

WORK=$(mktemp -d) || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- the scope: the namespaces the tree owns -----------------------------
if ! "$DERIVE" --namespaces > "$WORK/ns" 2> "$WORK/ns.err"; then
    sed 's/^/    /' "$WORK/ns.err" >&2
    fail "undeclared-objects.sh could not say which namespaces the tree owns (above) — no scope is not an empty scope"
fi
mapfile -t NAMESPACES < "$WORK/ns"
[ "${#NAMESPACES[@]}" -gt 0 ] || fail "the tree owns no namespace, by the derivation's own answer — refusing to read that as nothing to reap"

KUBECTL_LINE=$("$DERIVE" --kubectl) || fail "no kubectl to read the pods with (above)"
read -r -a KUBECTL <<< "$KUBECTL_LINE"

# --- the pods, one record each --------------------------------------------
# The namespace comes from the loop, not the item: the list was asked of
# that namespace, and the sort key must not depend on a field the server
# might omit.
: > "$WORK/pods.jsonl"
for ns in "${NAMESPACES[@]}"; do
    if ! "${KUBECTL[@]}" get pods -n "$ns" -o json --request-timeout=30s \
            > "$WORK/list.json" 2> "$WORK/list.err"; then
        sed 's/^/    /' "$WORK/list.err" >&2
        fail "could not list pods in $ns (kubectl's words above) — a namespace nobody read is not an empty one, so there is no plan"
    fi
    if ! jq -c --arg ns "$ns" '
        .items[] | {
            ns: $ns,
            name: .metadata.name,
            uid: .metadata.uid,
            created: .metadata.creationTimestamp,
            # WHEN IT FAILED, as the latest time the pod itself records:
            # creation, every condition transition, every container
            # finish. For a Failed pod that is its failure (the Ready
            # condition turning False with PodFailed, on the evictions of
            # 2026-09-23). Never creation alone: a dev pod lives for days
            # before it is evicted.
            failed: (
                [.metadata.creationTimestamp,
                 ((.status.conditions // [])[] | .lastTransitionTime // empty),
                 ((.status.initContainerStatuses // [])[], (.status.containerStatuses // [])[]
                  | .state.terminated.finishedAt // empty)]
                | map({t: ., e: fromdateiso8601}) | max_by(.e)),
            phase: (.status.phase // ""),
            deleting: (.metadata.deletionTimestamp != null),
            job: ([.metadata.ownerReferences[]? | select(.kind == "Job")] | length > 0),
            node: (.spec.nodeName // "none"),
            reason: (.status.reason // "none"),
            message: ((.status.message // "none") | gsub("[\\r\\n\\t]+"; " ")),
            containers: [
                (.status.initContainerStatuses // [])[], (.status.containerStatuses // [])[]
                | if .state.terminated then
                    "\(.name): \(.state.terminated.reason // "terminated") (exit \(.state.terminated.exitCode // "none"))"
                  else
                    "\(.name): \(.state // {} | keys | join(",") | if . == "" then "no state" else . end)"
                  end
            ]
        } | .epoch = .failed.e | .failed = .failed.t' "$WORK/list.json" >> "$WORK/pods.jsonl" 2> "$WORK/jq.err"; then
        sed 's/^/    /' "$WORK/jq.err" >&2
        fail "the pod list for $ns would not parse (above) — a pod whose timestamps cannot be read cannot be judged old enough"
    fi
done

# Failed pods left alone, said out loud (stderr: not part of what is signed).
jq -r 'select(.phase == "Failed" and (.job or .deleting))
    | "\(.ns)/\(.name) — \(if .job then "a Job owns it; its own retention reaps it" else "already being deleted" end)"' \
    "$WORK/pods.jsonl" | while IFS= read -r line; do say "left alone: $line"; done

# The candidates: every pod the scope admits at THIS render's clock.
jq -s --argjson cutoff "$CUTOFF" \
    'map(select(.phase == "Failed" and (.job | not) and (.deleting | not) and .epoch < $cutoff))' \
    "$WORK/pods.jsonl" > "$WORK/candidates.json" || fail "could not select the candidates"

# --- the plan document ------------------------------------------------------
# render <json-array-file> — the plan for exactly that set of pods. Pure
# in its input: nothing here reads a clock or the cluster.
render() {
    local set="$1" n
    n=$(jq 'length' "$set")
    echo "plan: reap-terminated-pods"
    echo "namespaces: ${NAMESPACES[*]}"
    echo "selects: every pod in those namespaces whose status.phase is Failed, which no Job owns and which is not already being deleted, which failed more than ${WINDOW_S}s before the render (failed by: the latest time the pod records — creation, a condition transition or a container finish). The render time is deliberately not in these bytes."
    echo
    echo "== pods to reap ($n) =="
    if [ "$n" -eq 0 ]; then
        echo "none — nothing to reap"
    else
        jq -r 'sort_by(.ns, .name)[]
            | "pod \(.ns)/\(.name)",
              "  uid: \(.uid)",
              "  node: \(.node)",
              "  created: \(.created)",
              "  failed by: \(.failed)",
              "  reason: \(.reason)",
              "  message: \(.message)",
              (if (.containers | length) == 0 then "  container: none reported"
               else (.containers[] | "  container \(.)") end)' "$set"
    fi
    echo
    echo "== the deletes this plan authorises =="
    if [ "$n" -eq 0 ]; then
        echo "none"
    else
        jq -r 'sort_by(.ns, .name)[]
            | "DELETE /api/v1/namespaces/\(.ns)/pods/\(.name) with preconditions.uid=\(.uid)"' "$set"
    fi
}

hash_of() { sha256sum "$1" | cut -d' ' -f1; }

render "$WORK/candidates.json" > "$WORK/plan"
HASH=$(hash_of "$WORK/plan")

if [ "$PLAN" -eq 1 ]; then cat "$WORK/plan"; echo "plan-sha256: $HASH" >&2; exit 0; fi

# --- the write --------------------------------------------------------------
# Find the approved set among the failure-time prefixes of today's
# candidates (header: THE CLOCK AT THE BOUNDARY). The empty prefix is one
# of them, so an approved "nothing to reap" is always found while it is
# still true of the cluster's planned pods.
jq -r '[.[].epoch] | unique | .[]' "$WORK/candidates.json" > "$WORK/cuts"
MATCHED=""
echo '[]' > "$WORK/prefix-0.json"
render "$WORK/prefix-0.json" > "$WORK/prefix.plan"
[ "$(hash_of "$WORK/prefix.plan")" = "$APPROVED" ] && MATCHED="$WORK/prefix-0.json"
k=0
while [ -z "$MATCHED" ] && IFS= read -r cut; do
    k=$((k + 1))
    jq --argjson c "$cut" 'map(select(.epoch <= $c))' "$WORK/candidates.json" > "$WORK/prefix-$k.json"
    render "$WORK/prefix-$k.json" > "$WORK/prefix.plan"
    [ "$(hash_of "$WORK/prefix.plan")" = "$APPROVED" ] && MATCHED="$WORK/prefix-$k.json"
done < "$WORK/cuts"

if [ -z "$MATCHED" ]; then
    cat "$WORK/plan" >&2
    refuse "no set of today's candidates renders to the approved plan $APPROVED; the plan as it stands (above) hashes to $HASH. A planned pod is gone, was replaced under its name, or a fact the plan recorded has moved since it was approved. Nothing was deleted; render and approve it again"
fi

# THE CAPTURE BEFORE THE DELETE: the approved plan, on stdout, first.
render "$MATCHED" > "$WORK/approved.plan"
cat "$WORK/approved.plan"
echo
n=$(jq 'length' "$MATCHED")
if [ "$n" -eq 0 ]; then
    echo "$ME: plan $APPROVED still holds and names no pod — nothing to reap"
    exit 0
fi
echo "$ME: plan $APPROVED still holds — reaping $n pod(s)"

# The delete reads its DeleteOptions body from stdin. The docker form of
# the resolved kubectl (undeclared-objects.sh resolve_kubectl) runs
# without -i, so its stdin is empty and the precondition would be
# dropped SILENTLY — a bare delete. Give that one invocation -i here,
# rather than to every caller of the resolution, where -i would swallow
# the stdin of any loop that runs kubectl inside a `while read`.
if [ "${KUBECTL[0]}" = docker ] && [ "${KUBECTL[1]:-}" = run ]; then
    KDEL=(docker run -i "${KUBECTL[@]:2}")
else
    KDEL=("${KUBECTL[@]}")
fi

failed=0
jq -c 'sort_by(.ns, .name)[] | [.ns, .name, .uid]' "$MATCHED" > "$WORK/targets"
while IFS= read -r t; do
    ns=$(jq -r '.[0]' <<< "$t"); name=$(jq -r '.[1]' <<< "$t"); uid=$(jq -r '.[2]' <<< "$t")
    jq -n --arg uid "$uid" '{kind: "DeleteOptions", apiVersion: "v1", preconditions: {uid: $uid}}' > "$WORK/body.json"
    if ! "${KDEL[@]}" delete --raw "/api/v1/namespaces/$ns/pods/$name" -f - --request-timeout=60s \
            < "$WORK/body.json" > "$WORK/del.out" 2> "$WORK/del.err"; then
        sed 's/^/    /' "$WORK/del.err" >&2
        say "the delete of $ns/$name (uid $uid) was refused or failed (above) — a precondition refusal means a different pod now holds the name, and it was left alone"
        failed=$((failed + 1))
        continue
    fi
    # A forge answer is not a forge effect: read it back.
    gone=""
    for _ in 1 2 3; do
        if "${KUBECTL[@]}" get pod "$name" -n "$ns" -o json --request-timeout=10s \
                > "$WORK/back.json" 2> "$WORK/back.err"; then
            now_uid=$(jq -r '.metadata.uid // empty' "$WORK/back.json")
            if [ "$now_uid" != "$uid" ]; then gone="its name is now held by uid ${now_uid:-?}, a different pod"; break; fi
        elif grep -q NotFound "$WORK/back.err"; then
            gone="gone"; break
        else
            sed 's/^/    /' "$WORK/back.err" >&2
            break
        fi
        sleep 2
    done
    if [ -n "$gone" ]; then
        echo "reaped $ns/$name uid $uid — $gone"
    else
        say "the delete of $ns/$name answered, but uid $uid could not be read back as gone"
        failed=$((failed + 1))
    fi
done < "$WORK/targets"

[ "$failed" -eq 0 ] || fail "$failed of $n planned pod(s) were not reaped (above); the rest were"
echo "$ME: reaped $n pod(s)"
