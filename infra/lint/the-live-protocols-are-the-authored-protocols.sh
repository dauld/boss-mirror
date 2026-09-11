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
# THE SECOND CHECKED PROPERTY — a live row still SAYS what its file says
# ----------------------------------------------------------------------
# The property above is about existence: whatever the registry admits,
# the tree can show you. It is satisfied by a file that exists and is
# WRONG, and on 2026-09-10 one was. `design-doc-review`'s live v1
# `description` described `boss-docs-api` parsing `### Qn:` headings,
# `/api/design/pending-decisions` and the flush jobs — every one of them
# deleted that day — while the tree file carried the corrected text.
# TWO mechanisms each declined to close the gap: the bundle seed is
# insert-if-missing, so a present row is skipped whole, and
# `bootstrap_reconcile`'s `kind_body_matches` excludes `description` as
# cosmetic, so it saw no drift and republished nothing (backlog
# e882b74c). The row was corrected by an operator publish at 02:53Z on
# 2026-09-11; this half is the mechanism that names the next one.
#
# THREE FIELDS, all scalar strings an operator reads and none of which
# changes what the protocol does: `description`, `label`, `category`.
# Structural fields are deliberately out, and so is `owning_team` — the
# loader overrides the file's key, so a disagreement there could never
# be cleared by a publish. The comparator's own comment carries the
# reason for each inclusion and each exclusion.
#
# NOT A CASE FOR WIDENING `kind_body_matches`. That function governs
# every bootstrap-created row, so widening it would change reconcile's
# behaviour for rows this problem is not about — and since
# `platform_workflows()` went empty on 2026-09-11 it iterates nothing,
# so widening it would compare nothing here either. Worth being exact:
# `label` and `category` ARE already in it and `description` alone is
# not, so for a bundle-authored kind nothing compares any of the three.
# That is the gap this half fills.
#
# A DRIFT IS REPORTED, NOT FAILED ON, under a bare invocation — the same
# tolerance the kind half gives the same window, read one field deeper:
# a car edits a description, merges, and the row does not move until an
# operator publishes. Failing would red every car in between, which is
# the churn argument that excluded the field from reconcile arriving
# again as a red gate. `--require-live` is the mode for a caller that
# can act on a verdict.
#
# Usage:  infra/lint/the-live-protocols-are-the-authored-protocols.sh
#           [--require-live] [--self-test]
#
#   --require-live  For a caller with somewhere to put the answer (a
#                   sweep, a cadence, an operator asking the question
#                   directly). The live comparison MUST happen: an
#                   unreachable registry exits 75 instead of skipping to
#                   0, and a field drift is a verdict (2) rather than a
#                   report. A check that passes when it could not read
#                   is worse than no check.
#   --self-test     Run the fixture cases and say what they proved.
#                   They run on every invocation regardless; the flag
#                   only makes them speak.
#
#   Exit codes:  0 clean (or drift, reported, bare invocation)
#                1 a failure of the tree — an unauthored live kind, a
#                  Rust literal, a stale exemption, an unreadable
#                  bundle file, or a comparison refused as vacuous
#                2 field drift, under --require-live only
#               64 unknown argument
#               75 EX_TEMPFAIL — the live comparison could not run;
#                  under --require-live only, bare exits 0
#
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

NAME="the-live-protocols-are-the-authored-protocols"

# How many kinds the field comparison must actually compare before its
# silence means anything. See the floor's own comment below.
FIELD_FLOOR=20

REQUIRE_LIVE=0
SELF_TEST=0
while [ $# -gt 0 ]; do
    case "$1" in
        --require-live) REQUIRE_LIVE=1 ;;
        --self-test)    SELF_TEST=1 ;;
        *) echo "$NAME: unknown argument: $1" >&2; exit 64 ;;
    esac
    shift
done

fail() { echo "the-live-protocols-are-the-authored-protocols: $*" >&2; problems=$((problems + 1)); }
problems=0

# ---------------------------------------------------------------------------
# The field comparator, and the self-test that proves it can refuse.
# ---------------------------------------------------------------------------
# `fields_report <bundle_dir> <live_json>` is the whole of the second
# property, factored out so it can be driven from fixtures with no
# network. It prints one machine-readable line per finding:
#
#   COUNTS  parsed=<n>  compared=<n>  drifted=<n>
#   DRIFT   <kind>  <field>  v<live_version>  at=<offset>  tree=<len>  live=<len>  <tree window>  <live window>
#   ABSENT  <kind>  <field>          the FILE makes no claim
#   NOROW   <kind>                   no ACTIVE live row for this kind
#
# Exit codes are the vocabulary the self-test asserts against, because a
# comparator that cannot say WHICH way it failed sends the next reader
# to re-derive it (CLAUDE.md §Diagnosis, "a verdict must name what
# failed"): 0 compared cleanly or with drift, 3 the live answer did not
# parse, 6 a bundle file did not parse or held no [[workflow]], 7 the
# non-vacuity floor refused.
fields_report() {
    python3 - "$1" "$2" "$FIELD_FLOOR" <<'PY'
import json, sys, tomllib, pathlib

bundle_dir, live_path, floor = pathlib.Path(sys.argv[1]), sys.argv[2], int(sys.argv[3])

# The fields compared, and the reason each one is on the list. All four
# are SCALAR STRINGS an operator reads, and none of them changes what
# the protocol DOES — which is exactly why nothing else compares them.
#
#   description  the sentence an operator reads to know what a protocol
#                is for. The measured instance: design-doc-review's live
#                v1 described `boss-docs-api`, `/api/design/pending-
#                decisions` and the flush jobs, all deleted on
#                2026-09-10, so the row sent a reader looking for a
#                service that is gone (backlog e882b74c). It is also
#                the ONE field `kind_body_matches` names as cosmetic and
#                skips.
#   label        the protocol's name in every list and tab that renders
#                it. A label disagreeing with its file means an operator
#                and a reviewer are talking about differently-named
#                things.
#   category     groups protocols in the UI, and has a measured defect
#                of its own: `maintenance_spec` put the DESCRIPTION in
#                the category column for all three chores (6c796f75).
#
# `owning_team` WAS on this list and came off it, which is worth keeping:
# the file's key is decorative. `seed_loader` ends its conversion with
# `spec.owning_team = default_owner` — "platform" for this bundle, the
# tenant id for a tenant's — so the TOML key is read and thrown away,
# and `boss workflow publish` reads the same loader. A disagreement
# there is therefore not a row lagging its file; it is a file claiming
# something the loader will not honour, and NO publish could ever clear
# it. A finding no action can close trains a reader to skip the whole
# report (§Diagnosis, "a check nobody reads").
#
# DELIBERATELY NOT COMPARED: `steps`, `subject_kinds`, `metadata_schema`,
# `entitlements`, `metadata`, `on_complete_create`. Those are
# STRUCTURAL — they decide what the protocol does — and a live row
# legitimately leads its file between a published version and the car
# that writes it down, so comparing them here would report the normal
# case as drift. They also need the same normalisation the publish path
# applies (defaults filled, predicates parsed) before an equality means
# anything, which is a check of its own, not a line in this one. Also
# out: `version`, `status`, `created_at`, `authoring_job_id` — four
# columns with no TOML key at all, so the file cannot disagree with
# them.
FIELDS = ("label", "description", "category")

try:
    doc = json.load(open(live_path))
except Exception as e:
    print(f"the live answer did not parse: {e}", file=sys.stderr)
    sys.exit(3)
rows = doc.get("workflows") if isinstance(doc, dict) else doc
if not isinstance(rows, list):
    print("the live answer is not an array of workflow rows", file=sys.stderr)
    sys.exit(3)
active = {
    r["kind"]: r
    for r in rows
    if isinstance(r, dict) and "kind" in r and r.get("status", "active") == "active"
}

def window(s, at, width=90):
    """The text around the first difference, on one line.

    Both full copies stay readable at named locations — the file in
    this tree, the row at GET /api/workflows — so this reduction
    discards no only-copy (§Diagnosis). It exists so a 1,200-character
    description does not make the finding unreadable.
    """
    if s is None:
        return "<absent>"
    start = max(0, at - 20)
    cut = s[start:start + width]
    cut = cut.replace("\t", " ").replace("\n", "\\n").replace("\r", " ")
    return ("…" if start else "") + cut + ("…" if start + width < len(s) else "")

def first_diff(a, b):
    a, b = a or "", b or ""
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return i
    return min(len(a), len(b))

files = sorted(bundle_dir.glob("*.toml"))
parsed = compared = drifted = 0
lines = []
for f in files:
    try:
        with open(f, "rb") as fh:
            body = tomllib.load(fh)
        blocks = body["workflow"]
        if not isinstance(blocks, list) or not blocks:
            raise KeyError("workflow")
    except Exception as e:
        print(f"{f}: not a readable [[workflow]] file: {e}", file=sys.stderr)
        sys.exit(6)
    for wf in blocks:
        kind = wf.get("kind")
        if not kind:
            print(f"{f}: a [[workflow]] block with no kind", file=sys.stderr)
            sys.exit(6)
        parsed += 1
        row = active.get(kind)
        if row is None:
            # Not drift, and not counted as compared: the tree leading
            # the deployment is the expected window between a protocol
            # car's merge and the converge + seed behind it.
            lines.append(f"NOROW\t{kind}")
            continue
        compared += 1
        for field in FIELDS:
            if field not in wf:
                lines.append(f"ABSENT\t{kind}\t{field}")
                continue
            tree, live = wf[field], row.get(field)
            if tree == live:
                continue
            drifted += 1
            at = first_diff(tree, live)
            lines.append(
                "DRIFT\t{}\t{}\tv{}\tat={}\ttree={}\tlive={}\t{}\t{}".format(
                    kind, field, row.get("version", "?"), at,
                    len(tree) if isinstance(tree, str) else "-",
                    len(live) if isinstance(live, str) else "-",
                    window(tree if isinstance(tree, str) else str(tree), at),
                    window(live if isinstance(live, str) else str(live), at),
                )
            )

print(f"COUNTS\tparsed={parsed}\tcompared={compared}\tdrifted={drifted}")
print("\n".join(lines)) if lines else None

# THE NON-VACUITY FLOOR. A comparison of nothing is the failure mode
# this whole check is written against: an empty bundle, a reader whose
# idiom moved, or a registry answering about a different world all
# produce "no drift found", which reads exactly like a clean bill
# (backlog 024c0db2; and the falsely-passing absence assertion of
# 61085a9e). The floor is deliberately far below the bundle's real size
# so that adding or retiring a protocol never edits it — it is a
# parse-sanity floor, not a ratchet with a number to bump (§9a).
if parsed != len(files):
    print(
        f"REFUSED\tread {parsed} [[workflow]] block(s) from {len(files)} file(s) — "
        "one kind per file is pinned by the loader, so the reader is broken",
        file=sys.stderr,
    )
    sys.exit(7)
if compared < floor:
    print(
        f"REFUSED\tcompared {compared} kind(s), floor is {floor} — "
        "a comparison this small proves nothing, whatever it found",
        file=sys.stderr,
    )
    sys.exit(7)
PY
}

# Fixtures, then the six refusals the comparator owes. Run on EVERY
# invocation, not behind a flag: on the forge host the live half below
# SKIPS, and without this the gate would exercise none of this code at
# all — a check that is not running (§Diagnosis). It is hermetic and
# costs one python per case.
self_test() {
    local t rc out
    t=$(mktemp -d) || return 1
    mkdir -p "$t/bundle"
    cat > "$t/bundle/alpha.toml" <<'FX'
[[workflow]]
kind = "alpha"
label = "Alpha"
category = "platform"
owning_team = "platform"
description = "The first protocol."
FX
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."
FX
    printf '%s' '[{"kind":"alpha","version":1,"status":"active","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."},
                  {"kind":"beta","version":3,"status":"active","label":"Beta","category":"platform","owning_team":"platform","description":"The second protocol."}]' > "$t/match.json"
    printf '%s' '[{"kind":"alpha","version":1,"status":"active","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."},
                  {"kind":"beta","version":3,"status":"active","label":"Beta","category":"platform","owning_team":"platform","description":"The second protocol, as described by a service deleted last week."}]' > "$t/drift.json"
    printf '%s' '[{"kind":"gamma","version":1,"status":"active","label":"Gamma","description":"Another world entirely."}]' > "$t/elsewhere.json"
    printf '%s' '[{"kind":"alpha","version":1,"status":"retired","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."}]' > "$t/retired.json"
    printf '%s' 'not json at all' > "$t/garbage.json"

    local FIELD_FLOOR_SAVED="$FIELD_FLOOR"
    FIELD_FLOOR=2

    # 1. Agreement is silence — and it still says how much it compared.
    out=$(fields_report "$t/bundle" "$t/match.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: matching fixtures exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    printf '%s\n' "$out" | grep -qF "COUNTS	parsed=2	compared=2	drifted=0" \
        || { echo "self-test FAILED: matching fixtures did not report 2 compared / 0 drifted: $out" >&2; rm -rf "$t"; return 1; }

    # 2. THE RED THIS CHECK EXISTS FOR: one description differs, and the
    #    finding must NAME the kind and the field.
    out=$(fields_report "$t/bundle" "$t/drift.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: a drifting description exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    printf '%s\n' "$out" | grep -qF "DRIFT	beta	description	v3" \
        || { echo "self-test FAILED: the drift was not named by kind, field and live version: $out" >&2; rm -rf "$t"; return 1; }
    printf '%s\n' "$out" | grep -qF "drifted=1" \
        || { echo "self-test FAILED: the drift was not counted: $out" >&2; rm -rf "$t"; return 1; }
    printf '%s\n' "$out" | grep -qF "deleted last week" \
        || { echo "self-test FAILED: the finding carries no excerpt of the live text: $out" >&2; rm -rf "$t"; return 1; }

    # 3. A FIELD THE FILE DOES NOT CLAIM is named rather than quietly
    #    dropped — the same vacuity one field down.
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
FX
    out=$(fields_report "$t/bundle" "$t/drift.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: an absent claim exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    printf '%s\n' "$out" | grep -qF "ABSENT	beta	description" \
        || { echo "self-test FAILED: a file claiming no description was not named: $out" >&2; rm -rf "$t"; return 1; }
    printf '%s\n' "$out" | grep -qF "drifted=0" \
        || { echo "self-test FAILED: an absent claim was counted as drift: $out" >&2; rm -rf "$t"; return 1; }
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."
FX

    # 4. THE FLOOR. A registry answering about kinds this bundle does
    #    not hold finds no drift, which must never read as clean.
    out=$(fields_report "$t/bundle" "$t/elsewhere.json" 2>&1); rc=$?
    [ "$rc" -eq 7 ] || { echo "self-test FAILED: a zero-kind comparison exited $rc, expected 7: $out" >&2; rm -rf "$t"; return 1; }
    printf '%s\n' "$out" | grep -qF "REFUSED" \
        || { echo "self-test FAILED: the floor refusal does not say so: $out" >&2; rm -rf "$t"; return 1; }

    # 5. A retired row is not an active one — it must not stand in for
    #    the comparison, and here that empties it below the floor.
    out=$(fields_report "$t/bundle" "$t/retired.json" 2>&1); rc=$?
    [ "$rc" -eq 7 ] || { echo "self-test FAILED: a retired-only row exited $rc, expected the floor's 7: $out" >&2; rm -rf "$t"; return 1; }

    # 6. An answer that is not JSON is no answer.
    out=$(fields_report "$t/bundle" "$t/garbage.json" 2>&1); rc=$?
    [ "$rc" -eq 3 ] || { echo "self-test FAILED: an unparseable live answer exited $rc, expected 3: $out" >&2; rm -rf "$t"; return 1; }

    # 7. A bundle file the reader cannot read is a broken reader, not an
    #    agreeing protocol.
    printf '%s' 'kind = "gamma"' > "$t/bundle/gamma.toml"
    out=$(fields_report "$t/bundle" "$t/match.json" 2>&1); rc=$?
    [ "$rc" -eq 6 ] || { echo "self-test FAILED: a file with no [[workflow]] exited $rc, expected 6: $out" >&2; rm -rf "$t"; return 1; }
    rm -f "$t/bundle/gamma.toml"

    FIELD_FLOOR="$FIELD_FLOOR_SAVED"

    # 8. THE UNREACHABLE CASE, end to end, because it is the one that
    #    must never read as success. `--require-live` is the mode for a
    #    caller with somewhere to put the answer, and it must exit 75
    #    (EX_TEMPFAIL) rather than 0 when the registry cannot be read.
    #    A link-local port nothing listens on: refused in ~6ms, and no
    #    DNS lookup, so the resolver flake cannot make this hang.
    if [ -z "${BOSS_LINT_SELFTEST_CHILD:-}" ]; then
        out=$(BOSS_LINT_SELFTEST_CHILD=1 BOSS_JOBS_URL="http://[::1]:9" bash "$0" --require-live 2>&1); rc=$?
        [ "$rc" -eq 75 ] || { echo "self-test FAILED: --require-live against an unreachable registry exited $rc, expected 75: $out" >&2; rm -rf "$t"; return 1; }
        printf '%s\n' "$out" | grep -qF "SKIPPED the live comparison" \
            || { echo "self-test FAILED: the skip is not loud: $out" >&2; rm -rf "$t"; return 1; }
        printf '%s\n' "$out" | grep -qF "[::1]:9" \
            || { echo "self-test FAILED: the skip does not name what it could not reach: $out" >&2; rm -rf "$t"; return 1; }
        printf '%s\n' "$out" | grep -qi "OK —" \
            && { echo "self-test FAILED: a skip printed an OK line: $out" >&2; rm -rf "$t"; return 1; }
    fi

    rm -rf "$t"
    [ "$SELF_TEST" -eq 1 ] && echo "$NAME: self-test ok — agreement is silent and counted, a drifting description is named by kind/field/version with an excerpt, a field the file does not claim is named and not counted as drift, a comparison below the floor and a retired-only row are refused, an unparseable answer and an unreadable bundle file each refuse distinctly, and --require-live exits 75 naming the target it could not reach"
    return 0
}

self_test || exit 1
[ "$SELF_TEST" -eq 0 ] || exit 0

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

# Home 3: the Rust literals inside `platform_workflows()`. EMPTY since
# 2026-09-11, and the check below is what keeps it that way.
#
# This home was always a tolerated waypoint, never a destination: a kind
# here is a protocol that cannot be changed without building and
# shipping a binary, which is CLAUDE.md's own definition of a protocol
# that has leaked into the substrate. The last four left on 2026-09-11
# (`design-doc-review` and the three `maintenance_spec` chores), and the
# concrete cost of the form is on the record — `maintenance_spec` put its
# description in the CATEGORY column for all three, and because
# `bootstrap_reconcile` re-asserts a code spec on every boot the wrong
# value could not drift back on its own and every new deployment
# reproduced it (6c796f75).
#
# So the roster is now checked EMPTY rather than merely scraped. The
# body is read between the signature and the first line closing it at
# column 0; two entry shapes are recognised — a kebab-case string
# literal (`maintenance_spec("maintenance-backup", …)`) and a
# no-argument `<name>_spec()` call, whose kind is its name with
# underscores as dashes (`design_doc_review_spec()` →
# `design-doc-review`) — and they still feed the authored set, so a
# literal put back tomorrow is counted as authoring and NOT reported as
# an unauthored live kind. It fails on the narrower ground that a
# protocol belongs in the bundle.
#
# An empty roster must also LOOK empty, because "the scrape saw nothing"
# and "there is nothing to see" have to stay distinguishable: the body
# with comments and whitespace stripped is required to be exactly
# `vec![]`. That is what keeps this honest against an entry shape the
# two patterns above do not recognise — such an entry leaves the body
# non-empty, and a non-empty body is a failure whether or not a kind
# name could be read out of it.
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

# The roster's body, comments and whitespace gone. `absent` means the
# signature itself could not be found — a renamed function, which is a
# broken scrape and not an empty roster.
roster_body=$(
    LC_ALL=C awk '
        /^pub fn platform_workflows\(\)/ { inside = 1; found = 1; next }
        inside && /^\}/                  { inside = 0 }
        inside {
            line = $0
            sub(/\/\/.*$/, "", line)
            gsub(/[ \t]/, "", line)
            body = body line
        }
        END { if (!found) print "absent"; else print body }
    ' "$REGISTRY_RS"
)
if [ "$roster_body" = "absent" ]; then
    fail "could not find \`pub fn platform_workflows()\` in $REGISTRY_RS — the scrape \
broke, so a green result would mean nothing"
elif [ "$roster_body" != "vec![]" ]; then
    n=$(printf '%s\n' "$code_kinds" | LC_ALL=C sed '/^$/d' | wc -l | tr -d ' ')
    fail "platform_workflows() authors $n workflow kind(s) as Rust literals:"
    printf '    %s\n' ${code_kinds:+$code_kinds} >&2
    echo "" >&2
    echo "  A protocol here cannot be changed without building and shipping a" >&2
    echo "  binary — CLAUDE.md's own definition of a protocol that has leaked" >&2
    echo "  into the substrate — and bootstrap_reconcile RE-ASSERTS it on every" >&2
    echo "  boot, so an operator's edit to the live row is reverted at the next" >&2
    echo "  pod roll and a wrong value cannot drift back (6c796f75)." >&2
    echo "" >&2
    echo "  Move it to $BUNDLE/<kind>.toml, rendered from the live row, and" >&2
    echo "  delete the literal. The row the deployment already has is NOT" >&2
    echo "  touched: the seed is insert-if-missing, so the kind keeps its" >&2
    echo "  current version and every in-flight packet keeps its spec — it" >&2
    echo "  simply stops being reconciled. See $BUNDLE/README.md." >&2
    echo "" >&2
    echo "  The roster must also READ as empty — \`vec![]\` with nothing but" >&2
    echo "  comments — so that an entry shape this script does not recognise" >&2
    echo "  still fails here instead of passing silently." >&2
fi

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
    echo "  NOTHING IS CLAIMED about what the running registry admits, or about" >&2
    echo "  whether any live row still says what its file says — the field" >&2
    echo "  comparison did not run, so ZERO kinds were compared." >&2
    [ "$problems" -eq 0 ] || exit 1
    # A caller with somewhere to put the answer runs `--require-live`,
    # and for it "I could not read the registry" must not be the same
    # exit as "I read it and it agrees". 75 is EX_TEMPFAIL, the code
    # this tree already uses for a run that could not happen rather
    # than one that failed (infra/gate-runner/run.sh, checkout-lock.sh).
    # The bare invocation keeps exiting 0: the gate runs on the forge
    # host, which has no route to the in-cluster read surface, and a
    # lint that reds there would red every car for an infrastructure
    # refusal that says nothing about the branch.
    [ "$REQUIRE_LIVE" -eq 0 ] || exit 75
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

# ---------------------------------------------------------------------------
# Field half — a live row still says what its FILE says.
# ---------------------------------------------------------------------------
# The half above asks whether the tree can DESCRIBE every live protocol.
# This one asks whether the description is still TRUE, which is a
# different question with its own measured instance: design-doc-review's
# live v1 `description` named `boss-docs-api`, `/api/design/pending-
# decisions` and the flush jobs — every one deleted on 2026-09-10 — while
# the tree file carried the corrected text, and TWO mechanisms each
# declined to notice. The bundle seed is insert-if-missing, so a present
# row is skipped whole. `bootstrap_reconcile`'s `kind_body_matches`
# excludes `description` as cosmetic, so it saw no drift and
# republished nothing (backlog e882b74c).
#
# NOT A CASE FOR WIDENING `kind_body_matches`, and the reason is
# stronger than the churn argument that excluded the field: since
# 2026-09-11 `platform_workflows()` is EMPTY, so reconcile iterates
# nothing and touches none of these rows. Widening its comparison
# today would change behaviour only for rows this problem is not
# about, and would still compare nothing here. It is also worth being
# exact about what that function already covers — `label`, `category`
# and `owning_team` ARE in it, and `description` alone is not — which
# is why all three are compared here: for a bundle-authored kind,
# nothing compares any of them.
#
# REPORTED, NEVER FAILED ON, in the bare invocation. The direction is
# the same legitimate window the half above tolerates, read one field
# deeper: a car edits a description in the tree, merges, and the live
# row does not change until an operator publishes a new version. Failing
# would red every car from that merge until the publish — the churn
# argument that excluded the field from reconcile, re-arriving as a red
# gate. `--require-live` is the mode for a reader that can act: there a
# drift exits 2.
fields_out=""
fields_rc=0
if [ -n "$live_kinds" ]; then
    fields_out=$(fields_report "$BUNDLE" "$body" 2>&1)
    fields_rc=$?
fi
case "$fields_rc" in
    0) ;;
    3) fail "the live answer could not be read for the field comparison: \
$(printf '%s' "$fields_out" | head -1)"
       echo "" >&2
       echo "  The kind comparison above read this same body, so this is a shape" >&2
       echo "  change, not an unreachable registry. Nothing was compared." >&2 ;;
    6) fail "a bundle file under $BUNDLE could not be read: \
$(printf '%s' "$fields_out" | head -1)"
       echo "" >&2
       echo "  One [[workflow]] per file, named for the kind — the loader pins" >&2
       echo "  it (platform_bundle.rs) and this reader needs it too." >&2 ;;
    7) fail "the field comparison refused rather than report a vacuous clean bill:"
       printf '    %s\n' "$(printf '%s' "$fields_out" | sed -n 's/^REFUSED\t//p')" >&2
       echo "" >&2
       echo "  A comparison of nothing finds no drift, which reads exactly like" >&2
       echo "  agreement. Either the bundle reader broke or the registry is" >&2
       echo "  answering about a different world; in both cases a green here" >&2
       echo "  would be the confident wrong answer (CLAUDE.md §Doors)." >&2 ;;
    *) fail "the field comparison exited $fields_rc: $(printf '%s' "$fields_out" | head -1)" ;;
esac

drift_lines=$(printf '%s\n' "$fields_out" | LC_ALL=C sed -n 's/^DRIFT\t//p')
fields_compared=$(printf '%s\n' "$fields_out" | LC_ALL=C sed -n 's/.*\tcompared=\([0-9]*\).*/\1/p' | head -1)
fields_compared=${fields_compared:-0}
drift_n=0
if [ -n "$drift_lines" ]; then
    drift_n=$(printf '%s\n' "$drift_lines" | wc -l | tr -d ' ')
    echo "$NAME: $drift_n operator-facing field(s) where the live row disagrees with its file:" >&2
    printf '%s\n' "$drift_lines" | while IFS=$'\t' read -r kind field ver at tlen llen twin lwin; do
        echo "    $kind.$field — live $ver, first differs $at ($tlen vs $llen chars)" >&2
        echo "      file: $twin" >&2
        echo "      live: $lwin" >&2
    done
    echo "" >&2
    echo "  A description is what an operator reads to know what a protocol is" >&2
    echo "  for, so a stale one sends somebody looking for a surface that may" >&2
    echo "  not exist — which is exactly what design-doc-review's live v1 did" >&2
    echo "  (backlog e882b74c). Clear one by publishing a new version FROM the" >&2
    echo "  file, which is the only write that moves a live row:" >&2
    echo "" >&2
    echo "    boss workflow publish <kind> $BUNDLE/<kind>.toml" >&2
    echo "" >&2
    echo "  In-flight packets are safe: publish adds a version, and every open" >&2
    echo "  Job stays pinned to the one it was admitted under." >&2
    echo "" >&2
    echo "  If the FILE is the wrong copy, fix the file — but do not retype a" >&2
    echo "  sentence you know to be false to make this quiet. The tree carrying" >&2
    echo "  the corrected text while the row lags is the safe direction, and it" >&2
    echo "  is the state this check exists to make visible rather than to" >&2
    echo "  forbid." >&2
fi

# A file that makes NO claim about a field is not drift — the row can
# hardly disagree with a sentence nobody wrote — but it is the quiet
# half of the same gap: nothing would compare that field ever again.
# Counted and named, because silence that nobody can see is how this
# class of defect gets in.
absent_lines=$(printf '%s\n' "$fields_out" | LC_ALL=C sed -n 's/^ABSENT\t//p')
if [ -n "$absent_lines" ]; then
    echo "  $(printf '%s\n' "$absent_lines" | wc -l | tr -d ' ') field(s) the file does not claim, and so cannot be compared:" >&2
    printf '%s\n' "$absent_lines" | while IFS=$'\t' read -r kind field; do
        echo "    $BUNDLE/$kind.toml has no \`$field\`" >&2
    done
fi

[ "$problems" -eq 0 ] || exit 1

# A drift is a verdict only for a caller that can act on it.
if [ "$drift_n" -gt 0 ] && [ "$REQUIRE_LIVE" -eq 1 ]; then
    echo "$NAME: $fields_compared kinds compared, $drift_n field(s) adrift — exiting 2 because --require-live was asked for a verdict" >&2
    exit 2
fi

live_n=$(printf '%s\n' "$live_kinds" | wc -l | tr -d ' ')
authored_n=$(printf '%s\n' "$authored" | wc -l | tr -d ' ')
if [ "$drift_n" -eq 0 ]; then
    msg="$NAME: OK — $live_n admitted kinds, $authored_n authored, $fields_compared live rows agree with their file"
else
    msg="$NAME: $live_n admitted kinds, $authored_n authored — $drift_n operator-facing field(s) adrift across $fields_compared compared (named above; REPORTED, not failed)"
fi
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
