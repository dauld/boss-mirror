#!/usr/bin/env bash
#
# the-live-rules-are-the-authored-rules — every reaction the running
# dispatcher enforces is one the authored registry records.
#
# WHY THIS EXISTS
# ---------------
# `infra/dispatcher/rules/` is the human-authored registry; the
# `dispatcher_rules` table is the runtime one. Two copies of one fact,
# which CLAUDE.md §9a says to collapse or pin. One direction was already
# pinned: `dispatcher_rules_seed_matches_toml` compares the directory
# against the SEED migrations, both ways, in a TestDb.
#
# That pin has a hole, and the hole is the whole of this script. The seed
# path is not the only way a row reaches the table — `POST
# /api/dispatcher/rules` + publish authors one live, with no file and no
# migration. A TestDb never sees those, so the seed guard stays green
# while the system enforces rules nobody wrote down. Measured 2026-09-10
# against the live registry: SIXTY-FOUR enforced rules, SIXTY files. The
# `why` guard — the justification every rule is supposed to carry — was
# covering sixty of sixty-four and reporting "OK (60 rules, each saying
# why it exists)", which is true about the directory and wrong about the
# system. Nothing could have noticed, because until now nothing compared
# the live set to anything.
#
# THE CHECKED PROPERTY
# --------------------
# Live rules are a subset of the authored directory, plus the exemptions
# named below. It reads the live set by NAME from the read surface and
# the authored set from THIS TREE — deliberately not from the endpoint's
# own `authored` flag, which is computed against the deployed image's
# tree, while the tree under test is the one a car changes.
#
# ONE DIRECTION, on purpose. A file with no live rule is the EXPECTED
# state between a rule car's merge and the converge that deploys and
# seeds it; failing on it would red every rule-adding car for a window
# it cannot control, which is the "an infrastructure refusal is not a
# consist failure" cost CLAUDE.md records. That direction is already a
# hard test on the path that can be tested hermetically (the seed guard
# above), so it is REPORTED here, not failed on.
#
# WHEN THE API IS UNREACHABLE it SKIPS, loudly, and exits 0 — the gate
# runs on the forge host, which has no route to the in-cluster read
# surface, and a lint that reds there would red every car. The static
# half below still runs, so a skip is never a no-op. What it must never
# do is treat "could not read" as "nothing to report": a wrong target
# answers instead of erroring (CLAUDE.md §Doors), so an answer that
# parses but carries no `rules` array, or an empty one, is a FAILURE —
# a dispatcher enforcing zero rules is dead air, not a clean bill.
#
# Usage:  infra/lint/the-live-rules-are-the-authored-rules.sh
#   BOSS_DISPATCHER_URL  read surface base (default: the in-cluster
#                        machine door, boss-dispatcher-internal:7950,
#                        backlog a757b72a)

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

RULES_DIR="infra/dispatcher/rules"
BASE="${BOSS_DISPATCHER_URL:-http://boss-dispatcher-internal.boss.svc.cluster.local:7950}"
URL="$BASE/api/dispatcher/rules"

# Rules the LIVE registry enforces with no file in $RULES_DIR, each with
# the reason it is tolerated. Not a count — a set of names, so adding a
# rule never edits this line and two cars cannot collide on it (the
# BASELINE=<n> lesson, §9a).
#
# Every one of these was authored live through POST /api/dispatcher/rules
# and never written back, so none of them carries a `why`. The fix is not
# an exemption: it is a file in $RULES_DIR plus an ON CONFLICT-safe seed
# migration (backlog 41ba00cd holds the collapse). They are listed rather
# than invented justifications for, because a `why` nobody who knows the
# intent wrote is worse than a named gap.
EXEMPT=(
    "auto-park-on-gate-green"
    "estate-alarm-on-comparison"
    "spawn-keg-return-on-delivery"
    "spawn-tasting-panel-on-brew-close"
)

fail() { echo "the-live-rules-are-the-authored-rules: $*" >&2; problems=$((problems + 1)); }
problems=0

[ -d "$RULES_DIR" ] || { echo "the-live-rules-are-the-authored-rules: $RULES_DIR does not exist" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Static half — always runs, needs no network.
# ---------------------------------------------------------------------------
# An exemption for a rule that now HAS a file is refused. Left standing
# it would keep a future live-authored rule of that name out of this
# check without anyone deciding so — the same refusal gate.sh applies to
# a PREFLIGHT_EXCLUDES entry naming a lint that no longer exists.
shopt -s nullglob
files=("$RULES_DIR"/*.toml)
shopt -u nullglob
[ ${#files[@]} -gt 0 ] || { echo "the-live-rules-are-the-authored-rules: no *.toml in $RULES_DIR — a wrong path, not an empty registry" >&2; exit 1; }

tree_names=$(for f in "${files[@]}"; do basename "$f" .toml; done | LC_ALL=C sort)

for name in "${EXEMPT[@]}"; do
    if [ -f "$RULES_DIR/$name.toml" ]; then
        fail "the exemption for \`$name\` is stale — $RULES_DIR/$name.toml now exists"
        echo "" >&2
        echo "  Drop \`$name\` from EXEMPT in this script. An exemption that" >&2
        echo "  outlives its reason silently excuses the next live-authored" >&2
        echo "  rule that happens to share the name." >&2
    fi
done

# ---------------------------------------------------------------------------
# Live half — skips loudly when the read surface is unreachable.
# ---------------------------------------------------------------------------
skip() {
    echo "the-live-rules-are-the-authored-rules: SKIPPED the live comparison — $1" >&2
    echo "  target: $URL (override with BOSS_DISPATCHER_URL)" >&2
    echo "  The exemption set was still checked against the tree" >&2
    echo "  (${#EXEMPT[@]} exemptions, ${#files[@]} authored rules). Nothing is claimed" >&2
    echo "  about what the running dispatcher enforces." >&2
    [ "$problems" -eq 0 ] || exit 1
    exit 0
}

command -v curl >/dev/null 2>&1 || skip "curl is not on this box"
command -v python3 >/dev/null 2>&1 || skip "python3 is not on this box"

body=$(mktemp) || exit 1
trap 'rm -f "$body"' EXIT
code=$(curl -sS -m 10 -o "$body" -w '%{http_code}' "$URL" 2>/dev/null)
# 000 is curl's "never got an answer" — no route, refused, timed out.
[ "$code" = "200" ] || skip "$URL answered HTTP $code"

# Rule names, one per line. A response that parses but carries no `rules`
# ARRAY is not an empty registry — it is a different endpoint, or an
# error body with a 200. Exit 3 and 4 separate those two cases so the
# message can say which.
live_names=$(python3 - "$body" <<'PY'
import json, sys
try:
    doc = json.load(open(sys.argv[1]))
except Exception as e:
    print(f"unparseable: {e}", file=sys.stderr)
    sys.exit(3)
rules = doc.get("rules") if isinstance(doc, dict) else None
if not isinstance(rules, list):
    print(f"no `rules` array (top-level keys: {sorted(doc) if isinstance(doc, dict) else type(doc).__name__})", file=sys.stderr)
    sys.exit(4)
names = sorted(r["name"] for r in rules if isinstance(r, dict) and "name" in r)
if not names:
    sys.exit(5)
print("\n".join(names))
PY
)
case "$?" in
    0) ;;
    # A 200 that is not JSON is a proxy or a captive portal answering for
    # something that never reached the dispatcher — no answer, wearing a
    # 200. The python error above is the evidence; treat it as no route.
    3) skip "the response did not parse as JSON — treated as no answer" ;;
    4) fail "$URL answered 200 with no \`rules\` array — a 200 from the wrong \
surface, or an error body; either way nothing read the registry"
       exit 1 ;;
    5) fail "$URL reports ZERO enforced rules"
       echo "" >&2
       echo "  An empty live registry is not a clean comparison. A dispatcher" >&2
       echo "  with no rules runs zero side effects: every step-completion" >&2
       echo "  effect, sweep and callback is dead air. Read as 'nothing to" >&2
       echo "  compare' this would be the confident wrong answer." >&2
       exit 1 ;;
    *) skip "could not read rule names from the response" ;;
esac

unauthored=$(LC_ALL=C comm -23 <(printf '%s\n' "$live_names") <(printf '%s\n' "$tree_names") \
             | LC_ALL=C grep -vxF -f <(printf '%s\n' "${EXEMPT[@]}") || true)
pending=$(LC_ALL=C comm -13 <(printf '%s\n' "$live_names") <(printf '%s\n' "$tree_names") || true)

# A stale exemption in the other direction: named here, not enforced
# live. Same reason as the file check above — it would excuse a future
# rule of that name that nobody decided to excuse.
for name in "${EXEMPT[@]}"; do
    if ! printf '%s\n' "$live_names" | LC_ALL=C grep -qxF "$name"; then
        fail "the exemption for \`$name\` is stale — the live registry does not enforce it"
        echo "" >&2
        echo "  Drop \`$name\` from EXEMPT in this script." >&2
    fi
done

if [ -n "$unauthored" ]; then
    fail "the dispatcher enforces $(printf '%s\n' "$unauthored" | wc -l | tr -d ' ') rule(s) that $RULES_DIR does not record:"
    printf '    %s\n' $unauthored >&2
    echo "" >&2
    echo "  Each of these reaches the registry without a file, so it carries" >&2
    echo "  no \`why\`: nothing says which standing exemption it claims" >&2
    echo "  (timer, threshold, external glue, cross-protocol reactor), and" >&2
    echo "  a fresh database will not have it at all. Write it down:" >&2
    echo "" >&2
    echo "    1. $RULES_DIR/<name>.toml — one [[rule]] named for the file," >&2
    echo "       carrying its \`why\`. Read the live body from" >&2
    echo "       GET $URL" >&2
    echo "    2. infra/postgres/schema/NNN-dispatcher-rule-<name>.sql — an" >&2
    echo "       ON CONFLICT-safe INSERT, so a fresh DB has it" >&2
    echo "       (101-dispatcher-rule-step-assigned.sql is the worked example)" >&2
    echo "    3. handler_emits() in crates/core/boss-dispatcher/src/cascade.rs," >&2
    echo "       if its \`do\` names a handler not already listed there" >&2
    echo "" >&2
    echo "  See $RULES_DIR/README.md. Exempting it in this script instead is" >&2
    echo "  a decision, and it belongs in the diff a reviewer reads." >&2
fi

[ "$problems" -eq 0 ] || exit 1

msg="the-live-rules-are-the-authored-rules: OK — $(printf '%s\n' "$live_names" | wc -l | tr -d ' ') enforced rules, ${#files[@]} authored"
[ ${#EXEMPT[@]} -eq 0 ] || msg="$msg, ${#EXEMPT[@]} exempt (${EXEMPT[*]})"
echo "$msg"
# Not a failure: the tree is ahead of the deployment between a rule
# car's merge and the converge that seeds it. The hard both-directions
# pin on the seed path is dispatcher_rules_seed_matches_toml.
[ -z "$pending" ] || printf '  authored but not yet enforced (awaiting converge + seed): %s\n' "$(printf '%s\n' $pending | tr '\n' ' ')"
exit 0
