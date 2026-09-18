#!/usr/bin/env bash
# tag-release.sh <version> <sha> <release-packet-id> — put an annotated
# tag on the forge at a LANDED train's merge commit, read it back, and
# answer on one line. MUTATING: it writes exactly one ref.
#
# WHY (backlog 05d301be, left by the cut-a-release tenant car on
# 2026-09-18). No ops verb writes a git ref on the forge — 31 verbs,
# none tags — so the tenant's cut-a-release protocol left its `tag`
# step to the founder by hand: fetch, `git tag -a`, `git push`, read
# the sha back, type it onto the step. David's rule (2026-09-16):
# "avoid doing anything by hand unless absolutely required" — a tag is
# a mechanical act on a fact the record already holds, so it is the
# machine's. This is that door, in the shape the allowlist demands
# (infra/ops/verbs/tag-release.json): three pattern-checked params, one
# fixed script, every bound a refusal that changes nothing.
#
# WHAT IT REFUSES, each named on stdout with exit 1 and NOTHING written
# (the refusals run before the tag exists anywhere):
#   * a version not shaped vMAJOR.MINOR.PATCH, a sha shorter than 40
#     hex, a packet id that is not a full uuid — the allowlist refuses
#     these first (patterns in the verb file), and the script does not
#     rely on it;
#   * a tag of that name already on the forge (`git ls-remote --tags`):
#     a release is cut once, and a second push would be a silent no-op
#     or a moved tag, both worse than a refusal;
#   * a sha that no CLOSED pr-train's `merged` step records as its
#     `merge_ref`, read off the system of record: the version names a
#     TRAIN THAT LANDED, never an arbitrary commit — `merge_ref` is the
#     conductor's 12-char prefix of the forge's merge oid
#     (train/conductor.rs), so the test is prefix-equality at >= 7
#     chars, the same `commits_match` the conductor applies. An open
#     train's is not proof; an EMPTY listing is a failed read, not a
#     fact (an unidentified reader is handed a smaller world, backlog
#     61085a9e), and refuses;
#   * a sha that is not an ancestor of the converged main — HEAD of the
#     converged checkout, which forge-converge.sh detaches at forge main
#     every ten minutes (`git merge-base --is-ancestor`).
#
# ON SUCCESS: `git tag -a <version> <sha>` in the converged checkout, as
# its OWNER (root's git in david's clone leaves root-owned objects the
# owner cannot collect — forge-converge.sh's rule), with a message that
# carries the version, the release packet id, the train and the
# ops-request; `git push <remote> refs/tags/<version>` as the same
# owner, whose login environment carries the forge credential helper
# (the converge's own arrangement — no token is read, held or printed
# here); then `git ls-remote --tags` READ BACK, and the peeled commit
# the forge now holds under that tag must be the sha given — a forge
# answer is not a forge effect. The LAST line is the answer:
#
#   tag-release: <version> at <sha> (packet <release-packet-id>)
#
# which the dispatcher rule complete-release-tag-on-tag-release-answered
# reads off the answered ops-request (jobs.complete_linked_step with a
# verdict_pattern) to complete the release packet's `tag` step with the
# tag and the sha COPIED from this read-back, never retyped. The rule
# follows `metadata.release` on the ops-request, which is the same id
# as the third argument: the filer writes both, the tag message keeps
# one, the rule reads the other.
#
# The GitHub half — the release on the public mirror, a tag on the
# snapshot whose tree equals the tagged tree — is a second verb, after
# David merges the mirror PR (backlog 05d301be names it). Not here.
#
# USAGE
#   tag-release.sh v1.2.3 <40-hex sha> <release packet uuid>
#
# EXIT
#   0  the tag is on the forge at the sha, read back; the answer line is last
#   1  refused (nothing written) or failed (the reason says what was left)
#
# ENV (test seams — the ops-runner passes no packet-supplied
# environment, only an argv built from the allowlist, so a packet
# cannot set these)
#   BOSS_TAG_RELEASE_TREE       the converged checkout (default: the one
#                               this script is in); HEAD is the
#                               converged main
#   BOSS_TAG_RELEASE_REMOTE     the forge, as a remote NAME in the tree
#                               (default forgejo, the converge's) or a
#                               URL / path
#   BOSS_TAG_RELEASE_SOR_READ   the trains reader (default: this
#                               checkout's infra/forge/probe-bin/
#                               boss-sor-read, which needs BOSS_JOBS_URL
#                               — the ops-runner's unit pins it)
#   BOSS_TAG_RELEASE_TAGGER_NAME / _EMAIL   the tag's tagger identity
#   OPS_REQUEST_ID              set by the ops-runner; recorded in the
#                               tag message when present

set -uo pipefail

ME="tag-release"
# STDOUT for every line, refusals included: the ops-runner records the
# two streams merged, the rule reads the LAST matching line, and a
# reader of the packet should see the refusal where the answer would be.
say() { echo "$ME: $*"; }
refuse() { say "REFUSED — $*"; say "  Nothing was written."; exit 1; }
fail() { say "FAILED — $*"; exit 1; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
TREE="${BOSS_TAG_RELEASE_TREE:-$REPO}"
REMOTE="${BOSS_TAG_RELEASE_REMOTE:-forgejo}"
SOR_READ="${BOSS_TAG_RELEASE_SOR_READ:-$REPO/infra/forge/probe-bin/boss-sor-read}"
TAGGER_NAME="${BOSS_TAG_RELEASE_TAGGER_NAME:-BOSS tag-release}"
TAGGER_EMAIL="${BOSS_TAG_RELEASE_TAGGER_EMAIL:-tag-release@noreply.algedonic.dev}"
TRAINS_LIMIT=200
TRAINS_PATH="/api/jobs?kind=pr-train&status=closed&limit=$TRAINS_LIMIT"
# The trains are read as a NAMED, READ-SCOPED actor through the reader
# a recorded probe uses (run-car-probe.sh builds this identity for the
# same reason: role `audit-readonly` is Read at Scope::All and nothing
# else, and an unidentified reader is answered a narrower world in
# silence — backlog 61085a9e).
READER_USER='{"id":"automation:tag-release-reader","role":"audit-readonly","access_tier":"auditor","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# --- bound 1: the arguments -------------------------------------------------
[ $# -eq 3 ] || refuse "usage: $ME <version vMAJOR.MINOR.PATCH> <40-hex sha> <release packet uuid> — got $# argument(s)"
VERSION="$1"; SHA="$2"; RELEASE="$3"
[[ "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || refuse "version '$VERSION' is malformed — a release is named vMAJOR.MINOR.PATCH (v1.2.3), nothing else"
[[ "$SHA" =~ ^[0-9a-f]{40}$ ]] \
    || refuse "sha '$SHA' is not a full 40-hex commit id — a tag is placed on a commit the record names in full, never a prefix"
[[ "$RELEASE" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
    || refuse "release packet id '$RELEASE' is not a full uuid — the tag message names the cut-a-release packet in full"

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# The remote's URL may carry a token (the converge's credential helper
# never puts one there, but a hand-configured remote might): every line
# a tool says back is redacted before it is printed, and the URL itself
# is never printed — the remote is named by its name.
redact() { sed -E 's#://[^/@[:space:]]+@#://<redacted>@#g'; }
# The ONE line of a git failure worth recording: the rejection or the
# error, not `To <remote>` (sweep-archive-branches.sh's said_why).
said_why() { # stdin -> the reason line (one awk: no early-exit grep under pipefail)
    redact | awk 'NF { if (!first) first = $0
                       if (!why && ($0 ~ /^[[:space:]]*!/ || $0 ~ /rejected/ || $0 ~ /^(error|fatal|remote):/)) why = $0 }
                  END { if (why) print why; else if (first) print first }'
}

# --- the checkout, and whose git runs in it --------------------------------
case "$TREE" in
    *[[:space:]\'\"]*) refuse "the tree path \`$TREE\` contains whitespace or a quote; name one without" ;;
esac
case "$REMOTE" in
    *[[:space:]\'\"]*) refuse "the remote \`$REMOTE\` contains whitespace or a quote; name one without" ;;
esac
[ -d "$TREE" ] || refuse "the tree $TREE does not exist, so the converged main cannot be read"
OWNER="$(stat -c %U "$TREE" 2>/dev/null)"
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ] || ! id -u "$OWNER" >/dev/null 2>&1; then
    refuse "cannot resolve the owner of $TREE (stat says '${OWNER:-}'), so there is no account to run its git as"
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
# Every git call runs as the checkout's OWNER: the ops-runner executes
# verbs as root, and a root write into david's clone leaves root-owned
# objects the owner's later fetches cannot collect (forge-converge.sh).
# The owner's HOME is what carries the forge credential helper.
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}
# GIT_TERMINAL_PROMPT=0: a unit has no terminal, and a prompt would
# hang, not fail.
G="GIT_TERMINAL_PROMPT=0 git -C '$TREE'"

# --- bound 2: the tag is not already on the forge --------------------------
# ALL the forge's tags, filtered here by exact name: a pattern argument
# to ls-remote matches `refs/tags/<v>` but never its peeled twin
# `refs/tags/<v>^{}`, and the peeled line is the one that names the
# COMMIT (the other names the tag object).
read_forge_tag() { # <file> -> stdout: "<tag object sha> <peeled commit sha>" or nothing
    awk -F'\t' -v t="refs/tags/$VERSION" '
        $2 == t { obj = $1 } $2 == t "^{}" { peel = $1 }
        END { if (obj != "") print obj, peel }' "$1"
}
if ! as_owner "$G ls-remote --tags '$REMOTE'" > "$TMP/exists.tsv" 2> "$TMP/exists.err"; then
    refuse "cannot read the forge's tags through remote $REMOTE of $TREE (as $OWNER); git said: $(said_why < "$TMP/exists.err")"
fi
EXISTS="$(read_forge_tag "$TMP/exists.tsv")"
if [ -n "$EXISTS" ]; then
    at="${EXISTS#* }"; [ -n "$at" ] || at="${EXISTS%% *}"
    refuse "tag $VERSION already exists on the forge at ${at:0:12} — a release is cut once; a different commit wants a different version"
fi
say "forge: no tag $VERSION on remote $REMOTE"

# --- bound 3: the sha is a CLOSED train's merge commit ---------------------
[ -x "$SOR_READ" ] || refuse "the trains reader $SOR_READ is not here, so a landed train cannot be told from an arbitrary commit (the record is read through boss-sor-read, which the converged checkout carries at infra/forge/probe-bin)"
if ! BOSS_SOR_USER="$READER_USER" "$SOR_READ" "$TRAINS_PATH" > "$TMP/trains.out" 2> "$TMP/trains.err"; then
    refuse "cannot read the closed pr-train packets ($TRAINS_PATH); the reader said: $(redact < "$TMP/trains.err" | tr '\n' ' ')"
fi
if ! TRAINS=$(jq -r 'if type != "object" or (.data | type) != "array" then error("not a listing: no data array") else (.data | length) end' \
        < "$TMP/trains.out" 2> "$TMP/jq.err"); then
    refuse "the closed pr-train listing is not a listing this can read: $(tr '\n' ' ' < "$TMP/jq.err")"
fi
if [ "$TRAINS" -eq 0 ]; then
    refuse "the system of record listed no closed pr-train packets, which is a failed read and not a fact — an unauthenticated or out-of-scope read is handed an empty page rather than an error"
fi
# The merged step's merge_ref of every CLOSED train, with the train's
# id; a prefix of the sha at >= 7 chars is the conductor's own equality.
TRAIN="$(jq -r --arg sha "$SHA" '
    .data[]
    | select(.status == "closed")
    | . as $t
    | ((.steps // [])[] | select(.spec_slug == "merged") | (.metadata // {}).merge_ref // empty) as $ref
    | select(($ref | length) >= 7 and ($sha | startswith($ref)))
    | "\($t.id) \($ref)"' < "$TMP/trains.out" 2>/dev/null | awk 'NR == 1')"
if [ -z "$TRAIN" ]; then
    refuse "sha ${SHA:0:12} is not the merge commit of any of the $TRAINS closed pr-train packets read (newest first, limit $TRAINS_LIMIT) — a release names a train that LANDED; read the sha off the newest closed train's merged step"
fi
TRAIN_ID="${TRAIN%% *}"; TRAIN_REF="${TRAIN#* }"
say "record: pr-train ${TRAIN_ID:0:8} merged as $TRAIN_REF ($TRAINS closed trains read)"

# --- bound 4: the sha is an ancestor of the converged main -----------------
if ! HEAD_SHA=$(as_owner "$G rev-parse HEAD" 2> "$TMP/head.err") || [[ ! "$HEAD_SHA" =~ ^[0-9a-f]{40}$ ]]; then
    refuse "cannot read HEAD of $TREE (as $OWNER); git said: $(said_why < "$TMP/head.err")"
fi
as_owner "$G merge-base --is-ancestor '$SHA' HEAD" 2> "$TMP/anc.err"; rc=$?
case "$rc" in
    0) ;;
    1) refuse "sha ${SHA:0:12} is not an ancestor of the converged main (${HEAD_SHA:0:12}, HEAD of $TREE) — main was not converged past this commit, or the commit is not on main" ;;
    *) refuse "cannot test whether ${SHA:0:12} is an ancestor of the converged main (${HEAD_SHA:0:12}); git said: $(said_why < "$TMP/anc.err")" ;;
esac
say "converged main: ${HEAD_SHA:0:12} carries ${SHA:0:12}"

# --- the tag: annotated, at the sha, then pushed ---------------------------
# -f: the forge has no such tag (bound 2), so a local one of that name
# is residue of an earlier attempt that never reached the forge, and
# the forge is the truth it is re-pointed to.
MSG_TITLE="BOSS $VERSION"
# The ops-request's id rides the message only in the shape the runner
# sets it (a uuid): the message is handed to git inside a quoted
# command string, and an id of any other shape is left out, not quoted.
REQUEST=""
[[ "${OPS_REQUEST_ID:-}" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] && REQUEST="$OPS_REQUEST_ID"
MSG_BODY="Release packet $RELEASE (cut-a-release). Tagged at the merge commit of pr-train $TRAIN_ID by the tag-release ops verb${REQUEST:+ (ops-request $REQUEST)}."
if ! as_owner "$G -c user.name='$TAGGER_NAME' -c user.email='$TAGGER_EMAIL' tag -a -f '$VERSION' '$SHA' -m '$MSG_TITLE' -m '$MSG_BODY'" > "$TMP/tag.out" 2>&1; then
    fail "git tag -a $VERSION $SHA in $TREE (as $OWNER): $(said_why < "$TMP/tag.out"). Nothing reached the forge"
fi
if ! as_owner "$G push '$REMOTE' 'refs/tags/$VERSION:refs/tags/$VERSION'" > "$TMP/push.out" 2> "$TMP/push.err"; then
    reason="$(said_why < "$TMP/push.err")"
    as_owner "$G tag -d '$VERSION'" > /dev/null 2>&1
    fail "pushing refs/tags/$VERSION to remote $REMOTE (as $OWNER): ${reason:-no message}. The local tag was removed; the forge holds nothing"
fi

# --- read back: a forge answer is not a forge effect -----------------------
if ! as_owner "$G ls-remote --tags '$REMOTE'" > "$TMP/back.tsv" 2> "$TMP/back.err"; then
    fail "the push answered but reading refs/tags/$VERSION back from remote $REMOTE failed: $(said_why < "$TMP/back.err")"
fi
BACK="$(read_forge_tag "$TMP/back.tsv")"
# "<object> <peeled>": an annotated tag has both; a lightweight one (or
# none) has no peeled commit and is not what was pushed.
case "$BACK" in
    *" "?*) BACK="${BACK#* }" ;;
    *) fail "the push answered but the forge lists no ANNOTATED tag $VERSION (ls-remote: $(grep -F "refs/tags/$VERSION" "$TMP/back.tsv" | tr '\n' ' '))" ;;
esac
if [ "$BACK" != "$SHA" ]; then
    fail "the push answered but the forge holds $VERSION at ${BACK:0:12}, not ${SHA:0:12}"
fi
say "read back: refs/tags/$VERSION on remote $REMOTE peels to $BACK"
echo "$ME: $VERSION at $BACK (packet $RELEASE)"
