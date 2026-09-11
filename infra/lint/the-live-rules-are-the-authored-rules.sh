#!/usr/bin/env bash
#
# the-live-rules-are-the-authored-rules — the dispatcher-rule registry
# the running system enforces is DERIVED from the authored directory, and
# this asks the running system whether the derivation actually happened.
#
# WHY THIS EXISTS
# ---------------
# `infra/dispatcher/rules/` is the definition: one `<rule-name>.toml`
# per rule, carrying its shape and its reviewed `why`. The
# `dispatcher_rules` table is the runtime registry, published from that
# directory by the dispatcher at boot (`rules::seed::seed_authored_rules`).
# Nothing else writes rules: no migration seeds one, and deleting a file
# is how a rule is retired.
#
# That leaves exactly one question a static check cannot answer — did the
# derivation RUN on the deployment, and is the registry it is enforcing
# the one the image carries? Before the collapse (backlog 41ba00cd) the
# same question was unanswerable for a different reason: a rule was
# declared twice, in a file and in an `INSERT INTO dispatcher_rules`, and
# only the second one ran. Measured 2026-09-10 against the live registry:
# sixty-four enforced rules, sixty files — the `why` guard was covering
# sixty of sixty-four and reporting "OK (60 rules, each saying why it
# exists)", true about the directory and wrong about the system. The four
# it found had been authored live through `POST /api/dispatcher/rules`
# and never written back (backlog 8d471ec5).
#
# THE CHECKED PROPERTY
# --------------------
# Three failures, each a statement about the SYSTEM:
#
#   1. The read surface answers, but not with a rule registry — a 200
#      from the wrong surface, or an error body wearing a 200.
#   2. It reports ZERO enforced rules. A dispatcher with no rules runs
#      zero side effects; read as "nothing to compare" that would be the
#      confident wrong answer (CLAUDE.md §Doors).
#   3. The deployment enforces a rule its OWN image does not author
#      (`authored: false`), or cannot read its own authored directory at
#      all. Either way the derivation is not holding, and a fresh
#      database would not have that rule.
#
# CHECK 3 IS THE ONE THAT USED TO NEED A WINDOW TOLERANCE, and the
# collapse is why it no longer does. It used to compare the live set
# against THIS tree, which legitimately LEADS the deployment between a
# rule car's merge and the converge behind it — in both directions, since
# a retirement deletes the file before the converge. Reasoning about that
# window took ~150 lines of awk replaying every `dispatcher_rules` status
# write in `infra/postgres/schema/` in apply order, and the first version
# of it read only the add direction, which held every rule-retirement car
# out of a train for good (car a8262b51, `fix/the-flush-pipeline-is-
# deleted`, refused on every attempt).
#
# None of that is needed now. The `authored` flag and the live registry
# come from the SAME deployment — the flag is computed against the tree
# that deployment's image carries — so there is no window between them,
# and no migration to replay, because no migration writes rules. The
# replay was deleted with the second home it was reading.
#
# Differences against THIS tree are still REPORTED, both directions,
# because that window is exactly what an operator wants to see: a file
# not yet enforced is waiting on the converge, and a live rule this tree
# no longer authors is one the next converge retires.
#
# WHEN THE API IS UNREACHABLE it SKIPS, loudly, and exits 0 — the gate
# runs on the forge host, which has no route to the in-cluster read
# surface, and a lint that reds there would red every car.
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

fail() { echo "the-live-rules-are-the-authored-rules: $*" >&2; problems=$((problems + 1)); }
problems=0

[ -d "$RULES_DIR" ] || { echo "the-live-rules-are-the-authored-rules: $RULES_DIR does not exist" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Static half — always runs, needs no network.
# ---------------------------------------------------------------------------
shopt -s nullglob
files=("$RULES_DIR"/*.toml)
shopt -u nullglob
[ ${#files[@]} -gt 0 ] || { echo "the-live-rules-are-the-authored-rules: no *.toml in $RULES_DIR — a wrong path, not an empty registry" >&2; exit 1; }

tree_names=$(for f in "${files[@]}"; do basename "$f" .toml; done | LC_ALL=C sort)

# ---------------------------------------------------------------------------
# Live half — skips loudly when the read surface is unreachable.
# ---------------------------------------------------------------------------
skip() {
    echo "the-live-rules-are-the-authored-rules: SKIPPED the live comparison — $1" >&2
    echo "  target: $URL (override with BOSS_DISPATCHER_URL)" >&2
    echo "  ${#files[@]} authored rules were counted in the tree. Nothing is claimed" >&2
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

# The response, flattened to lines bash can read:
#   REGISTRY<TAB><dir><TAB><rules-counted><TAB><error-or-empty>
#   RULE<TAB><name><TAB><version><TAB><authored true|false>
#
# A response that parses but carries no `rules` ARRAY is not an empty
# registry — it is a different endpoint, or an error body with a 200.
# Exit 3/4/5 separate unparseable, wrong-shape and empty so the message
# can say which.
read_out=$(python3 - "$body" <<'PY'
import json, sys
try:
    doc = json.load(open(sys.argv[1]))
except Exception as e:
    print(f"unparseable: {e}", file=sys.stderr)
    sys.exit(3)
rules = doc.get("rules") if isinstance(doc, dict) else None
if not isinstance(rules, list):
    keys = sorted(doc) if isinstance(doc, dict) else type(doc).__name__
    print(f"no `rules` array (top-level keys: {keys})", file=sys.stderr)
    sys.exit(4)
reg = doc.get("authored_registry") or {}
if not isinstance(reg, dict):
    reg = {}
print("REGISTRY\t%s\t%s\t%s" % (
    reg.get("dir") or "",
    reg.get("rules") if isinstance(reg.get("rules"), int) else "",
    (reg.get("error") or "").replace("\t", " ").replace("\n", " "),
))
out = []
for r in rules:
    if not isinstance(r, dict) or "name" not in r:
        continue
    v = r.get("version")
    out.append((r["name"], "" if v is None else str(v), "true" if r.get("authored") else "false"))
if not out:
    sys.exit(5)
for name, v, authored in sorted(out):
    print("RULE\t%s\t%s\t%s" % (name, v, authored))
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

registry_line=$(printf '%s\n' "$read_out" | LC_ALL=C grep -m1 '^REGISTRY' || true)
registry_dir=$(printf '%s\n' "$registry_line" | cut -f2)
registry_count=$(printf '%s\n' "$registry_line" | cut -f3)
registry_error=$(printf '%s\n' "$registry_line" | cut -f4)

live_rules=$(printf '%s\n' "$read_out" | LC_ALL=C grep '^RULE' || true)
live_names=$(printf '%s\n' "$live_rules" | cut -f2)

# ---------------------------------------------------------------------------
# Check 3 — the deployment against its OWN authored registry.
# ---------------------------------------------------------------------------
# No window here: both halves come from the same deployment. A rule the
# image's own directory does not record is one the derivation did not
# publish and a fresh database would not have.
if [ -n "$registry_error" ]; then
    fail "the deployment cannot read its own authored rule registry: $registry_error"
    echo "" >&2
    echo "  dir as the deployment sees it: ${registry_dir:-<unset>}" >&2
    echo "  That directory IS the definition of the rule registry, so a" >&2
    echo "  deployment that cannot read it cannot derive the rules it" >&2
    echo "  enforces: nothing publishes a new rule file, nothing retires a" >&2
    echo "  deleted one, and every \`why\` reads null. It is carried in the" >&2
    echo "  image at /opt/boss/infra/dispatcher/rules and named by" >&2
    echo "  BOSS_DISPATCHER_RULES — check both." >&2
elif [ -z "$registry_count" ] || [ "$registry_count" -lt 1 ]; then
    fail "the deployment reports an EMPTY authored rule registry at ${registry_dir:-<unset>}"
    echo "" >&2
    echo "  An empty authored registry with a non-empty live one means the" >&2
    echo "  image is missing infra/dispatcher/rules — a packaging fault. The" >&2
    echo "  rules it is enforcing are residue nothing authors." >&2
else
    unauthored=$(printf '%s\n' "$live_rules" | LC_ALL=C awk -F'\t' '$4 == "false" { print $2 }')
    if [ -n "$unauthored" ]; then
        count=$(printf '%s\n' "$unauthored" | wc -l | tr -d ' ')
        fail "the dispatcher enforces $count rule(s) its OWN image does not author:"
        printf '    %s\n' $unauthored >&2
        echo "" >&2
        echo "  These reach the registry without a file, so each carries no" >&2
        echo "  \`why\`: nothing says which standing exemption it claims" >&2
        echo "  (timer, threshold, external glue, cross-protocol reactor)." >&2
        echo "  The usual cause is a rule published live through" >&2
        echo "  POST /api/dispatcher/rules and never written back — the" >&2
        echo "  two-phase flip (ship the handler inert, turn it on live) is" >&2
        echo "  fine; stopping at it is the defect, and it is how the four" >&2
        echo "  rules of backlog 8d471ec5 were found." >&2
        echo "" >&2
        echo "  Write it down: $RULES_DIR/<name>.toml — one [[rule]] named" >&2
        echo "  for the file, carrying its \`why\`. Read the live body from" >&2
        echo "  GET $URL. No migration: the dispatcher publishes the file at" >&2
        echo "  boot. Also handler_emits() in" >&2
        echo "  crates/core/boss-dispatcher/src/cascade.rs if its \`do\` names" >&2
        echo "  a handler not already listed there." >&2
        echo "" >&2
        echo "  RETIRING it instead? Delete nothing here — deleting the file" >&2
        echo "  IS the retirement, and the next converge retires the row." >&2
        echo "  Until then this check stays quiet about it, because the flag" >&2
        echo "  it reads comes from the deployed image's tree, not yours." >&2
    fi
fi

[ "$problems" -eq 0 ] || exit 1

echo "the-live-rules-are-the-authored-rules: OK — $(printf '%s\n' "$live_names" | wc -l | tr -d ' ') enforced rules, ${#files[@]} authored in this tree, ${registry_count} in the deployed one"

# Neither direction is a failure: both are THIS tree differing from the
# deployment, in the window between a rule car's merge and the converge
# behind it. The hermetic both-directions proof is
# boss-dispatcher's `the_registry_equals_the_authored_directory_after_a_seed`.
pending=$(LC_ALL=C comm -13 <(printf '%s\n' "$live_names") <(printf '%s\n' "$tree_names") || true)
retiring=$(LC_ALL=C comm -23 <(printf '%s\n' "$live_names") <(printf '%s\n' "$tree_names") || true)
[ -z "$pending" ] || printf '  authored in this tree, not yet enforced (awaiting converge + seed): %s\n' "$(printf '%s\n' $pending | tr '\n' ' ')"
[ -z "$retiring" ] || printf '  no longer authored in this tree, still enforced (the next converge retires): %s\n' "$(printf '%s\n' $retiring | tr '\n' ' ')"
exit 0
