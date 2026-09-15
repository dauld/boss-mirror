#!/usr/bin/env bash
#
# publish-workflow — put the tree's row for one platform workflow kind
# live in the registry, from the converged checkout on boss-gcp, with
# the refusals that make it safe to hand to a button.
#
# WHY IT EXISTS (backlog 3ce95b85, car 3a of 8f4e9cc0)
# ---------------------------------------------------------------------
# A change to an EXISTING platform workflow kind goes live only when an
# operator runs `boss workflow publish <kind> <toml>` on a host with the
# checkout — the seed is insert-if-missing by decision. That publish is
# the one act that lands a data car for an existing kind, and until this
# verb it had no door but an operator's terminal: the daily drift
# measurement (19dec171) and the Drift tab (#378) show the drift and
# offer nothing, because the server has no tree. CLAUDE.md §Doors: an
# ops verb is a path made safe — correct target, correct actor,
# pre-approved. David, 2026-09-11: "We might need some sort of doc diff
# view for me to approve." This is the publish that approve fires.
#
# Measured while building it, 2026-09-15, against the system of record:
# sixteen `maintenance-*` kinds carry a `failed` step in the tree that
# the live row lacks — landed on 2026-09-08, never published — while
# `ship-a-change`'s live v31 carries `procedure` texts and a required
# `proof` field that no revision of the tree ever said, so publishing
# the tree over it would REGRESS the protocol. A publish verb that
# cannot tell those two apart is a footgun with a button on it.
#
# THE RULE
# ---------------------------------------------------------------------
# The live row is safe to overwrite iff it is a row THE TREE ONCE SAID:
# some revision of the bundle under infra/platform/ renders to the same
# label, description, category, subject_kinds and steps (title, kind,
# ready_when, title_template, authority_role, metadata_defaults, and
# each field's name/type/required). Then the tree moved ahead and live
# lags — publish. Otherwise the live row carries an edit the tree lacks:
# REFUSE, name the drift field by field, and publish only when the
# request says `--force-tree`. The walk covers the bundle DIRECTORY's
# history, not one file's, because the bundle was one file until
# 2026-09-08 and `git log --follow` cannot trace a split — the sixteen
# lagging rows match the OLD `workflows.toml`, not any revision of their
# own file (measured: design-doc matched its own file's first revision,
# maintenance-views-catchup matched 8cc7a03e:infra/platform/workflows.toml,
# pr-train and ship-a-change matched nothing).
#
# WHAT IT DOES NOT COMPARE, and why that is not a hole: metadata_schema,
# entitlements, the workflow-level `metadata`, sign_offs_required. They
# are part of the row `boss workflow publish` sends, so they GO LIVE
# with a publish; they are left out of the equality only, so a tree that
# changed nothing but those reads as "nothing to publish" — and
# `--force-tree` publishes anyway, saying so. Widening the view is one
# edit to `row_view` below.
#
# THE SEQUENCE, each step checked rather than assumed
# ---------------------------------------------------------------------
#   1. the kind's file exists in the tree               else exit 3
#   1b. the host's CLI can read a bundle: `boss --version` names a
#      commit at or after the loader's arrival (the floor) else exit 78
#   2. the live active row is read                       else exit 75 / 8
#   3. tree vs live: equal is nothing to publish         else exit 5
#      differing: the history walk decides; a live row the tree never
#      said is refused unless --force-tree               else exit 6
#   4. `boss workflow publish <kind> <file> --dry-run` lints the row and
#      refuses a dirty registry (a stale draft)          else exit 4
#   5. --check stops here, exit 0, saying what it would do
#   6. `boss workflow publish <kind> <file>` — the CLI's own checked
#      sequence: create the draft, read it back, publish, confirm
#   7. the live row is read back and compared to the tree AGAIN; the
#      version must have moved and the view must be equal   else exit 7
#
# The binary is the CLI's — the TOML is read by `boss_jobs::seed_loader`,
# the same loader the seed uses, so the row this publishes and the row
# a fresh database seeds are one definition (§9a); a shell re-derivation
# of that loader would be a second copy of it. The binary on boss-gcp is
# the one `deploy-services.sh prod` installs at /usr/local/bin/boss; a
# missing one is a configuration refusal naming that, never an ENOENT
# dressed as a verdict.
#
# AND A STALE ONE IS REFUSED BY NAME (backlog fec2851f). The first live
# run (ops-request 25cb2f71, 2026-09-15 16:36Z) reached step 4 with a
# host CLI older than the loader: it rejected the kind file as "is not
# JSON" — the branch an older `load_spec` falls into for any path —
# and this script reported exit 4, "the tree's file does not lint
# clean", about a file that lints clean. Nothing refreshes that binary
# (boss-gcp-converge installs units only; `prod` is a deliberate human
# run), so the misnamed refusal would have sent an operator to the
# tree every time. Now `boss --version` is read first: the commit it
# names must sit in this checkout's history at or after CLI_FLOOR —
# the train that taught the CLI to read a bundle — else the refusal
# names the binary, its commit, the floor and the refresh path, as a
# host-configuration fault (78, where "no boss CLI" lives). A binary
# that cannot say (`unknown`) or names a commit this checkout does not
# hold is refused the same way: unplaceable is not assumed current.
#
# USAGE
#   publish-workflow.sh <kind> [--check | --force-tree]
#
# ENV
#   BOSS_JOBS_URL                (required) the system of record
#   BOSS_PUBLISH_WORKFLOW_REPO   the checkout whose bundle and history
#                                are read (default: the one this script
#                                is in — /opt/boss on boss-gcp)
#   BOSS_BIN                     the CLI (default: `boss` on PATH, else
#                                /usr/local/bin/boss)
#   BOSS_PUBLISH_CLI_FLOOR       the oldest commit a usable CLI may be
#                                built from (default CLI_FLOOR_DEFAULT
#                                below; the test fixture's history has
#                                its own)
#   BOSS_ACTOR                   who the publish signs as (default
#                                automation:ops-runner — the account
#                                RUNNING it, the identity rule every
#                                `boss` verb applies; the approving
#                                human is the ops-request's filer)
#   OPS_REQUEST_ID               the packet, from the runner; printed
#   BOSS_PUBLISH_HISTORY_LIMIT   commits walked at most (default 500)
#
# EXIT
#   0  published and confirmed (or, with --check, would publish)
#   2  usage: a kind outside ^[a-z][a-z0-9-]{1,60}$, or a foreign mode
#   3  REFUSED: the tree has no infra/platform/workflows/<kind>.toml
#   4  REFUSED: the tree's row does not lint / the registry is dirty
#   5  REFUSED: nothing to publish — live already equals the tree
#   6  REFUSED: the live row carries what the tree never said
#   7  the publish ran and was NOT confirmed by the read-back
#   8  the kind has no live active row (the seed admits new kinds)
#  75  cannot answer: the registry could not be read
#  78  configuration: no BOSS_JOBS_URL, no `boss`, a `boss` older than
#      the bundle loader (or of unknown provenance), no python3/tomllib
#
# Runs as root under the ops-runner with NO HOME; every git read drops
# to the checkout's owner (root cannot even READ a checkout it does not
# own — "dubious ownership", measured on ops-request c9877f75).
set -uo pipefail

NAME="publish-workflow"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '%s: %s\n' "$NAME" "$*"; }
refuse() { # <exit> <message...>
    local rc="$1"; shift
    printf '%s: REFUSED — %s\n' "$NAME" "$*"
    exit "$rc"
}

usage() {
    sed -n '/^# USAGE/,/^# EXIT/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2
}

# --- arguments ------------------------------------------------------------
KIND="${1:-}"
MODE="${2:-}"
if [ -z "$KIND" ] || [ $# -gt 2 ]; then
    echo "$NAME: usage: $(basename "$0") <kind> [--check | --force-tree]" >&2
    usage; exit 2
fi
# The allowlist's pattern, applied again here: the runner is one reader
# of it and this script is another, and a script that trusts its caller
# is a script that is one caller away from `../`.
if ! printf '%s' "$KIND" | grep -Eqx -- '^[a-z][a-z0-9-]{1,60}$'; then
    echo "$NAME: usage: kind '$KIND' is not a workflow kind (^[a-z][a-z0-9-]{1,60}$)" >&2
    exit 2
fi
case "$MODE" in
    ""|--check|--force-tree) ;;
    *)
        echo "$NAME: usage: mode '$MODE' is not one of --check, --force-tree" >&2
        exit 2 ;;
esac

# --- configuration --------------------------------------------------------
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$NAME: BOSS_JOBS_URL is not set, and there is no safe default — a publish against the wrong instance answers instead of erroring (CLAUDE.md §Doors). The ops-runner's unit pins it." >&2
    exit 78
fi
for tool in jq curl python3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$NAME: no $tool on PATH — nothing compared, nothing published" >&2; exit 78; }
done
python3 -c 'import tomllib' 2>/dev/null || { echo "$NAME: python3 has no tomllib (3.11+) — the bundle cannot be read, so nothing compared" >&2; exit 78; }

# The floor: train #268 (2026-09-08), where `load_spec` in
# crates/orchestrators/boss-cli/src/workflow.rs began reading a `.toml`
# path through boss_jobs::seed_loader. A CLI built from any commit
# before it cannot read what this verb publishes. One constant, pinned
# by crates/core/boss-testing/tests/publish_workflow_sh.rs to the
# commit whose workflow.rs first calls the loader.
CLI_FLOOR_DEFAULT="c17827f37b3a464c7e2377281ff3dd8206f6ee8a"
CLI_FLOOR="${BOSS_PUBLISH_CLI_FLOOR:-$CLI_FLOOR_DEFAULT}"

BOSS_BIN="${BOSS_BIN:-}"
if [ -z "$BOSS_BIN" ]; then
    if command -v boss >/dev/null 2>&1; then BOSS_BIN="$(command -v boss)"; else BOSS_BIN=/usr/local/bin/boss; fi
fi
if [ ! -x "$BOSS_BIN" ]; then
    echo "$NAME: no boss CLI at $BOSS_BIN — the publish IS the CLI's checked sequence (boss workflow publish), and this host has none installed. deploy-services.sh prod installs it as /usr/local/bin/boss; until then this verb cannot act." >&2
    exit 78
fi

REPO="${BOSS_PUBLISH_WORKFLOW_REPO:-$SELF_DIR/../..}"
REPO="$(cd "$REPO" 2>/dev/null && pwd)" || { echo "$NAME: checkout ${BOSS_PUBLISH_WORKFLOW_REPO:-$SELF_DIR/../..} is not a directory this process can enter" >&2; exit 78; }
BUNDLE_REL="infra/platform/workflows"
KIND_FILE_REL="$BUNDLE_REL/$KIND.toml"
KIND_FILE="$REPO/$KIND_FILE_REL"
HISTORY_LIMIT="${BOSS_PUBLISH_HISTORY_LIMIT:-500}"

# Who the publish signs as: the account running it. The runner signs
# its own SoR calls as automation:ops-runner; the `boss` binary refuses
# an unnamed write, and there is no HOME here for the actor file.
export BOSS_ACTOR="${BOSS_ACTOR:-automation:ops-runner}"

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# --- 1. the tree authors the kind -----------------------------------------
if [ ! -f "$KIND_FILE" ]; then
    refuse 3 "the tree has no $KIND_FILE_REL (checkout $REPO), so there is nothing to publish for '$KIND'. A NEW kind is admitted by the seed (insert-if-missing) once its file lands; this verb republishes kinds the tree already authors."
fi

# --- git, as the checkout's owner -------------------------------------------
OWNER="$(stat -c %U "$REPO" 2>/dev/null)"
OWNER_UID="$(stat -c %u "$REPO" 2>/dev/null)"
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ]; then
    refuse 78 "cannot resolve the owner of $REPO (stat says '${OWNER:-}', uid ${OWNER_UID:-?}) — no passwd entry, so there is no account to read git as"
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}

# --- 1b. the host's CLI can read what it will publish ---------------------------
# `boss --version` prints `boss <crate> built from <sha>` (built_from.rs).
# Read before the registry: a host that cannot act is a host fault, and
# an operator should not be sent to the SoR or the tree for it.
# Captured whole, first line taken in the shell: an external producer
# piped into an early-exiting reader is the SIGPIPE coin-flip the lint
# no-lint-flips-a-coin-on-an-external-producer refuses (2026-09-15).
BOSS_VERSION_LINE="$("$BOSS_BIN" --version 2>&1)"
BOSS_VERSION_LINE="${BOSS_VERSION_LINE%%$'\n'*}"
BUILT_FROM="$(printf '%s\n' "$BOSS_VERSION_LINE" | sed -n 's/.*built from \([0-9a-f]\{7,40\}\|unknown\).*/\1/p')"
REFRESH="the binary is refreshed by deploy-services.sh prod on this host (boss-gcp-converge installs units only)"
case "${BUILT_FROM:-}" in
    "")
        refuse 78 "$BOSS_BIN cannot say what it was built from ('$BOSS_VERSION_LINE' names no commit), so whether it reads a bundle is unknown and is not assumed; $REFRESH" ;;
    unknown)
        refuse 78 "$BOSS_BIN cannot say what it was built from (built from unknown — compiled without git and no BOSS_BUILD_COMMIT), so whether it reads a bundle is unknown and is not assumed; $REFRESH" ;;
esac
if ! as_owner "git -C '$REPO' cat-file -e '$BUILT_FROM^{commit}'" 2>/dev/null; then
    refuse 78 "$BOSS_BIN is built from $BUILT_FROM, a commit not in the history of $REPO, so it cannot be placed before or after the bundle loader ($CLI_FLOOR); an unplaceable binary is not assumed current. $REFRESH"
fi
if ! as_owner "git -C '$REPO' merge-base --is-ancestor '$CLI_FLOOR' '$BUILT_FROM'" 2>/dev/null; then
    refuse 78 "$BOSS_BIN is built from ${BUILT_FROM:0:8}, older than the bundle loader (floor ${CLI_FLOOR:0:8}, train #268 2026-09-08): it would reject $KIND_FILE_REL as not JSON, a fault of the host and not of the tree. $REFRESH"
fi
say "boss: $BOSS_BIN built from ${BUILT_FROM:0:8} (floor ${CLI_FLOOR:0:8} reached)"

# --- the comparator -----------------------------------------------------------
# `python3 compare.py <toml> <live.json> <kind>`: exit 0 and print EQUAL
# when the kind's row in the TOML renders to the live row's view, else
# exit 1 and print one `DIFF <path> <tree> <live>` line per differing
# field with a window of each side (the lint's excerpt idiom — both full
# copies stay readable at their homes, so nothing only-copy is lost).
# Exit 3 when the TOML does not parse or lacks the kind (a historical
# revision may legitimately lack it).
cat > "$TMP/compare.py" <<'PY'
import json, sys, tomllib

def norm(v):
    # The registry hands back null where the file has no key and [] / {}
    # where it has an empty one; the file never distinguishes them.
    return None if v in (None, [], {}, "") else v

def step_view(s):
    return {
        "title": s.get("title"), "kind": s.get("kind"), "ready_when": s.get("ready_when"),
        "title_template": norm(s.get("title_template")),
        "authority_role": norm(s.get("authority_role")),
        "metadata_defaults": norm(s.get("metadata_defaults")),
        "fields": norm([[f.get("name"), f.get("field_type"), bool(f.get("required", False))]
                        for f in (s.get("fields") or [])]),
    }

def row_view(r, steps_key):
    return {"label": r.get("label"), "description": r.get("description"),
            "category": r.get("category"), "subject_kinds": norm(r.get("subject_kinds")),
            "steps": [step_view(s) for s in (r.get(steps_key) or [])]}

def window(v, width=80):
    s = json.dumps(v, ensure_ascii=False) if not isinstance(v, str) else v
    s = s.replace("\n", "\\n")
    return s if len(s) <= width else s[:width] + "…"

def diffs(a, b, path=""):
    if isinstance(a, dict) and isinstance(b, dict):
        for k in a:
            yield from diffs(a[k], b.get(k), f"{path}.{k}" if path else k)
        return
    if isinstance(a, list) and isinstance(b, list) and path.endswith("steps"):
        for i in range(max(len(a), len(b))):
            x = a[i] if i < len(a) else None
            y = b[i] if i < len(b) else None
            if x is None or y is None:
                yield (f"{path}[{i}]", x, y)
            else:
                yield from diffs(x, y, f"{path}[{i}]")
        return
    if a != b:
        yield (path, a, b)

toml_path, live_path, kind = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    with open(toml_path, "rb") as fh:
        doc = tomllib.load(fh)
except Exception as e:
    print(f"UNREADABLE {toml_path}: {e}")
    sys.exit(3)
rows = [w for w in (doc.get("workflow") or []) if isinstance(w, dict) and w.get("kind") == kind]
if not rows:
    print(f"NOKIND {toml_path}")
    sys.exit(3)
with open(live_path) as fh:
    live = json.load(fh)
a, b = row_view(rows[0], "step"), row_view(live, "steps")
if a == b:
    print("EQUAL")
    sys.exit(0)
for path, x, y in diffs(a, b):
    print(f"DIFF\t{path}\ttree={window('<absent>' if x is None else x)}\tlive={window('<absent>' if y is None else y)}")
sys.exit(1)
PY

# --- 2. the live active row ---------------------------------------------------
# Unauthenticated, like the drift lint's read: /api/workflows is a public
# registry surface and answered 86 rows to a bare GET on 2026-09-15.
read_live() { # <out file>; exit 0 with the active row written, 75 otherwise
    local body="$TMP/live.raw" code
    code=$(curl -sS -m 20 -o "$body" -w '%{http_code}' "$BOSS_JOBS_URL/api/workflows/$KIND" 2>"$TMP/curl.err")
    if [ "$code" != "200" ]; then
        printf '%s: could not read %s/api/workflows/%s — HTTP %s. %s\n' "$NAME" "$BOSS_JOBS_URL" "$KIND" "${code:-000}" "$(tr '\n' ' ' <"$TMP/curl.err")" >&2
        say "cannot answer: nothing compared, nothing published (a verdict from an unread registry would be the confident wrong answer)"
        return 75
    fi
    # The endpoint answers the active row as an object (or an envelope,
    # or every version as an array — the CLI tolerates all three, so do
    # we): keep the active one.
    if ! jq -e '
        (if type == "object" and has("data") then .data else . end)
        | (if type == "array" then (map(select(.status == "active")) | last) else . end)
        | select(type == "object" and has("version"))' "$body" > "$1" 2>"$TMP/jq.err"; then
        printf '%s: %s/api/workflows/%s answered 200 but no active row with a version could be read from it: %s\n' "$NAME" "$BOSS_JOBS_URL" "$KIND" "$(tr '\n' ' ' <"$TMP/jq.err")" >&2
        return 8
    fi
}
read_live "$TMP/live.json"; rc=$?
case "$rc" in
    0) ;;
    8) refuse 8 "'$KIND' has no live active row at $BOSS_JOBS_URL — the seed admits a new kind (insert-if-missing); this verb republishes one that exists." ;;
    *) exit "$rc" ;;
esac
LIVE_VERSION=$(jq -r '.version' "$TMP/live.json")

# --- 3. tree vs live ------------------------------------------------------------
python3 "$TMP/compare.py" "$KIND_FILE" "$TMP/live.json" "$KIND" > "$TMP/diff.txt"; cmp_rc=$?
case "$cmp_rc" in
    0)
        if [ "$MODE" != "--force-tree" ]; then
            refuse 5 "nothing to publish — the live $KIND v$LIVE_VERSION already says what $KIND_FILE_REL says (label, description, category, subject_kinds and every step compared equal). A publish now would mint an identical version; --force-tree does that anyway, if a field this comparison leaves out (metadata_schema, entitlements) is what changed."
        fi
        say "FORCED: live $KIND v$LIVE_VERSION equals the tree on every compared field; publishing an identical row because the request said --force-tree"
        ;;
    1)
        say "live $KIND v$LIVE_VERSION differs from $KIND_FILE_REL:"
        sed 's/^DIFF\t/  /; s/\t/  /g' "$TMP/diff.txt"
        # THE HISTORY WALK. Every commit that touched infra/platform,
        # newest first; at each, every TOML there that names the kind is
        # rendered and compared to the live row. The first match ends
        # it: live is a row the tree once said.
        matched=""
        walked=0
        if ! as_owner "git -C '$REPO' rev-parse --git-dir" >/dev/null 2>"$TMP/git.err"; then
            say "cannot read $REPO as a git checkout (as '$OWNER'): $(tr '\n' ' ' <"$TMP/git.err")"
            say "so whether the tree ever said the live row cannot be judged; treating it as never said"
        else
            while IFS= read -r sha; do
                [ -n "$sha" ] || continue
                walked=$((walked + 1))
                while IFS= read -r path; do
                    [ -n "$path" ] || continue
                    case "$path" in *.toml) ;; *) continue ;; esac
                    as_owner "git -C '$REPO' show '$sha:$path'" > "$TMP/rev.toml" 2>/dev/null || continue
                    if python3 "$TMP/compare.py" "$TMP/rev.toml" "$TMP/live.json" "$KIND" >/dev/null 2>&1; then
                        matched="${sha:0:8}:$path"
                        break 2
                    fi
                done <<PATHS
$(as_owner "git -C '$REPO' grep -l -F 'kind = \"$KIND\"' '$sha' -- infra/platform" 2>/dev/null | sed 's/^[0-9a-f]*://')
PATHS
            done <<SHAS
$(as_owner "git -C '$REPO' log --format=%H -n '$HISTORY_LIMIT' -- infra/platform" 2>/dev/null)
SHAS
        fi
        if [ -n "$matched" ]; then
            say "the tree moved ahead: live $KIND v$LIVE_VERSION is the row at $matched (found after $walked commit(s)) — live lags the tree, and the publish brings it forward"
        elif [ "$MODE" = "--force-tree" ]; then
            say "FORCED: live $KIND v$LIVE_VERSION matches no revision of infra/platform in $walked commit(s) walked — an edit the tree never said — and the request said --force-tree, so the tree's row is published over the drift named above"
        else
            refuse 6 "live $KIND v$LIVE_VERSION carries what the tree never said: no revision of infra/platform in $walked commit(s) walked renders to it, so the drift named above is an edit made live (a publish from a JSON spec, an authoring surface) that $KIND_FILE_REL lacks. Publishing the tree would erase it. Either write the live edit into the file and land that car, or, if the tree is what should win, file this verb again with --force-tree (the Drift tab's approve)."
        fi
        ;;
    *)
        say "the tree's $KIND_FILE_REL could not be read as a [[workflow]] file for '$KIND':"
        sed 's/^/  /' "$TMP/diff.txt"
        refuse 4 "the row does not parse, so nothing was compared or published"
        ;;
esac

# --- 4. lint, and refuse a dirty registry, without writing -----------------------
if ! "$BOSS_BIN" workflow publish "$KIND" "$KIND_FILE" --dry-run > "$TMP/lint.out" 2>&1; then
    say "boss workflow publish --dry-run refused the tree's row; its output, whole:"
    sed 's/^/  /' "$TMP/lint.out"
    refuse 4 "the tree's $KIND_FILE_REL does not lint clean (or the registry holds a stale draft — see above), so nothing was written"
fi
say "$(grep -m1 'lints clean' "$TMP/lint.out" || tail -n1 "$TMP/lint.out")"

# --- 5. --check stops here ------------------------------------------------------------
if [ "$MODE" = "--check" ]; then
    say "--check ok: would publish $KIND_FILE_REL over live v$LIVE_VERSION at $BOSS_JOBS_URL (nothing written; packet ${OPS_REQUEST_ID:-none})"
    exit 0
fi

# --- 6. the publish — the CLI's own checked sequence --------------------------------
say "publishing $KIND_FILE_REL as $BOSS_ACTOR for packet ${OPS_REQUEST_ID:-none} (boss: $BOSS_BIN)"
if ! "$BOSS_BIN" workflow publish "$KIND" "$KIND_FILE" > "$TMP/publish.out" 2>&1; then
    say "boss workflow publish failed; its output, whole:"
    sed 's/^/  /' "$TMP/publish.out"
    say "NOT CONFIRMED — the CLI did not report $KIND live; read GET $BOSS_JOBS_URL/api/workflows/$KIND/versions before retrying (a failed create can leave a draft armed — the v16 regression)"
    exit 7
fi
sed 's/^/  /' "$TMP/publish.out"

# --- 7. observed, never assumed ---------------------------------------------------------
read_live "$TMP/after.json" || { say "NOT CONFIRMED — the publish ran but the registry could not be read back (live was v$LIVE_VERSION)"; exit 7; }
NEW_VERSION=$(jq -r '.version' "$TMP/after.json")
case "${NEW_VERSION:-empty}" in empty|*[!0-9]*) say "NOT CONFIRMED — the row read back carries no numeric version (live was v$LIVE_VERSION)"; exit 7 ;; esac
if [ "$NEW_VERSION" -le "$LIVE_VERSION" ]; then
    say "NOT CONFIRMED — the active row is still v$NEW_VERSION after a publish over v$LIVE_VERSION; the CLI's report above is not borne out by the registry"
    exit 7
fi
if ! python3 "$TMP/compare.py" "$KIND_FILE" "$TMP/after.json" "$KIND" > "$TMP/after-diff.txt"; then
    say "NOT CONFIRMED — v$NEW_VERSION is live but does not equal $KIND_FILE_REL:"
    sed 's/^DIFF\t/  /; s/\t/  /g' "$TMP/after-diff.txt"
    exit 7
fi
say "$KIND v$LIVE_VERSION -> v$NEW_VERSION live at $BOSS_JOBS_URL — confirmed by reading the active row back and comparing it to $KIND_FILE_REL (equal); packet ${OPS_REQUEST_ID:-none}"
exit 0
