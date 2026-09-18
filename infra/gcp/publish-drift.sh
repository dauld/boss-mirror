#!/usr/bin/env bash
#
# publish-drift — publish EVERY platform workflow kind the tree moved
# ahead of, as one act, from the converged checkout on boss-gcp; list
# the ones it must not touch, field by field; answer one table.
#
# WHY IT EXISTS (backlog a2f97942, ratified by retro 27fad542)
# ---------------------------------------------------------------------
# `publish-workflow` (the sibling in this directory) is ONE kind per
# request by design — bounded, with the refusals that make it safe to
# hand to a button. On 2026-09-18 13:4x that bound became the operator's
# loop: after the maintenance-audience car landed, 23 publish-workflow
# ops-requests were hand-scripted one per kind (26 in the retro's
# window), preceded by a hand `until --check ok` wait for boss-gcp's
# checkout to carry the train. The gap is one level up: after a train
# lands, every kind the tree moved ahead of should be published as one
# act — the Drift tab already computes the set (protocol-drift.sh
# daily; /api/workflows drift) and its approve fires one publish — and
# the wait should be a verdict, not a loop an operator runs.
#
# THE RULE, INHERITED WHOLE
# ---------------------------------------------------------------------
# This verb re-derives NOTHING. The drift set is `publish-workflow.sh
# <kind> --check` per kind in the bundle, classified by that verb's
# documented exit codes (its EXIT block is the contract; the phrases
# below are pinned to its text by boss-testing/tests/publish_drift_sh.rs):
#
#   0  TREE-AHEAD  live is a row the tree once said, now behind — publish
#   5  EQUAL       nothing to publish — skip
#   6  REFUSED     live carries what the tree NEVER SAID — LISTED, field
#                  by field, copied from the sub-verb; NEVER published
#                  by this verb. There is no --force-tree here: that
#                  decision is the operator's, one kind at a time, at the
#                  Drift tab's approve (publish-workflow --force-tree).
#   4  REFUSED     the tree's row does not lint — listed
#   8  no live row — the seed admits it; listed as pending, skipped
#  75/78           the sub-verb could not answer / is misconfigured —
#                  THIS RUN STOPS with the same code: half a drift set
#                  judged from half a registry is the confident wrong
#                  answer (CLAUDE.md §Doors)
#
# In --for-real, each TREE-AHEAD kind is published in bundle order
# through `publish-workflow.sh <kind>` itself — the CLI's checked
# sequence, the read-back, the actor rule, all its own — and each row
# of the table reports what that verb confirmed. A publish it did NOT
# confirm (7) is reported on its row and makes this verb's exit 1; the
# kinds after it still run, because each kind's publish is independent
# and a stopped loop would hand the operator the loop back.
#
# THE CHECKOUT IS NAMED FIRST, AND A STALE ONE IS `not yet`
# ---------------------------------------------------------------------
# The first line states the sha of the checkout read, so a publish from
# a stale checkout is visible on the packet. Then the newest converged
# train is read off the system of record (the newest CLOSED pr-train
# whose `merged` step carries a `merge_ref`, the way
# infra/forge/landed-train-shas.lib.sh reads it) and must be in this
# checkout's history; otherwise `not yet: checkout at X, main at Y`,
# exit 75, and nothing is asked — the operator's `until --check ok` loop
# becomes a verdict a rule can re-file on the next converge.
#
# USAGE
#   publish-drift.sh [--check | --for-real]      (default: --check)
#
# ENV
#   BOSS_JOBS_URL                (required) the system of record
#   BOSS_PUBLISH_WORKFLOW_REPO   the checkout whose bundle is read — the
#                                SAME variable the sub-verb reads, so the
#                                two cannot read different trees
#                                (default: the one this script is in)
#   BOSS_PUBLISH_WORKFLOW_SH     the sub-verb (default: publish-workflow.sh
#                                beside this script; a test plants one)
#   BOSS_ACTOR                   who the publishes sign as (default
#                                automation:ops-runner, the account
#                                RUNNING it); also signs the train read
#   OPS_REQUEST_ID               the packet, from the runner; printed
#
# EXIT
#   0  the verdict was reached: every tree-ahead kind published and
#      confirmed (or, with --check, listed) — refusals listed count as
#      the operator's decision, not this verb's failure
#   1  at least one publish was NOT confirmed, or a sub-verb run ended
#      outside its documented codes
#   2  usage: a mode outside --check / --for-real
#  75  cannot answer: the checkout is behind the newest converged train
#      (not yet), the system of record could not be read, or a kind's
#      registry row could not
#  78  configuration: no BOSS_JOBS_URL, no jq/curl/git, no sub-verb, no
#      bundle in the checkout
#
# Runs as root under the ops-runner with NO HOME; every git read drops
# to the checkout's owner, as the sibling does.
set -uo pipefail

NAME="publish-drift"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '%s: %s\n' "$NAME" "$*"; }
usage() {
    sed -n '/^# USAGE/,/^# EXIT/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2
}

# --- arguments ------------------------------------------------------------
MODE="${1:---check}"
if [ $# -gt 1 ]; then
    echo "$NAME: usage: $(basename "$0") [--check | --for-real]" >&2
    usage; exit 2
fi
case "$MODE" in
    --check|--for-real) ;;
    *)
        echo "$NAME: usage: mode '$MODE' is not one of --check, --for-real (there is no --force-tree here: a live row the tree never said is the operator's decision at the Drift tab, one kind at a time)" >&2
        exit 2 ;;
esac

# --- configuration --------------------------------------------------------
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$NAME: BOSS_JOBS_URL is not set, and there is no safe default — a publish against the wrong instance answers instead of erroring (CLAUDE.md §Doors). The ops-runner's unit pins it." >&2
    exit 78
fi
for tool in jq curl git; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$NAME: no $tool on PATH — nothing compared, nothing published" >&2; exit 78; }
done

REPO="${BOSS_PUBLISH_WORKFLOW_REPO:-$SELF_DIR/../..}"
REPO="$(cd "$REPO" 2>/dev/null && pwd)" || { echo "$NAME: checkout ${BOSS_PUBLISH_WORKFLOW_REPO:-$SELF_DIR/../..} is not a directory this process can enter" >&2; exit 78; }
# The sub-verb reads the same variable: one checkout, named once.
export BOSS_PUBLISH_WORKFLOW_REPO="$REPO"
BUNDLE_REL="infra/platform/workflows"
BUNDLE="$REPO/$BUNDLE_REL"
[ -d "$BUNDLE" ] || { echo "$NAME: the checkout $REPO has no $BUNDLE_REL directory — there is no bundle to compare" >&2; exit 78; }

SUB="${BOSS_PUBLISH_WORKFLOW_SH:-$SELF_DIR/publish-workflow.sh}"
if [ ! -x "$SUB" ]; then
    echo "$NAME: the sub-verb $SUB is not an executable file — the comparison and the publish are ITS, and this verb re-derives neither" >&2
    exit 78
fi

# The account running it signs the train read and, through the
# sub-verb, every publish — the identity rule every `boss` verb applies.
export BOSS_ACTOR="${BOSS_ACTOR:-automation:ops-runner}"
BOSS_USER="{\"id\":\"$BOSS_ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# --- git, as the checkout's owner -------------------------------------------
OWNER="$(stat -c %U "$REPO" 2>/dev/null)"
OWNER_UID="$(stat -c %u "$REPO" 2>/dev/null)"
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ]; then
    echo "$NAME: cannot resolve the owner of $REPO (stat says '${OWNER:-}', uid ${OWNER_UID:-?}) — no passwd entry, so there is no account to read git as" >&2
    exit 78
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}

# --- the first line: which checkout, at what sha ------------------------------
HEAD="$(as_owner "git -C '$REPO' rev-parse HEAD" 2>"$TMP/git.err")"
if [ -z "$HEAD" ]; then
    echo "$NAME: cannot read $REPO as a git checkout (as '$OWNER'): $(tr '\n' ' ' <"$TMP/git.err")" >&2
    exit 78
fi
say "checkout $REPO at ${HEAD:0:12} — $MODE (packet ${OPS_REQUEST_ID:-none}, as $BOSS_ACTOR)"

# --- freshness: the newest converged train must be in this history --------------
# Signed: an unauthenticated read is handed a smaller world (total: 0)
# rather than an error, so an empty listing below is a failed read and
# never "no train has landed".
TRAINS_URL="$BOSS_JOBS_URL/api/jobs?kind=pr-train&limit=40"
if ! curl -fsS --max-time 20 -H "x-boss-user: $BOSS_USER" \
        ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
        "$TRAINS_URL" > "$TMP/trains.json" 2>"$TMP/curl.err"; then
    say "cannot answer: the system of record did not answer the pr-train read ($TRAINS_URL): $(tr '\n' ' ' <"$TMP/curl.err")"
    say "whether this checkout carries the newest train cannot be judged, so nothing was compared or published"
    exit 75
fi
NEWEST="$(jq -c '
    (if type == "object" and has("data") then .data else . end)
    | map(. as $t
          | ((.steps // []) | map(select(.spec_slug == "merged" and .status == "completed")) | .[0]) as $m
          | select($m != null and (($m.metadata.merge_ref // "") | length) >= 7)
          | {id: $t.id, at: ($m.completed_at // ""), ref: $m.metadata.merge_ref})
    | sort_by(.at) | last // empty' "$TMP/trains.json" 2>/dev/null)"
if [ -z "$NEWEST" ]; then
    n="$(jq -r '(if type == "object" and has("data") then .data else . end) | length' "$TMP/trains.json" 2>/dev/null || echo '?')"
    say "cannot answer: $TRAINS_URL listed no closed pr-train with a merge_ref ($n packet(s) read, signed as $BOSS_ACTOR) — a read that answers no trains is a denied scope as often as an empty world, and neither is a fact this verb may publish on"
    exit 75
fi
TRAIN_ID="$(jq -r .id <<<"$NEWEST")"
TRAIN_AT="$(jq -r .at <<<"$NEWEST")"
MAIN_SHA="$(jq -r .ref <<<"$NEWEST")"
if ! as_owner "git -C '$REPO' cat-file -e '$MAIN_SHA^{commit}' && git -C '$REPO' merge-base --is-ancestor '$MAIN_SHA' HEAD" >/dev/null 2>&1; then
    say "not yet: checkout at ${HEAD:0:8}, main at ${MAIN_SHA:0:8} — the newest converged train ${TRAIN_ID:0:8} merged ${MAIN_SHA:0:8} at ${TRAIN_AT:-?} and this checkout does not carry it yet (boss-gcp-converge fast-forwards it on its next tick); nothing compared, nothing published"
    exit 75
fi
say "fresh: the newest converged train ${TRAIN_ID:0:8} (merged ${MAIN_SHA:0:8} at ${TRAIN_AT:-?}) is in this checkout's history"

# --- the bundle, in its order --------------------------------------------------
# The seed loader reads every *.toml directly in the bundle directory,
# sorted by file name (boss_jobs::seed_loader); the kind IS the file
# name, because the sub-verb looks the kind's file up by that name.
KINDS=()
while IFS= read -r f; do
    [ -n "$f" ] || continue
    k="$(basename "$f" .toml)"
    KINDS+=("$k")
done <<LIST
$(LC_ALL=C ls "$BUNDLE"/*.toml 2>/dev/null)
LIST
if [ "${#KINDS[@]}" -eq 0 ]; then
    echo "$NAME: $BUNDLE_REL holds no *.toml — there is no bundle to compare" >&2
    exit 78
fi
say "bundle: ${#KINDS[@]} kind(s) under $BUNDLE_REL; asking $SUB --check for each"

# --- the table -----------------------------------------------------------------
# Every row is `kind  from  to  result`; the result column carries the
# sub-verb's verdict in its words. A version the phrase did not yield
# reads `?` rather than a number this verb made up.
ROWS="$TMP/rows.tsv"
: > "$ROWS"
row() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >> "$ROWS"; }
version_after() { # <prefix> <one line>: the digits right after <prefix>
    local v
    v="$(sed -n "s/.*$1\([0-9]\{1,\}\).*/\1/p" <<<"$2")"
    printf '%s' "${v:-?}"
}
first_line_with() { # <phrase> <text>
    grep -m1 -F -- "$1" <<<"$2"
}

AHEAD=()          # kinds to publish, in bundle order
n_equal=0; n_refused=0; n_pending=0; n_other=0
for kind in "${KINDS[@]}"; do
    if ! grep -Eqx -- '^[a-z][a-z0-9-]{1,60}$' <<<"$kind"; then
        row "$kind" "-" "-" "skipped: file name outside the kind pattern ^[a-z][a-z0-9-]{1,60}$"
        n_other=$((n_other + 1))
        continue
    fi
    out="$("$SUB" "$kind" --check 2>&1)"; rc=$?
    case "$rc" in
        0)
            live="$(version_after 'over live v' "$(first_line_with '--check ok: would publish' "$out")")"
            row "$kind" "v$live" "tree" "would publish (the tree moved ahead; live v$live is a row the tree once said)"
            AHEAD+=("$kind")
            ;;
        5)
            live="$(version_after "the live $kind v" "$(first_line_with 'already says what' "$out")")"
            row "$kind" "v$live" "v$live" "equal — nothing to publish"
            n_equal=$((n_equal + 1))
            ;;
        6)
            live="$(version_after "live $kind v" "$(first_line_with 'carries what the tree never said' "$out")")"
            row "$kind" "v$live" "-" "REFUSED: live v$live carries what the tree never said — never published by this verb; the fields, from the sub-verb:"
            # The drift, field by field, copied not retyped: the
            # sub-verb prints one two-space-indented line per field.
            grep -E '^  ' <<<"$out" | sed 's/^/\t\t\t    /' >> "$ROWS"
            printf '\t\t\t    (write the live edit into %s/%s.toml and land that car, or approve --force-tree for this one kind at the Drift tab)\n' "$BUNDLE_REL" "$kind" >> "$ROWS"
            n_refused=$((n_refused + 1))
            ;;
        4)
            row "$kind" "-" "-" "REFUSED: the tree's row does not lint clean (or the registry holds a stale draft) — not published; the sub-verb's output:"
            grep -E '^  ' <<<"$out" | sed 's/^/\t\t\t    /' >> "$ROWS"
            n_refused=$((n_refused + 1))
            ;;
        8)
            row "$kind" "-" "-" "pending: no live row yet (the seed admits a new kind; this verb republishes existing ones)"
            n_pending=$((n_pending + 1))
            ;;
        75|78)
            say "$kind: the sub-verb could not answer (exit $rc); its output, whole:"
            sed 's/^/  /' <<<"$out"
            say "cannot answer: a drift set judged from a registry that could not be read for '$kind' would be the confident wrong answer — nothing published"
            exit "$rc"
            ;;
        *)
            row "$kind" "-" "-" "ERROR: publish-workflow.sh --check exited $rc, outside its documented codes: $(tail -n1 <<<"$out")"
            n_other=$((n_other + 1))
            ;;
    esac
done

# --- --for-real: each tree-ahead kind, in order, through the sub-verb ---------------
n_published=0; n_unconfirmed=0
if [ "$MODE" = "--for-real" ] && [ "${#AHEAD[@]}" -gt 0 ]; then
    say "publishing ${#AHEAD[@]} tree-ahead kind(s) in bundle order, each through $SUB <kind> (its own lint, publish, read-back)"
    PUB="$TMP/pub.tsv"
    : > "$PUB"
    for kind in "${AHEAD[@]}"; do
        out="$("$SUB" "$kind" 2>&1)"; rc=$?
        sed "s/^/  [$kind] /" <<<"$out"
        case "$rc" in
            0)
                # "<kind> vA -> vB live at <url> — confirmed ..."
                line="$(first_line_with ' live at ' "$out")"
                from="$(sed -n 's/.*[^0-9]v\([0-9]\{1,\}\) -> v[0-9]\{1,\} live at.*/\1/p' <<<"$line")"
                to="$(sed -n 's/.*[^0-9]v[0-9]\{1,\} -> v\([0-9]\{1,\}\) live at.*/\1/p' <<<"$line")"
                printf '%s\t%s\t%s\t%s\n' "$kind" "v${from:-?}" "v${to:-?}" "published — confirmed by the sub-verb's read-back" >> "$PUB"
                n_published=$((n_published + 1))
                ;;
            7)
                # The sub-verb ran the publish and the registry did not
                # bear it out (or could not be read back): its last line
                # says which; a draft may be armed for this ONE kind.
                printf '%s\t%s\t%s\t%s\n' "$kind" "-" "-" "NOT CONFIRMED: $(tail -n1 <<<"$out")" >> "$PUB"
                n_unconfirmed=$((n_unconfirmed + 1))
                ;;
            *)
                printf '%s\t%s\t%s\t%s\n' "$kind" "-" "-" "NOT CONFIRMED (publish-workflow.sh exited $rc, a verdict --check did not reach): $(tail -n1 <<<"$out")" >> "$PUB"
                n_unconfirmed=$((n_unconfirmed + 1))
                ;;
        esac
    done
    # The published rows replace their `would publish` rows, in place.
    while IFS=$'\t' read -r k f t r; do
        [ -n "$k" ] || continue
        awk -v k="$k" -v f="$f" -v t="$t" -v r="$r" 'BEGIN{FS=OFS="\t"} $1==k && $4 ~ /^would publish/ {print k, f, t, r; next} {print}' "$ROWS" > "$ROWS.new" \
            && mv "$ROWS.new" "$ROWS"
    done < "$PUB"
fi

# --- the answer ------------------------------------------------------------------
echo
printf 'kind\tfrom\tto\tresult\n'
cat "$ROWS"
echo
n_ahead="${#AHEAD[@]}"
verdict=""
if [ "$MODE" = "--check" ]; then
    verdict="$NAME: would publish $n_ahead, skipped $n_equal equal, refused $n_refused"
else
    verdict="$NAME: published $n_published, skipped $n_equal equal, refused $n_refused"
    [ "$n_unconfirmed" -eq 0 ] || verdict="$verdict, not confirmed $n_unconfirmed"
fi
[ "$n_pending" -eq 0 ] || verdict="$verdict, pending $n_pending"
[ "$n_other" -eq 0 ] || verdict="$verdict, errored $n_other"
verdict="$verdict (checkout ${HEAD:0:8}, ${#KINDS[@]} kind(s), packet ${OPS_REQUEST_ID:-none})"
echo "$verdict"
if [ "$n_unconfirmed" -gt 0 ] || [ "$n_other" -gt 0 ]; then
    exit 1
fi
exit 0
