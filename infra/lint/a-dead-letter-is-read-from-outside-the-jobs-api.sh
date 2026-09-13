#!/usr/bin/env bash
# a-dead-letter-is-read-from-outside-the-jobs-api.sh — the cluster
# observer carries the dispatcher's dead-letter counters into its
# observation, read off the dispatcher's OWN surface, and losing that
# read costs the counters alone, visibly, never the observation.
#
# WHY (backlog 8834804a). feat/a-dead-letter-lands-on-its-packet made a
# budget-exhausted handler failure annotate the SUBJECT PACKET, which
# covers 28 of the 34 topics the dispatcher binds. Two cases it cannot
# serve: six topics carry no packet at all (the commerce/inventory/
# ledger facts and the two estate events), and a dead-letter whose
# annotation write ITSELF fails — which is precisely the case where the
# jobs API is what broke. In both, the only record was a WARN line with
# a pod's lifetime. The dispatcher now counts them on
# /api/dispatcher/readyz (dead_letters, dead_letters_unrecorded,
# last_unrecorded_dead_letter_unix) — a local read that owes nothing to
# the jobs API — and this observer carries that count to the record on
# the next firing the record answers. CLAUDE.md §Diagnosis: a monitor
# needs a path to a reader that does not depend on what it watches.
#
# WHAT IT CHECKS, by RUNNING the observer rather than reading it (the
# extraction is a-cluster-node-reports-its-headroom's, byte for byte —
# inline shell nothing executes is untested shell):
#
#   1. the observation carries `dispatcher` with the three counters as
#      NUMBERS, read from GET <dispatcher door>/api/dispatcher/readyz,
#      and the reading is spoken on stdout;
#   2. a dispatcher that cannot be reached costs `dispatcher: null` plus
#      a `dispatcher_unread` that names WHY, with every node field
#      intact, the POST still made and exit 0 — the node observation is
#      the observer's first duty (the alarm's silence sweep rides it),
#      and `estate.compare` turns the null into an informational
#      `dispatcher_unread` so going blind stays distinguishable from
#      zero dead-letters;
#   3. a 200 from the WRONG surface (a body without the counters) is
#      UNREAD, never zero — the confident wrong answer CLAUDE.md §Doors
#      warns about, refused at the instrument;
#   4. the door the block reads is the one the tree declares: the
#      Service name and port come from boss-dispatcher-internal.yaml,
#      and the block's default must name them (§9a — a fact that lives
#      twice, pinned; the Service cannot be sourced into a CronJob).
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/../cluster/manifests/boss-estate-observe.yaml"
door="$here/../cluster/manifests/boss-dispatcher-internal.yaml"
name="a-dead-letter-is-read-from-outside-the-jobs-api"
[[ -f "$manifest" ]] || { echo "$name: missing $manifest" >&2; exit 1; }
[[ -f "$door" ]] || { echo "$name: missing $door" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "$name: no jq — the observer is sh + jq, so this check cannot run without it" >&2; exit 1; }

fail() { echo "$name: FAIL — $*" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

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

# ----- 4: the door, read from the Service that declares it -----------
svc=$(awk '/^metadata:/{m=1} m && /^  name:/{print $2; exit}' "$door")
port=$(grep -oE 'port: *[0-9]+' "$door" | head -n 1 | grep -oE '[0-9]+')
[[ -n "$svc" && -n "$port" ]] || fail "could not read the Service name and port out of $door"
grep -q "http://$svc.boss.svc.cluster.local:$port" "$tmp/observe.sh" \
    || fail "the observer's shell does not default its dispatcher door to http://$svc.boss.svc.cluster.local:$port — the Service in $door says that is where the dispatcher answers, and a read aimed anywhere else is a permanent dispatcher_unread that looks like a dispatcher outage"

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
# /api/dispatcher/readyz as DispatcherLiveness::snapshot renders it.
cat >"$tmp/readyz.json" <<'JSON'
{"ready":true,"assigning":true,"assignment_events":412,"rules_running":true,"rules_events":9031,
 "schedule_running":true,"schedule_events":3,"last_event_unix":1789300000,
 "dead_letters":5,"dead_letters_unrecorded":2,"last_dead_letter_unix":1789299000,
 "last_unrecorded_dead_letter_unix":1789298000}
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
if [[ "${1:-}" == "get" && "${2:-}" == "jobs" ]]; then echo '{"items":[]}'; exit 0; fi
echo "kubectl stub: unexpected args: $*" >&2
exit 99
STUB
# curl: logs every request; answers the readyz GET per $READYZ_MODE
# (ok | refused | wrong-surface), the observation POST 202.
cat >"$tmp/bin/curl" <<'STUB'
#!/usr/bin/env bash
method=GET; url=""; prev=""
for a in "$@"; do
    case "$prev" in
        -X) method="$a" ;;
        --data-binary) [[ "$a" == @* ]] && cat "${a#@}" >"$CAPTURE" ;;
    esac
    [[ "$a" == http* ]] && url="$a"
    prev="$a"
done
printf '%s\t%s\n' "$method" "$url" >>"$LOG"
case "$method $url" in
    "GET "*/api/dispatcher/readyz)
        case "$READYZ_MODE" in
            ok) cat "$FIXTURES/readyz.json"; exit 0 ;;
            refused) echo "curl: (7) Failed to connect to $url port 7950: Connection refused" >&2; exit 7 ;;
            wrong-surface) printf '{"recorded":true}\n'; exit 0 ;;
            *) echo "curl stub: READYZ_MODE unset" >&2; exit 98 ;;
        esac ;;
    "POST "*/api/estate/observation) printf '{"recorded":true}\n202'; exit 0 ;;
    *) printf '{"error":"stub has no answer for %s %s"}\n500' "$method" "$url"; exit 0 ;;
esac
STUB
chmod +x "$tmp/bin/kubectl" "$tmp/bin/curl"

run_observer() {
    local mode="$1" work="$tmp/work-$1"
    mkdir -p "$work"
    : >"$tmp/log-$mode"; : >"$tmp/posted-$mode.json"
    READYZ_MODE="$mode" LOG="$tmp/log-$mode" CAPTURE="$tmp/posted-$mode.json" FIXTURES="$tmp" \
        PATH="$tmp/bin:$PATH" JOBS_API="http://stub" BOSS_OBSERVE_WORK="$work" \
        bash "$tmp/observe.sh" >"$tmp/out-$mode" 2>&1
}

# ----- 1: the counters ride the observation, as numbers --------------
run_observer ok; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-ok" >&2; fail "the observer exited $rc with the dispatcher answering"; }
grep -q "^GET	http://.*/api/dispatcher/readyz$" "$tmp/log-ok" \
    || { cat "$tmp/log-ok" "$tmp/out-ok" >&2; fail "the observer never read /api/dispatcher/readyz — the dead-letter counters have no path to the record"; }
posted="$tmp/posted-ok.json"
[[ -s "$posted" ]] || { cat "$tmp/out-ok" >&2; fail "nothing was posted"; }
for field in dead_letters dead_letters_unrecorded last_unrecorded_dead_letter_unix; do
    t=$(jq -r ".dispatcher.$field | type" "$posted")
    [[ "$t" == "number" ]] || { cat "$tmp/out-ok" >&2; fail "the posted observation's dispatcher.$field is $t, not a number (observation: $(jq -c .dispatcher "$posted"))"; }
done
unrec=$(jq -r '.dispatcher.dead_letters_unrecorded' "$posted")
[[ "$unrec" == "2" ]] || fail "dispatcher.dead_letters_unrecorded=$unrec, expected 2 as readyz reported it — a reading copied, not retyped"
[[ "$(jq -r '.dispatcher_unread // empty' "$posted")" == "" ]] \
    || fail "a successful read still carried dispatcher_unread: $(jq -c .dispatcher_unread "$posted")"
[[ "$(jq -r '.nodes | length' "$posted")" == "1" ]] || fail "the node observation lost its nodes when the dispatcher read was added"
grep -q 'dead_letters' "$tmp/out-ok" \
    || { cat "$tmp/out-ok" >&2; fail "the dispatcher reading was not spoken on stdout — whoever is next in front of a dead-letter should read the number, not re-derive it"; }

# ----- 2: an unreachable dispatcher costs the counters, not the run --
run_observer refused; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-refused" >&2; fail "the observer exited $rc because the DISPATCHER was unreachable — a best-effort read took the whole observation with it"; }
posted="$tmp/posted-refused.json"
[[ -s "$posted" ]] || { cat "$tmp/out-refused" >&2; fail "nothing was posted when the dispatcher was unreachable — the node observation must never depend on the dispatcher read"; }
[[ "$(jq -r '.dispatcher' "$posted")" == "null" ]] \
    || fail "an unreachable dispatcher reported dispatcher=$(jq -c .dispatcher "$posted") — a failed read must be null, never a number and never a silent omission"
why=$(jq -r '.dispatcher_unread // empty' "$posted")
[[ -n "$why" ]] || fail "an unreachable dispatcher left no dispatcher_unread on the observation — estate.compare would read the null as an observer that never looked, not one that was refused"
printf '%s' "$why" | grep -q 'Connection refused' \
    || fail "dispatcher_unread does not carry curl's own reason (got: $why) — a verdict must name what failed"
[[ "$(jq -r '.nodes[0].disk_free_gb' "$posted")" == "390" ]] || fail "the node fields were lost when the dispatcher read failed"
grep -q 'Connection refused' "$tmp/out-refused" \
    || { cat "$tmp/out-refused" >&2; fail "the failed dispatcher read was not reported on stdout — quiet is a loan against the next diagnosis (CLAUDE.md §Diagnosis)"; }

# ----- 3: a 200 without the counters is unread, never zero -----------
run_observer wrong-surface; rc=$?
[[ $rc -eq 0 ]] || { cat "$tmp/out-wrong-surface" >&2; fail "the observer exited $rc on a counter-less readyz body"; }
posted="$tmp/posted-wrong-surface.json"
[[ "$(jq -r '.dispatcher' "$posted")" == "null" ]] \
    || fail "a readyz body WITHOUT the counters was recorded as dispatcher=$(jq -c .dispatcher "$posted") — the wrong surface answering 200 must read as unread, never as zero dead-letters"
[[ -n "$(jq -r '.dispatcher_unread // empty' "$posted")" ]] \
    || fail "a counter-less readyz body left no dispatcher_unread"

echo "$name: ok — the observer carries the dispatcher's dead-letter counters off $svc:$port as numbers (unrecorded=2 in the fixture), an unreachable dispatcher degrades to dispatcher:null with curl's reason on the observation and the nodes intact, and a counter-less 200 is unread rather than zero"
exit 0
