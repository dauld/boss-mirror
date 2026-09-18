#!/usr/bin/env bash
# alerts-are-packets.sh — an alert becomes a packet in David's queue,
# or is kept and filed by the next run that reaches the API; and the
# watchdog raises one on every outcome that needs a person.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="$here/../forge/alert-lib.sh"; wd="$here/../forge/cluster-watchdog.sh"
[[ -f "$lib" && -f "$wd" ]] || { echo "alerts-are-packets: missing $lib or $wd" >&2; exit 1; }
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export ALERT_SPOOL="$tmp/spool" JOBS_API="http://stub"
# shellcheck source=/dev/null
. "$lib"
fail() { echo "FAIL: $*" >&2; exit 1; }
# The body is a well-formed urgent backlog-item for the platform owner
# with the facts. The owner is READ, never named (backlog 3c23662d):
# the unit's BOSS_PLATFORM_OWNER wins outright; else the people
# registry's first active platform-admin by hire date (the roster
# below lists the later hire first, and a null hire_date sorts last);
# else nobody — an empty owner_id the jobs API resolves or refuses.
# employee-id-ok: a staged roster, this lint's fixture (no-employee-id-literal)
body=$(BOSS_PLATFORM_OWNER=emp-named alert_body 'cluster DARK: hands "needed"' 'line one
line two')
python3 -c "import json,sys;b=json.loads(sys.argv[1]);assert b['kind']=='backlog-item' and b['priority']=='urgent' and b['owner_id']=='emp-named' and 'needed' in b['title'] and b['metadata']['raised_at'] and 'line one line two'==b['metadata']['detail']" "$body" || fail "the alert body is not the packet shape: $body"
curl() { printf '%s' '[{"id":"emp-later","hire_date":"2026-09-17","role":"platform-admin","status":"active"},{"id":"emp-undated","hire_date":null,"role":"platform-admin","status":"active"},{"id":"emp-first","hire_date":"2026-09-16","role":"platform-admin","status":"active"}]'; }
[[ "$(alert_owner)" == "emp-first" ]] || fail "the first platform-admin hire is the owner, got: $(alert_owner)"
[[ "$(alert_people_api)" == "http://stub:$(sed -n 's/^people=//p' "$here/../forge/sor-ports.env")" ]] || fail "the people door is the SoR host on sor-ports.env's people port, got: $(alert_people_api)"
curl() { return 7; }
[[ -z "$(alert_owner)" ]] || fail "a dark registry names nobody, got: $(alert_owner)"
body=$(alert_body 'still filed' 'd')
python3 -c "import json,sys;b=json.loads(sys.argv[1]);assert b['owner_id']==''" "$body" || fail "an alert with no owner resolvable still files, with nobody: $body"
unset -f curl
# Stub the POST: down keeps the alert, up files it; replay drains oldest first.
posted="$tmp/posted"; : >"$posted"; echo down >"$tmp/api"
alert_post() { printf '%s\n' "$1" >>"$posted"; [[ "$(<"$tmp/api")" == "up" ]]; }
alert "first" "d1" 2>/dev/null; alert "second" "d2" 2>/dev/null
[[ "$(alert_count)" -eq 2 ]] || fail "alerts were not kept while the API was down: $(alert_count)"
echo up >"$tmp/api"; : >"$posted"
alert_replay 2>/dev/null || fail "replay failed with the API up"
[[ "$(alert_count)" -eq 0 ]] || fail "kept alerts were not filed on replay"
grep -q '"title":"first"' "$posted" && grep -q '"title":"second"' "$posted" || fail "replay did not file both alerts"
[[ "$(grep -n '"title":"first"' "$posted" | cut -d: -f1)" -lt "$(grep -n '"title":"second"' "$posted" | cut -d: -f1)" ]] || fail "replay was not oldest first"
alert "third" "d3" 2>/dev/null; [[ "$(alert_count)" -eq 0 ]] || fail "an alert with the API up was kept instead of filed"
# The watchdog raises an alert on every outcome that needs a person, and replays on ok.
for want in 'alert "cluster restored by the watchdog' 'alert "cluster DARK: hands needed — the rollback' 'alert "cluster DARK: hands needed — the last converged' 'alert_replay'; do
    grep -qF "$want" "$wd" || fail "cluster-watchdog.sh lacks: $want"
done
echo "alerts-are-packets: self-test ok — an alert is an urgent packet for the platform owner (read from the registry, the unit's override winning, nobody when neither answers); kept while the API is down and filed oldest first when it answers; the watchdog raises one on restore and on both hands-needed outcomes"
exit 0
