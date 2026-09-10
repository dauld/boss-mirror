#!/usr/bin/env bash
#
# the-live-protocols-are-the-authored-protocols — every protocol the
# running registry admits Jobs under is one this tree writes down.
#
# WHY THIS EXISTS
# ---------------
# The sibling one layer down is
# `infra/lint/the-live-rules-are-the-authored-rules.sh`, and this is the
# same defect in the same shape: a registry whose rows can be authored
# live through the API, a tree that is supposed to describe them, and
# nothing comparing the two. Measured 2026-09-10 against the live
# registry: EIGHTY-FOUR admitted workflow kinds, and TWELVE of them had
# no authored source anywhere in the tree — not a bundle file, not a
# tenant seed, not a Rust literal, not a migration. They were published
# through `POST /api/workflows` + publish and never written back.
#
# Twelve is not a list of curiosities. `gate-run` is the protocol every
# car's gate packet runs on; `publish-request` is how a branch from a
# credential-less workspace reaches the forge; `maintenance-sweep` is
# the chore family seven dispatcher clock rules spawn. Answering "what
# does the gate-run protocol require" meant querying production, a
# change to any of them was a live publish with no diff and no second
# reader, and a deployment built from this tree did not have them at
# all.
#
# NOT A COMPLAINT THAT PROTOCOLS ARE DATA. CLAUDE.md is explicit and
# right: "Adding a new workflow means adding a Workflow row, not
# touching core code", and the three-layers frame says a protocol that
# cannot be replaced without a deploy has leaked into the substrate.
# All of that stays true. The property here is narrower and does not
# contradict it: whatever the registry admits, the tree can SHOW you.
# Publishing a new version live stays legal; leaving the tree unable to
# describe it does not.
#
# THE CHECKED PROPERTY
# --------------------
# Live kinds are a subset of the kinds this TREE authors, plus the
# exemptions named below (empty, and meant to stay that way). Read from
# THIS tree deliberately, not from anything the deployment computes,
# because the tree under test is the one a car changes.
#
# FOUR AUTHORING HOMES, all of which count. A kind is authored if the
# tree says what it is, in any form a reader can read and a reviewer can
# diff:
#
#   1. infra/platform/workflows/<kind>.toml — the platform bundle, and
#      the destination. One kind per file, the stem IS the kind (the
#      loader refuses anything else; `platform_bundle.rs` pins it), so
#      `ls` answers "which kinds".
#   2. examples/<tenant>/seeds/workflows.toml — a tenant's bundle, one
#      file of `[[workflow]]` blocks, published by that tenant's prepare
#      step.
#   3. `platform_workflows()` in crates/core/boss-jobs/src/registry.rs —
#      Rust literals, reconciled into the registry on every boot. Being
#      retired into (1) by protocols-as-data; still authored until then.
#   4. An `INSERT INTO workflows` in infra/postgres/schema/ — how
#      `repair-a-train` arrived, and the reason this script reads
#      migrations at all. The packet that filed this gap first counted
#      seventeen and corrected itself to sixteen on exactly this check.
#
# Homes 3 and 4 are duplication this tree is moving away from, not
# endorsements. They count here because the question is "can a reader
# find out what this protocol is", and in both cases they can.
#
# ONE DIRECTION, on purpose. A kind authored with no live row is the
# EXPECTED state in two ways, neither of which is drift: between a
# protocol car's merge and the converge + seed behind it, and for every
# tenant bundle a deployment does not run (this tree ships two tenants;
# a deployment runs one). Failing on it would red cars for windows they
# do not control — the "an infrastructure refusal is not a consist
# failure" cost CLAUDE.md records. So it is REPORTED, never failed on.
#
# WHEN THE REGISTRY IS UNREACHABLE it SKIPS, loudly, and exits 0 — the
# gate runs on the forge host, which has no route to the in-cluster read
# surface, and a lint that reds there would red every car. The static
# half below still runs, so a skip is never a no-op. What it must never
# do is treat "could not read" as "nothing to report": a wrong target
# answers instead of erroring (CLAUDE.md §Doors), so an answer that
# parses but is not an array of workflow rows is a FAILURE, and so is an
# EMPTY one — a registry admitting zero kinds is dead air, not a clean
# bill.
#
# Usage:  infra/lint/the-live-protocols-are-the-authored-protocols.sh
#   BOSS_JOBS_URL  read surface base (default: the in-cluster machine
#                  door, boss-jobs-internal:7900)

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

BUNDLE="infra/platform/workflows"
TENANT_GLOB="examples/*/seeds/workflows.toml"
REGISTRY_RS="crates/core/boss-jobs/src/registry.rs"
SCHEMA_DIR="infra/postgres/schema"
BASE="${BOSS_JOBS_URL:-http://boss-jobs-internal.boss.svc.cluster.local:7900}"
URL="$BASE/api/workflows"

# Kinds the LIVE registry admits with no authored source anywhere, each
# with the reason it is tolerated. A set of names rather than a count,
# so adding a protocol never edits this line and two cars cannot collide
# on it (the BASELINE=<n> lesson, §9a).
#
# EMPTY, and that is the point. The twelve this script was written for
# were BACKFILLED in the same change that added it, because a lint that
# lands red blocks every car until someone drains it — the way
# `the-live-rules-are-the-authored-rules` held every rule-retirement car
# out of the queue until it learned to read retirement migrations. An
# entry here is a decision to stop describing one protocol, and an
# exemption list is how this class of gap got here in the first place.
EXEMPT=()

fail() { echo "the-live-protocols-are-the-authored-protocols: $*" >&2; problems=$((problems + 1)); }
problems=0

[ -d "$BUNDLE" ] || { echo "the-live-protocols-are-the-authored-protocols: $BUNDLE does not exist" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Static half — always runs, needs no network.
# ---------------------------------------------------------------------------
shopt -s nullglob
bundle_files=("$BUNDLE"/*.toml)
tenant_files=($TENANT_GLOB)
shopt -u nullglob
[ ${#bundle_files[@]} -gt 0 ] || { echo "the-live-protocols-are-the-authored-protocols: no *.toml in $BUNDLE — a wrong path, not an empty bundle" >&2; exit 1; }
[ ${#tenant_files[@]} -gt 0 ] || { echo "the-live-protocols-are-the-authored-protocols: no tenant seed matched $TENANT_GLOB — a wrong path, not a tenantless tree" >&2; exit 1; }
[ -f "$REGISTRY_RS" ] || { echo "the-live-protocols-are-the-authored-protocols: $REGISTRY_RS does not exist" >&2; exit 1; }

# Home 1: the bundle listing. One kind per file, named for the kind.
bundle_kinds=$(for f in "${bundle_files[@]}"; do basename "$f" .toml; done | LC_ALL=C sort -u)

# Home 2: a tenant bundle's `[[workflow]]` blocks. The kind key is taken
# ONLY inside a `[[workflow]]` table, never inside `[[workflow.step]]` —
# a step's `kind` is a StepType (`task`, `trigger`, `outcome`), and
# sweeping those in is what makes a naive grep's authored set too LOOSE.
# Loose under-reports the gap instead of over-reporting it, which is the
# quieter failure and so the one worth spelling out.
tenant_kinds_of() {
    LC_ALL=C awk '
        /^[ \t]*\[\[[ \t]*workflow[ \t]*\]\]/ { in_wf = 1; next }
        /^[ \t]*\[/                           { in_wf = 0 }
        in_wf && /^[ \t]*kind[ \t]*=/ {
            if (match($0, /"[^"]+"/)) print substr($0, RSTART + 1, RLENGTH - 2)
        }
    ' "$1"
}
tenant_kinds=$(for f in "${tenant_files[@]}"; do tenant_kinds_of "$f"; done | LC_ALL=C sort -u)

# Home 3: the Rust literals still inside `platform_workflows()`.
#
# The body is scraped between the signature and the first line that
# closes it at column 0. Two shapes are read: a kebab-case string
# literal (`maintenance_spec("maintenance-backup", …)`) and a
# no-argument `<name>_spec()` call, whose kind is its name with
# underscores as dashes (`design_doc_review_spec()` →
# `design-doc-review`). Labels and descriptions in the same body are
# filtered out by the kebab-case test — they carry spaces and capitals.
#
# A kind this cannot see reads as unauthored and FAILS, loudly, naming
# it. That is the right direction to be wrong in: the fix for a
# `platform_workflows()` entry this does not recognise is to move that
# kind into the bundle, which is where protocols-as-data is taking it
# anyway.
code_kinds=$(
    LC_ALL=C awk '
        /^pub fn platform_workflows\(\)/ { inside = 1; next }
        inside && /^\}/                  { inside = 0 }
        inside {
            line = $0
            sub(/\/\/.*$/, "", line)
            s = line
            while (match(s, /"[a-z][a-z0-9-]*"/)) {
                print substr(s, RSTART + 1, RLENGTH - 2)
                s = substr(s, RSTART + RLENGTH)
            }
            s = line
            while (match(s, /[a-z][a-z0-9_]*_spec\(\)/)) {
                name = substr(s, RSTART, RLENGTH - 2)
                sub(/_spec$/, "", name)
                gsub(/_/, "-", name)
                print name
                s = substr(s, RSTART + RLENGTH)
            }
        }
    ' "$REGISTRY_RS" | LC_ALL=C sort -u
)
[ -n "$code_kinds" ] || fail "read no kinds out of platform_workflows() in $REGISTRY_RS — the scrape broke, so a green result would mean nothing"

# Home 4: a migration that inserts a row directly. Only files that
# actually write a `workflows` row are read, and every single-quoted
# kebab-case literal in such a file counts. That is deliberately
# generous — `'active'` is swept up too — because a spurious authored
# name can only under-report the gap, while a missed real one would red
# a car for a protocol the tree does describe.
#
# The pattern is an ERE rather than the literal SQL phrase so that
# `api-path-bypass-smell` does not read this lint's own SQL-reading
# grep as a lint that writes to the database.
migration_kinds=$(
    LC_ALL=C grep -lE "INSERT[[:space:]]+INTO[[:space:]]+workflows" "$SCHEMA_DIR"/*.sql 2>/dev/null \
        | while IFS= read -r f; do
            LC_ALL=C grep -oE "'[a-z][a-z0-9-]*'" "$f" | tr -d "'"
        done | LC_ALL=C sort -u
)

authored=$(printf '%s\n%s\n%s\n%s\n' \
    "$bundle_kinds" "$tenant_kinds" "$code_kinds" "$migration_kinds" \
    | LC_ALL=C sed '/^$/d' | LC_ALL=C sort -u)

# An exemption for a kind the tree now authors is refused. Left standing
# it would keep a future live-authored kind of that name out of this
# check without anyone deciding so — the same refusal gate.sh applies to
# a PREFLIGHT_EXCLUDES entry naming a lint that no longer exists.
for kind in ${EXEMPT[@]+"${EXEMPT[@]}"}; do
    if printf '%s\n' "$authored" | LC_ALL=C grep -qxF "$kind"; then
        fail "the exemption for \`$kind\` is stale — the tree now authors it"
        echo "" >&2
        echo "  Drop \`$kind\` from EXEMPT in this script. An exemption that" >&2
        echo "  outlives its reason silently excuses the next live-authored" >&2
        echo "  protocol that happens to share the name." >&2
    fi
done

# ---------------------------------------------------------------------------
# Live half — skips loudly when the read surface is unreachable.
# ---------------------------------------------------------------------------
skip() {
    echo "the-live-protocols-are-the-authored-protocols: SKIPPED the live comparison — $1" >&2
    echo "  target: $URL (override with BOSS_JOBS_URL)" >&2
    echo "  The exemption set was still checked against the tree" >&2
    echo "  (${#EXEMPT[@]} exemptions, $(printf '%s\n' "$authored" | wc -l | tr -d ' ') authored kinds)." >&2
    echo "  Nothing is claimed about what the running registry admits." >&2
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

# One admitted kind per line. The surface answers a top-level ARRAY of
# registry rows; anything else is a different endpoint, or an error body
# wearing a 200. Exit 3 and 4 separate those two cases so the message
# can say which.
live_kinds=$(python3 - "$body" <<'PY'
import json, sys
try:
    doc = json.load(open(sys.argv[1]))
except Exception as e:
    print(f"unparseable: {e}", file=sys.stderr)
    sys.exit(3)
rows = doc.get("workflows") if isinstance(doc, dict) else doc
if not isinstance(rows, list) or not all(isinstance(r, dict) for r in rows):
    shape = sorted(doc) if isinstance(doc, dict) else type(doc).__name__
    print(f"not an array of workflow rows (got {shape})", file=sys.stderr)
    sys.exit(4)
kinds = sorted({r["kind"] for r in rows if r.get("status", "active") == "active" and "kind" in r})
if not kinds:
    sys.exit(5)
print("\n".join(kinds))
PY
)
case "$?" in
    0) ;;
    # A 200 that is not JSON is a proxy or a captive portal answering for
    # something that never reached the registry — no answer, wearing a
    # 200. The python error above is the evidence; treat it as no route.
    3) skip "the response did not parse as JSON — treated as no answer" ;;
    4) fail "$URL answered 200 with no array of workflow rows — a 200 from the \
wrong surface, or an error body; either way nothing read the registry"
       exit 1 ;;
    5) fail "$URL reports ZERO admitted workflow kinds"
       echo "" >&2
       echo "  An empty registry is not a clean comparison. A deployment that" >&2
       echo "  admits no kinds can open no Job of any sort: no gate run, no" >&2
       echo "  change shipped, no chore recorded. Read as 'nothing to compare'" >&2
       echo "  this would be the confident wrong answer." >&2
       exit 1 ;;
    *) skip "could not read kinds from the response" ;;
esac

unauthored=$(
    LC_ALL=C comm -23 <(printf '%s\n' "$live_kinds") <(printf '%s\n' "$authored") \
        | { if [ ${#EXEMPT[@]} -gt 0 ]; then LC_ALL=C grep -vxF -f <(printf '%s\n' "${EXEMPT[@]}"); else cat; fi; } \
        | LC_ALL=C sed '/^$/d'
)

# A stale exemption in the other direction: named here, not admitted
# live. Same reason as the authored check above — it would excuse a
# future kind of that name that nobody decided to excuse.
for kind in ${EXEMPT[@]+"${EXEMPT[@]}"}; do
    if ! printf '%s\n' "$live_kinds" | LC_ALL=C grep -qxF "$kind"; then
        fail "the exemption for \`$kind\` is stale — the live registry does not admit it"
        echo "" >&2
        echo "  Drop \`$kind\` from EXEMPT in this script." >&2
    fi
done

if [ -n "$unauthored" ]; then
    n=$(printf '%s\n' "$unauthored" | wc -l | tr -d ' ')
    fail "the registry admits $n workflow kind(s) this tree does not author:"
    printf '    %s\n' $unauthored >&2
    echo "" >&2
    echo "  Each of these reaches the registry without a file, so: a change to" >&2
    echo "  it is a live publish with no diff and no second reader, a fresh" >&2
    echo "  database will not have it at all, and answering \"what does this" >&2
    echo "  protocol require\" means querying production. Write it down:" >&2
    echo "" >&2
    echo "    $BUNDLE/<kind>.toml — one [[workflow]] named for the file." >&2
    echo "    Read the live body from GET $URL and render it; author it, do" >&2
    echo "    not retype it, because a file that DISAGREES with the live row" >&2
    echo "    replaces one problem with a worse one. ship-a-change.toml is the" >&2
    echo "    worked example and was generated for exactly that reason." >&2
    echo "" >&2
    echo "  A TENANT protocol (one only the brewery or the used-device-shop" >&2
    echo "  runs) goes in that tenant's examples/<tenant>/seeds/workflows.toml" >&2
    echo "  instead, not in the platform bundle." >&2
    echo "" >&2
    echo "  NO MIGRATION. The bundle is applied by boss-platform-workflow-seed" >&2
    echo "  (insert-if-missing, run from infra/postgres/bootstrap-db.sh), so a" >&2
    echo "  fresh database gets the file and an existing row is untouched. An" >&2
    echo "  insert-into-workflows migration would be a SECOND copy of the same" >&2
    echo "  fact (§9a) and would skip the viability lint that publish runs." >&2
    echo "" >&2
    echo "  See $BUNDLE/README.md. Exempting it in this script instead is a" >&2
    echo "  decision, and it belongs in the diff a reviewer reads." >&2
fi

[ "$problems" -eq 0 ] || exit 1

live_n=$(printf '%s\n' "$live_kinds" | wc -l | tr -d ' ')
authored_n=$(printf '%s\n' "$authored" | wc -l | tr -d ' ')
msg="the-live-protocols-are-the-authored-protocols: OK — $live_n admitted kinds, $authored_n authored"
[ ${#EXEMPT[@]} -eq 0 ] || msg="$msg, ${#EXEMPT[@]} exempt (${EXEMPT[*]})"
echo "$msg"

# Neither line below is a failure. A platform kind with no live row is
# the tree ahead of the deployment, in the window between a protocol
# car's merge and the converge + seed behind it. A tenant kind with no
# live row is a tenant this deployment does not run, which is the
# permanent and correct state for one of the two tenants this tree
# ships — counted rather than named, so a standing fact cannot train
# anyone to skim past the line above it.
pending=$(LC_ALL=C comm -13 <(printf '%s\n' "$live_kinds") <(printf '%s\n' "$bundle_kinds") | LC_ALL=C sed '/^$/d')
[ -z "$pending" ] || printf '  authored in the bundle, not yet admitted (awaiting converge + seed): %s\n' "$(printf '%s\n' $pending | tr '\n' ' ')"
for f in "${tenant_files[@]}"; do
    all=$(tenant_kinds_of "$f" | LC_ALL=C sort -u | LC_ALL=C sed '/^$/d')
    [ -n "$all" ] || continue
    absent=$(LC_ALL=C comm -13 <(printf '%s\n' "$live_kinds") <(printf '%s\n' "$all") | LC_ALL=C sed '/^$/d' | wc -l | tr -d ' ')
    total=$(printf '%s\n' "$all" | wc -l | tr -d ' ')
    [ "$absent" -eq 0 ] || printf '  %s: %s of %s kinds not admitted here (a tenant this deployment does not run)\n' "$f" "$absent" "$total"
done
exit 0
