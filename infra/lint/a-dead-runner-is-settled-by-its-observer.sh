#!/usr/bin/env bash
# a-dead-runner-is-settled-by-its-observer.sh — a gate runner that dies
# without reporting is settled `lost` by the cluster observer, on the
# observer's next pass, from the Job status Kubernetes already holds.
#
# WHY. 2026-09-11 18:58Z (incident a398c4d3): cp-2 and w-1 went NotReady
# together and the gate runner for docs/a-probe-shape-follows-the-car
# died with w-1 at 19:03:08. Kubernetes marked its Job `Failed` that
# second. Its packet read "active, stale: false" forty minutes later and
# drew two gate rows for one branch once it was re-gated — because the
# only reader of a gate Job was the Job, and the conductor that reaps
# dead runs has no Kubernetes access by design (boss-conductor.yaml:
# "git/HTTP to the forge and NOTHING to Kubernetes"), so it reaps by
# CLOCK: three hours after opened_at. Three hours of a corpse holding a
# slot on the yard, for a fact the cluster could state in one read.
#
# The actor that has both halves is the cluster observer: `kubectl` in
# the alpine/k8s image, a ServiceAccount, and a signed write path to the
# system of record every fifteen minutes. This check RUNS the observer's
# inline shell (the extraction is a-cluster-node-reports-its-headroom's,
# byte for byte) under stub `kubectl` and `curl` and asserts:
#
#   1. a gate Job with status.failed > 0 whose packet is open with the
#      verdict step not yet completed gets ONE PUT: verdict `lost`, and a
#      receipt that names the Job and its Failed condition (reason and
#      time) — a verdict must name what failed (CLAUDE.md §Diagnosis);
#   2. a failed Job whose packet already carries a verdict is left alone
#      (a red gate's runner exits non-zero AFTER reporting — the report
#      is the truth, the exit code is not a second verdict);
#   3. a live Job (neither succeeded nor failed) is left alone;
#   4. the settle is BEST-EFFORT: when `kubectl get jobs` is refused the
#      observation still posts, the run exits 0, and the refusal is
#      spoken on stdout — the node observation is the observer's first
#      duty and a second duty must never cost it;
#   5. the RBAC the read depends on is granted in the same file — a
#      namespaced Role on batch/jobs in the gate namespace, bound to the
#      observer's ServiceAccount — because a manifest that reads what its
#      Role does not grant is denied at run time and reads as "no dead
#      runners", which is the silent-absence class this exists to end.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/../cluster/manifests/boss-estate-observe.yaml"
[[ -f "$manifest" ]] || { echo "a-dead-runner-is-settled-by-its-observer: missing $manifest" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "a-dead-runner-is-settled-by-its-observer: no jq — the observer is sh + jq, so this check cannot run without it" >&2; exit 1; }

fail() { echo "a-dead-runner-is-settled-by-its-observer: FAIL — $*" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# ----- 5: the grant, read from the same file -------------------------
grep -qE '^\s*resources:\s*\[jobs\]' "$manifest" \
    || fail "no Role grants batch/jobs — the observer's read of gate Jobs would be Forbidden and every dead runner would read as none"
grep -qE '^\s*(- )?apiGroups:\s*\[batch\]' "$manifest" \
    || fail "the jobs grant is not in apiGroups [batch] — a jobs grant in the core group authorizes nothing"

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

# Three gate Jobs as `kubectl get jobs -l app=gate-runner -o json` shows
# them: DEAD (failed, packet open, no verdict), REPORTED (failed, but its
# packet already carries a verdict), LIVE (no status yet). The DEAD one
# is the real Job from the incident, condition and all.
DEAD=610d715e-48f3-4f2f-9269-cffddcb84ca0
REPORTED=22222222-2222-4222-8222-222222222222
LIVE=33333333-3333-4333-8333-333333333333
cat >"$tmp/gate-jobs.json" <<JSON
{"items":[
 {"metadata":{"name":"gate-docs-a-probe-shape-f-x8c5q",
              "labels":{"app":"gate-runner","boss.dev/packet":"$DEAD"}},
  "status":{"failed":1,"conditions":[
    {"type":"FailureTarget","reason":"BackoffLimitExceeded","lastTransitionTime":"2026-09-11T19:03:08Z"},
    {"type":"Failed","reason":"BackoffLimitExceeded","message":"Job has reached the specified backoff limit","lastTransitionTime":"2026-09-11T19:20:30Z"}]}},
 {"metadata":{"name":"gate-fix-red-for-real-abcde",
              "labels":{"app":"gate-runner","boss.dev/packet":"$REPORTED"}},
  "status":{"failed":1,"conditions":[
    {"type":"Failed","reason":"BackoffLimitExceeded","lastTransitionTime":"2026-09-11T18:00:00Z"}]}},
 {"metadata":{"name":"gate-feat-still-running-fghij",
              "labels":{"app":"gate-runner","boss.dev/packet":"$LIVE"}},
  "status":{"active":1}}
]}
JSON
# The packets as GET /api/jobs/{id} returns them (flat: status + steps).
cat >"$tmp/packet-$DEAD.json" <<JSON
{"id":"$DEAD","kind":"gate-run","status":"open","metadata":{"branch":"docs/a-probe-shape-follows-the-car"},
 "steps":[{"id":"64594238-0000-4000-8000-000000000001","kind":"trigger","status":"completed"},
          {"id":"bfdc7ff5-0000-4000-8000-000000000002","kind":"gate-verdict","status":"ready","metadata":{}},
          {"id":"dc8f0b53-0000-4000-8000-000000000005","kind":"outcome","status":"pending"}]}
JSON
cat >"$tmp/packet-$REPORTED.json" <<JSON
{"id":"$REPORTED","kind":"gate-run","status":"closed","metadata":{"branch":"fix/red-for-real"},
 "steps":[{"id":"aaaaaaaa-0000-4000-8000-000000000001","kind":"trigger","status":"completed"},
          {"id":"bbbbbbbb-0000-4000-8000-000000000002","kind":"gate-verdict","status":"completed","metadata":{"verdict":"failed"}}]}
JSON

# ----- stubs -----
mkdir -p "$tmp/bin"
cat >"$tmp/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "get" && "${2:-}" == "nodes" ]]; then cat "$FIXTURES/nodes.json"; exit 0; fi
if [[ "${1:-}" == "get" && "${2:-}" == "--raw" ]]; then
    node="${3#/api/v1/nodes/}"; node="${node%%/*}"
    cat "$FIXTURES/stats-$node.json"; exit 0
fi
if [[ "${1:-}" == "get" && "${2:-}" == "jobs" ]]; then
    if [[ -n "${JOBS_FORBIDDEN:-}" ]]; then
        echo 'Error from server (Forbidden): jobs.batch is forbidden: User "system:serviceaccount:boss:estate-observer" cannot list resource "jobs" in API group "batch" in the namespace "boss-dev"' >&2
        exit 1
    fi
    # The read must be scoped to gate runners: an unlabelled read would
    # settle every failed Job in the namespace as a lost gate.
    printf '%s\n' "$*" | grep -q -- '-l app=gate-runner' || { echo "kubectl stub: jobs read is not scoped to app=gate-runner: $*" >&2; exit 98; }
    cat "$FIXTURES/gate-jobs.json"; exit 0
fi
echo "kubectl stub: unexpected args: $*" >&2
exit 99
STUB
# curl: records every request (method, url, body) in order; answers the
# observation POST 202, a packet GET with its fixture, a step PUT 204.
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
    "GET "*/api/jobs/*)
        id="${url##*/api/jobs/}"
        if [[ -f "$FIXTURES/packet-$id.json" ]]; then cat "$FIXTURES/packet-$id.json"; else printf '{"error":"not found"}'; fi
        [[ -n "$want_code" ]] && printf '\n200' ;;
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
        BOSS_OBSERVE_WORK="$work" BOSS_GATE_NAMESPACE="boss-dev" \
        bash "$tmp/observe.sh" >"$tmp/out-$1" 2>&1
}

# ----- 1-3: one settle, the right one, with a receipt that names -----
run_observer settle; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-settle" >&2; fail "the observer exited $rc"; }
grep -q 'POST	http://stub/api/estate/observation' "$tmp/log-settle" \
    || { cat "$tmp/out-settle" >&2; fail "the node observation was not posted — the settle must ride behind it, never replace it"; }
puts=$(grep -c '^PUT	' "$tmp/log-settle")
[[ "$puts" -eq 1 ]] || { cat "$tmp/log-settle" "$tmp/out-settle" >&2; fail "expected exactly ONE step write (the dead runner's), got $puts"; }
grep -q "^PUT	http://stub/api/jobs/$DEAD/steps/bfdc7ff5-0000-4000-8000-000000000002	" "$tmp/log-settle" \
    || { cat "$tmp/log-settle" >&2; fail "the one write did not go to the dead runner's gate-verdict step"; }
put_body=$(grep "^PUT	http://stub/api/jobs/$DEAD/" "$tmp/log-settle" | cut -f3-)
[[ "$(printf '%s' "$put_body" | jq -r '.status')" == "completed" ]] \
    || fail "the settle does not complete the step (body: $put_body)"
[[ "$(printf '%s' "$put_body" | jq -r '.metadata.verdict')" == "lost" ]] \
    || fail "the verdict written is not \`lost\` (body: $put_body)"
receipt=$(printf '%s' "$put_body" | jq -r '.metadata.receipt')
for must in 'gate-docs-a-probe-shape-f-x8c5q' 'BackoffLimitExceeded' '2026-09-11T19:20:30Z'; do
    printf '%s' "$receipt" | grep -qF "$must" \
        || fail "the receipt does not name '$must' — a verdict must name what failed and when (receipt: $receipt)"
done
grep -q "$REPORTED" "$tmp/log-settle" && grep -q "^PUT	http://stub/api/jobs/$REPORTED" "$tmp/log-settle" \
    && fail "a failed Job whose packet already carries a verdict was written again — the runner's report is the truth, its exit code is not a second verdict"
grep -q "$LIVE" "$tmp/log-settle" \
    && fail "a live Job's packet was touched — nothing has finished, so nothing may be settled"
grep -q 'gate-docs-a-probe-shape-f-x8c5q' "$tmp/out-settle" \
    || { cat "$tmp/out-settle" >&2; fail "the settle was not spoken on stdout — quiet is a loan against the next diagnosis"; }

# ----- 4: a refused read costs nothing but a line --------------------
JOBS_FORBIDDEN=1 run_observer refused; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-refused" >&2; fail "the observer exited $rc when the jobs read was refused — a second duty took the first with it"; }
grep -q 'POST	http://stub/api/estate/observation' "$tmp/log-refused" \
    || fail "the observation was not posted when the jobs read was refused"
grep -q '^PUT	' "$tmp/log-refused" && fail "a refused read still produced a step write"
grep -qi 'forbidden' "$tmp/out-refused" \
    || { cat "$tmp/out-refused" >&2; fail "the refused jobs read was not spoken on stdout with the server's reason"; }

echo "a-dead-runner-is-settled-by-its-observer: ok — one Failed runner settled lost with its Job and condition named; a reported and a live Job untouched; a refused read costs one line"
