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
# THOSE FOUR ARE NOW FILED (backlog 8d471ec5), so EXEMPT is empty and
# this script's "OK" is a statement about the system. The hole it covers
# is not closed by that — it is closed by this script existing. Re-read
# 2026-09-10 18:52 UTC after four retirements converged: 60 live, 56
# authored, the same four unauthored; 60 and 60 with them filed.
#
# THE CHECKED PROPERTY
# --------------------
# Live rules are a subset of the authored directory, plus the rules the
# tree RETIRES, plus the exemptions named below. It reads the live set by
# name and version from the read surface and the authored set from THIS
# TREE — deliberately not from the endpoint's own `authored` flag, which
# is computed against the deployed image's tree, while the tree under
# test is the one a car changes.
#
# ONE DIRECTION, on purpose. A file with no live rule is the EXPECTED
# state between a rule car's merge and the converge that deploys and
# seeds it; failing on it would red every rule-adding car for a window
# it cannot control, which is the "an infrastructure refusal is not a
# consist failure" cost CLAUDE.md records. That direction is already a
# hard test on the path that can be tested hermetically (the seed guard
# above), so it is REPORTED here, not failed on.
#
# A RETIREMENT IS THE SAME WINDOW, READ BACKWARDS. The tolerance above
# is not about which set is larger; it is that THE TREE LEGITIMATELY
# LEADS THE LIVE REGISTRY between a merge and the converge behind it.
# A car that retires a rule deletes its file and retires the row in a
# migration — and a migration runs at converge, which is AFTER the
# consist check. So at check time the rule is still live and its file
# is already gone: the tree leading the live registry again, in the
# other direction. The first version of this script reasoned only
# about a rule being ADDED, so every rule-RETIREMENT car was blocked
# for good: `fix/the-flush-pipeline-is-deleted` was refused a train on
# every attempt (car a8262b51), and no amount of waiting would have
# helped, because the converge that would have cleared it is on the
# far side of the board.
#
# So the question this asks of a live rule with no file is not "is
# there a file" but "does the TREE still enforce this rule". The tree's
# answer is its migrations: replay every `dispatcher_rules` status
# write under infra/postgres/schema/ in apply order, and if the last
# word on THIS rule at THIS version is a retirement, the tree has
# retired it and the live row is residue the next converge clears.
#
# WHAT THAT DELIBERATELY DOES NOT EXCUSE. A live rule the tree says
# NOTHING about — no file and no migration touching it — still FAILS,
# which is the drift this check exists for and how the four rules that
# used to sit in EXEMPT were found (backlog 8d471ec5). "No migration
# mentions it" and "a migration retires it" are opposite answers, not
# the same silence.
#
# NOT AN EXEMPTION, on purpose. An EXEMPT entry would pass this car and
# then fail the next one: the static half refuses an exemption whose
# rule HAS a file and the live half refuses one the registry does not
# enforce, so an exemption written for a retirement goes stale the
# moment that retirement converges — a two-step dance across a converge,
# and a red gate for whoever is standing there when it goes stale.
# Reading the retirement migration needs no second step: the migration
# is applied history, it stays in the tree forever, and once the row is
# gone from the live set the clause simply stops matching anything.
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
# EMPTY, and the fix that emptied it is the one this set was waiting for.
# It held four rules authored live through POST /api/dispatcher/rules and
# never written back — auto-park-on-gate-green, estate-alarm-on-comparison,
# spawn-keg-return-on-delivery, spawn-tasting-panel-on-brew-close — each
# therefore carrying no `why` (backlog 8d471ec5). They were LISTED rather
# than justified, because a `why` nobody who knows the intent wrote is
# worse than a named gap. Each now has a file in $RULES_DIR carrying a
# sourced `why` and an ON CONFLICT-safe seed migration, so the "OK" this
# script prints is finally a statement about the SYSTEM and not about the
# directory. Adding a name back here is a decision that belongs in the
# diff a reviewer reads, and the two staleness checks below make it
# expire on its own.
EXEMPT=()

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

# One enforced rule per line as `name<TAB>version`. The version is what
# lets the retirement clause below be about THIS version of the rule
# rather than the name — a retirement naming a version retires only
# that row, which is how every version bump in the schema directory is
# written. A rule whose body carries no version prints an empty one and
# so matches only a version-less retirement: fails closed, never open.
#
# A response that parses but carries no `rules` ARRAY is not an empty
# registry — it is a different endpoint, or an error body with a 200.
# Exit 3 and 4 separate those two cases so the message can say which.
live_pairs=$(python3 - "$body" <<'PY'
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
pairs = sorted(
    (r["name"], r.get("version"))
    for r in rules
    if isinstance(r, dict) and "name" in r
)
if not pairs:
    sys.exit(5)
print("\n".join(f"{n}\t{'' if v is None else v}" for n, v in pairs))
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

live_names=$(printf '%s\n' "$live_pairs" | cut -f1)

# ---------------------------------------------------------------------------
# What the TREE says each rule's status will be after the next converge.
# ---------------------------------------------------------------------------
# The authored directory answers "is this rule written down"; it cannot
# answer "is this rule RETIRED", because a retirement's whole shape is
# the file being gone. The migrations answer that, and they are the same
# authority the database will obey: `infra/postgres/schema/*.sql` in
# apply order (the numeric prefix, `sort -t- -k1,1n`, exactly as
# migrate.sh derives it).
#
# One event per status write, `name|version|status`, in apply order, with
# `*` for a retirement that names no version and so takes every row of
# that name. Replaying them is how a version BUMP is told apart from a
# retirement: `202608311700-sweep-spawn-guards.sql` retires v1 and
# inserts v2 active, so the last word on v2 is an insert and that rule is
# still enforced by the tree — a missing file for it is real drift, not a
# retirement. Nothing here is per-rule; it is a replay.
#
# SHAPES IT READS. Comments are stripped first (quote-aware, so a `--`
# inside a literal survives). An `INSERT INTO dispatcher_rules` row is
# read as `('<name>', <version>, '<status>'` — the column order every
# insert in the directory uses. A status write is `UPDATE
# dispatcher_rules ... SET status = '<status>'` (or a DELETE) with the
# names taken from whatever follows `name` in the statement, so
# `name = 'x'` and `name IN ('x', 'y')` both read, across lines, in
# either clause order.
#
# WHAT IT DOES NOT READ, each of which leaves the rule looking untouched
# by the tree and so FAILS rather than passes: a retirement matching by
# anything but a literal name (`name LIKE`, a subquery), one whose `SET`
# and `status` land on different lines, and a version bump done live
# through the API with no migration behind it.
SCHEMA_DIR="infra/postgres/schema"

schema_files=()
while IFS= read -r base; do
    [ -n "$base" ] && schema_files+=("$SCHEMA_DIR/$base")
done <<EOF
$(find "$SCHEMA_DIR" -maxdepth 1 -name '*.sql' -type f -exec basename {} \; 2>/dev/null \
    | LC_ALL=C sort -t- -k1,1n)
EOF

if [ ${#schema_files[@]} -lt 10 ]; then
    fail "found only ${#schema_files[@]} migration(s) in $SCHEMA_DIR — the scrape broke"
    echo "" >&2
    echo "  The retirement replay below reads that directory. Refusing rather" >&2
    echo "  than reporting every retired rule as unauthored drift." >&2
    exit 1
fi

rule_events=$(mktemp) || exit 1
trap 'rm -f "$body" "$rule_events"' EXIT

LC_ALL=C awk '
function strip(line,   i, n, c, out, inq) {
    out = ""; inq = 0; n = length(line)
    for (i = 1; i <= n; i++) {
        c = substr(line, i, 1)
        if (c == "\047") { inq = 1 - inq; out = out c; continue }
        if (inq == 0 && c == "-" && substr(line, i + 1, 1) == "-") break
        out = out c
    }
    return out
}
function reset_stmt() { un = 0; ustatus = ""; uversion = ""; seenname = 0; split("", uname) }
function emit_stmt(   i, st) {
    st = (mode == "DELETE") ? "deleted" : ustatus
    if ((mode != "UPDATE" && mode != "DELETE") || st == "") return
    for (i = 1; i <= un; i++)
        print uname[i] "|" (uversion == "" ? "*" : uversion) "|" st
}
FNR == 1 { mode = "NONE"; reset_stmt() }
{
    line = strip($0)
    low = tolower(line)

    if (low ~ /^[ \t]*insert[ \t]+into[ \t]+dispatcher_rules([ \t(]|$)/) { mode = "INSERT"; reset_stmt() }
    else if (low ~ /^[ \t]*update[ \t]+dispatcher_rules([ \t]|$)/) { mode = "UPDATE"; reset_stmt() }
    else if (low ~ /^[ \t]*delete[ \t]+from[ \t]+dispatcher_rules([ \t]|$)/) { mode = "DELETE"; reset_stmt() }
    else if (low ~ /^[ \t]*(insert[ \t]+into|update|delete[ \t]+from)[ \t]+[a-z_]/) { mode = "NONE"; reset_stmt() }

    if (mode == "INSERT") {
        s = line
        while (match(s, /\047[A-Za-z0-9_.:@+-]+\047[ \t]*,[ \t]*[0-9]+[ \t]*,[ \t]*\047[a-z]+\047/)) {
            tup = substr(s, RSTART, RLENGTH)
            s = substr(s, RSTART + RLENGTH)
            nm = tup; sub(/\047[ \t]*,.*$/, "", nm); sub(/^\047/, "", nm)
            vv = tup; sub(/^[^,]*,[ \t]*/, "", vv); sub(/[ \t]*,.*$/, "", vv)
            st = tup; sub(/^.*,[ \t]*\047/, "", st); sub(/\047$/, "", st)
            print nm "|" vv "|" st
        }
    } else if (mode == "UPDATE" || mode == "DELETE") {
        if (match(low, /set[ \t]+status[ \t]*=[ \t]*\047[a-z]+\047/)) {
            seg = substr(line, RSTART, RLENGTH)
            if (match(seg, /\047[a-z]+\047/)) ustatus = substr(seg, RSTART + 1, RLENGTH - 2)
        }
        if (match(low, /version[ \t]*=[ \t]*[0-9]+/)) {
            seg = substr(low, RSTART, RLENGTH); gsub(/[^0-9]/, "", seg); uversion = seg
        }
        seg = ""
        if (seenname) seg = line
        else if (match(low, /(^|[^a-z_])name([^a-z_]|$)/)) { seenname = 1; seg = substr(line, RSTART + RLENGTH) }
        while (match(seg, /\047[^\047]*\047/)) {
            cand = substr(seg, RSTART + 1, RLENGTH - 2)
            seg = substr(seg, RSTART + RLENGTH)
            if (cand != "" && cand != "active" && cand != "draft" && cand != "retired") uname[++un] = cand
        }
    }

    if (index(line, ";") > 0) { emit_stmt(); mode = "NONE"; reset_stmt() }
}
' "${schema_files[@]}" > "$rule_events"

# Does the tree's last word on THIS rule at THIS version retire it?
# A rule with no event at all is not retired — it is a rule the tree has
# never heard of, which is precisely the drift this check fails on.
tree_retires() {
    LC_ALL=C awk -F'|' -v n="$1" -v v="$2" '
        $1 != n { next }
        ($2 == "*" || $2 == v) { last = $3 }
        END { exit (last != "" && last != "active") ? 0 : 1 }
    ' "$rule_events"
}

# Live, no file, not exempt — then split by what the tree says.
unauthored=()
retiring=()
while IFS= read -r name; do
    [ -n "$name" ] || continue
    version=$(printf '%s\n' "$live_pairs" | LC_ALL=C awk -F'\t' -v n="$name" '$1 == n { print $2; exit }')
    if tree_retires "$name" "$version"; then
        retiring+=("$name")
    else
        unauthored+=("$name")
    fi
done <<EOF
$(LC_ALL=C comm -23 <(printf '%s\n' "$live_names") <(printf '%s\n' "$tree_names") \
  | LC_ALL=C grep -vxF -f <(printf '%s\n' "${EXEMPT[@]}") || true)
EOF

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

if [ ${#unauthored[@]} -gt 0 ]; then
    fail "the dispatcher enforces ${#unauthored[@]} rule(s) that $RULES_DIR does not record, and no migration retires:"
    printf '    %s\n' "${unauthored[@]}" >&2
    echo "" >&2
    echo "  Each of these reaches the registry without a file, so it carries" >&2
    echo "  no \`why\`: nothing says which standing exemption it claims" >&2
    echo "  (timer, threshold, external glue, cross-protocol reactor), and" >&2
    echo "  a fresh database will not have it at all. Write it down:" >&2
    echo "" >&2
    echo "    1. $RULES_DIR/<name>.toml — one [[rule]] named for the file," >&2
    echo "       carrying its \`why\`. Read the live body from" >&2
    echo "       GET $URL" >&2
    echo "    2. $SCHEMA_DIR/NNN-dispatcher-rule-<name>.sql — an" >&2
    echo "       ON CONFLICT-safe INSERT, so a fresh DB has it" >&2
    echo "       (101-dispatcher-rule-step-assigned.sql is the worked example)" >&2
    echo "    3. handler_emits() in crates/core/boss-dispatcher/src/cascade.rs," >&2
    echo "       if its \`do\` names a handler not already listed there" >&2
    echo "" >&2
    echo "  RETIRING it instead? Then the file is meant to be gone, and what" >&2
    echo "  this check reads is the migration that retires the row — a status" >&2
    echo "  write in $SCHEMA_DIR naming the rule, which is also what a fresh" >&2
    echo "  database needs. 202609101200-the-flush-pipeline-is-deleted.sql is" >&2
    echo "  the worked example. If you wrote one and this still names the" >&2
    echo "  rule, the replay did not recognise its shape — fix the replay in" >&2
    echo "  this script, not the rule." >&2
    echo "" >&2
    echo "  See $RULES_DIR/README.md. Exempting it in this script instead is" >&2
    echo "  a decision, and it belongs in the diff a reviewer reads." >&2
fi

[ "$problems" -eq 0 ] || exit 1

msg="the-live-rules-are-the-authored-rules: OK — $(printf '%s\n' "$live_names" | wc -l | tr -d ' ') enforced rules, ${#files[@]} authored"
[ ${#EXEMPT[@]} -eq 0 ] || msg="$msg, ${#EXEMPT[@]} exempt (${EXEMPT[*]})"
echo "$msg"
# Neither of these is a failure: both are the tree ahead of the
# deployment, in the window between a rule car's merge and the converge
# behind it. The hard both-directions pin on the seed path is
# dispatcher_rules_seed_matches_toml.
[ -z "$pending" ] || printf '  authored but not yet enforced (awaiting converge + seed): %s\n' "$(printf '%s\n' $pending | tr '\n' ' ')"
[ ${#retiring[@]} -eq 0 ] || printf '  retired in this tree but still enforced (awaiting converge + migrate): %s\n' "${retiring[*]}"
exit 0
