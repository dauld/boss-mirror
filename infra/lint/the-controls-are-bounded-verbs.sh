#!/usr/bin/env bash
# the-controls-are-bounded-verbs.sh — the ad hoc controls are ops verbs
# with the reclaim-disk shape: a script in the tree, params with
# patterns that admit no whitespace and no leading dash, a required
# sha for the rollback; the hold file round-trips; the runner's hold
# check reads it.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; repo="$(cd "$here/../.." && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }
python3 - "$repo" <<'PY' || exit 1
import json,re,sys,os
repo=sys.argv[1]; v=json.load(open(f"{repo}/infra/ops/verbs.json"))["verbs"]
for name in ("rollback-to","hold-converge","release-converge"):
    spec=v.get(name) or sys.exit(f"FAIL: verb {name} missing")
    script=spec["argv"][0].replace("/home/david/boss/", f"{repo}/")
    os.path.isfile(script) or sys.exit(f"FAIL: {name} points at a script not in the tree: {spec['argv'][0]}")
    os.access(script, os.X_OK) or sys.exit(f"FAIL: {name}'s script is not executable")
    for p in spec["params"]:
        pat=p["pattern"]
        for bad in (" ", "\t", "\n"):
            re.fullmatch(pat, "a"+bad+"b") and sys.exit(f"FAIL: {name}.{p['name']} admits whitespace")
        re.fullmatch(pat, "-x") and sys.exit(f"FAIL: {name}.{p['name']} admits a leading dash")
    "MUTATING" in spec["about"] or sys.exit(f"FAIL: {name} does not say it is MUTATING")
    "David" in spec["about"] or sys.exit(f"FAIL: {name} names no authorization")
rb=v["rollback-to"]["params"][0]
"default" in rb and sys.exit("FAIL: rollback-to's sha has a default — a rollback must name its target")
re.fullmatch(rb["pattern"],"2683908") or sys.exit("FAIL: a 7-char sha is refused")
re.fullmatch(rb["pattern"],"b2814ef") or sys.exit("FAIL: a real short sha is refused")
re.fullmatch(rb["pattern"],"latest") and sys.exit("FAIL: 'latest' passes as a sha")
print("verbs: rollback-to, hold-converge, release-converge are bounded and authorized")
PY
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export BOSS_CONVERGE_HOLD="$tmp/hold"
bash "$repo/infra/forge/converge-hold.sh" hold learning-the-new-runner >/dev/null || fail "hold failed"
[[ "$(<"$tmp/hold")" == "learning-the-new-runner" ]] || fail "the hold file does not carry the reason"
# shellcheck source=/dev/null
. "$repo/infra/forge/cluster-deploy-lib.sh"
reason=$(converge_held "$tmp/hold") || fail "the runner's hold check did not see the hold"
[[ "$reason" == "learning-the-new-runner" ]] || fail "the hold check returned '$reason'"
bash "$repo/infra/forge/converge-hold.sh" release >/dev/null || fail "release failed"
converge_held "$tmp/hold" >/dev/null && fail "a released hold still holds"
bash "$repo/infra/forge/converge-hold.sh" hold 2>/dev/null && fail "a hold with no reason was accepted"
echo "the-controls-are-bounded-verbs: self-test ok — three bounded, authorized ops verbs; the hold round-trips through the file the runner reads; a hold needs a reason; a rollback needs a sha"
exit 0
