#!/usr/bin/env bash
# prep-github-publish.sh — measure what a push to the public GitHub
# mirror would actually publish, and prove it is safe to publish.
#
# WHY THIS EXISTS. Shipping moved to the internal forge on 2026-08-12
# and the GitHub mirror silently stopped being fed. Nothing broke, so
# nothing announced it; the drift was found by asking. A protocol that
# measures the gap on a cadence turns "is the public mirror current?"
# into a query instead of a thing somebody remembers to wonder about.
#
# This script MEASURES AND REPORTS. It never pushes and never opens a
# PR. Publication is a human decision because the target is public and
# cannot be taken back — a force-push removes the commit but not what
# was already cloned, indexed, or mirrored.
#
# THE MIRROR IS FED BY PULL REQUEST, NOT BY PUSHING main (David,
# 2026-08-14: "We should be opening a PR"). That gives two independent
# gates: the protocol's sign-off before the branch is pushed, and the
# merge on GitHub afterwards. Note that on a public repo the PR itself
# is the publication event — a PR diff is world-readable the moment it
# opens — so the sign-off still sits BEFORE the push, not before the
# merge.
#
# THE MIRROR IS READ THROUGH A REMOTE NAMED `github`, read-only and
# anonymous — the repo is public, so measuring needs no credential
# anywhere. The refusal below prints the `git remote add` that configures
# it, with the URL read from the one place the mirror is spelled
# (infra/estate/estate.toml, through infra/lib/sor.sh) — until
# 2026-09-20 that URL was a literal here, and a move would have left this
# script telling an operator to add a remote that no longer exists
# (backlog f8af6040).
#
# THE MIRROR IS GUARDED BY CONTENT, NOT BY COMMIT IDENTITY. A mirror
# holding work nobody published must block: a snapshot pushed over it
# destroys that work. Until 2026-09-10 the test for it was commit
# identity — any commit in `SOURCE..TARGET` blocked — and that test was
# a false positive BY CONSTRUCTION, because the protocol manufactures
# exactly such commits: the snapshot is a `commit-tree` of the source
# ref's tree onto the mirror's main, and merging the PR adds GitHub's
# merge commit above it. Neither can ever appear on the source ref, so
# from the first successful publish onward the condition was permanently
# true and the protocol could never run again. Measured on 2026-09-10:
# the one "absent" commit was b178a534 "Publish: forge main through
# 2026-08-27 (#129) (#236)", whose tree 02a76d57 IS the tree of forge
# main's b78ae6d2 — identical content, nothing to lose — and it was
# refusing a publish of 238 commits / 763 files. A check its own success
# makes permanently true protects nothing.
#
# So the question asked is now the one worth asking — does the mirror
# hold file content the source history does not? — and it is asked of
# TREES, in infra/mirror-drift-lib.sh, which carries the rule and its
# boundary. `commits_behind` still counts every mirror-only commit;
# `commits_behind_foreign` counts the ones that changed a file, and only
# those block. A blocking finding names the commit and the files.
#
# A REF THAT DOES NOT RESOLVE IS A REFUSAL, NOT A MEASUREMENT. Until
# 2026-09-08 a SOURCE_REF or mirror ref that did not exist made every
# `git rev-list` fail silently under `|| echo 0`, and the script exited
# 0 with `has_drift:false` — "the mirror is current" — while the mirror
# was 211 commits behind (design 7b59af2c). On the protocol's measure
# step that is the number that closes the packet nothing-to-publish. A
# wrong target must error, not answer (CLAUDE.md §Doors), so both refs
# are verified first and a miss exits 2 naming the ref.
#
# Usage:  infra/prep-github-publish.sh [--json]
# Exit:   0 = safe to publish (or nothing to publish)
#         1 = a blocking finding; do not publish
#         2 = refused: a ref did not resolve, SOURCE_REF moved during
#             the run, or the scanned tree was not SOURCE_REF's tree —
#             no verdict was given
set -uo pipefail

REMOTE="${GITHUB_REMOTE:-github}"
BRANCH="${GITHUB_BRANCH:-main}"
SOURCE="${SOURCE_REF:-origin/main}"
# Dated so each day's sync is its own reviewable PR rather than a
# moving branch whose diff changes under the reviewer.
PR_BRANCH="${PR_BRANCH:-publish/$(date -u +%Y-%m-%d)}"
JSON=0
[ "${1:-}" = "--json" ] && JSON=1

# Resolved BESIDE THIS SCRIPT, and before the cd: the classifier is part
# of this script, while the cd below is about the repo being measured
# (whose SOURCE_REF tree the secrets lint reads). Those are two different
# directories whenever the script is invoked against a scratch repo.
LIB="$(cd "$(dirname "$0")" && pwd)/mirror-drift-lib.sh"

cd "$(git rev-parse --show-toplevel)" || exit 1

say() { [ "$JSON" -eq 0 ] && echo "$@"; }
say "prep-github-publish: $SOURCE -> $REMOTE/$BRANCH"

# Refuse loudly. The JSON form carries the refusal too, so a caller that
# parses stdout sees `refused` and never a drift verdict.
refuse() {
  echo "prep-github-publish: REFUSED — $1" >&2
  echo "  nothing was measured; fix the target and run again" >&2
  [ "$JSON" -eq 1 ] && printf '{"refused":"%s"}\n' "$1"
  exit 2
}

# The classifier is not optional. A tree without it cannot answer
# whether the mirror holds foreign work, and a measurement that skips
# the question is the `|| echo 0` failure in a new coat.
if [ ! -f "$LIB" ]; then
  refuse "$LIB is missing; without it nothing can tell the mirror's own publish bookkeeping from foreign work on the mirror"
fi
# shellcheck source=infra/mirror-drift-lib.sh
. "$LIB" || refuse "$LIB could not be sourced"

# The mirror's clone URL, for the one message that names it. Read
# LAZILY: this script measures against whatever remote it was pointed at
# — a scratch fixture, in the self-test — and only the refusal below has
# to know where the real mirror lives, so a tree with no rendered
# address file still measures.
mirror_clone_url() {
  if [ -z "${BOSS_MIRROR_URL:-}" ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$(cd "$(dirname "$0")" && pwd)/lib/sor.sh"
    sor_require BOSS_MIRROR_URL
  fi
  printf '%s.git' "$BOSS_MIRROR_URL"
}

if ! git remote get-url "$REMOTE" >/dev/null 2>&1; then
  add_url=$(mirror_clone_url) || exit 1
  refuse "remote '$REMOTE' is not configured (the mirror ref $REMOTE/$BRANCH cannot resolve); add it: git remote add $REMOTE $add_url"
fi
git fetch -q "$REMOTE" "$BRANCH" 2>/dev/null
TARGET="$REMOTE/$BRANCH"
if ! git rev-parse --verify --quiet "$TARGET^{commit}" >/dev/null; then
  refuse "mirror ref $TARGET does not resolve (remote $REMOTE = $(git remote get-url "$REMOTE" 2>/dev/null))"
fi
if ! SOURCE_SHA=$(git rev-parse --verify --quiet "$SOURCE^{commit}"); then
  refuse "source ref $SOURCE does not resolve; set SOURCE_REF to a ref this clone has (e.g. origin/main after git fetch origin)"
fi
# Resolved ONCE, and every measurement below reads this commit rather
# than the name: `origin/main` is shared by every worktree of the clone,
# so another session's fetch can move it between two reads, and a drift
# counted at one commit beside a scan of another vouches for neither.
# The name is re-read after the scan and a move is a refusal.
# owner/repo for `gh pr create --repo`, derived from the remote rather
# than hardcoded so a fork or a renamed repo does not print a command
# that quietly targets the wrong place.
SLUG=$(git remote get-url "$REMOTE" 2>/dev/null \
  | sed -E 's#^git@[^:]+:##; s#^https?://[^/]+/##; s#\.git$##')

# ---------------------------------------------------------------------
# 1. DRIFT — what is on the source ref that the public mirror lacks.
# ---------------------------------------------------------------------
# No `|| echo 0` fallbacks: both refs were verified above, so a failure
# here is a real git error and must surface, not read as "current".
AHEAD=$(git rev-list --count "$TARGET".."$SOURCE_SHA") || refuse "git rev-list $TARGET..$SOURCE failed"
BEHIND=$(git rev-list --count "$SOURCE_SHA".."$TARGET") || refuse "git rev-list $SOURCE..$TARGET failed"
FILES=$(git diff --name-only "$TARGET" "$SOURCE_SHA" | wc -l | tr -d ' ')

say "  commits ahead : $AHEAD"
say "  commits behind: $BEHIND"
say "  files changed : $FILES"

BLOCKING=""

# A mirror commit the source ref does not contain is only a threat if it
# carries content the source history does not — see the header and
# mirror-drift-lib.sh. The classifier refuses (2) rather than answering
# when it cannot tell, and that refusal is passed straight through.
FOREIGN=$(foreign_mirror_commits "$SOURCE_SHA" "$TARGET")
case $? in
  0) ;;
  *) refuse "cannot tell the mirror's own publish bookkeeping from foreign work on $TARGET (see the error above)" ;;
esac
FOREIGN_COUNT=$(printf '%s' "$FOREIGN" | grep -c . || true)

if [ "$FOREIGN_COUNT" -gt 0 ]; then
  BLOCKING="${BLOCKING}mirror-has-unmerged-commits "
  say "  BLOCKING: $TARGET has $FOREIGN_COUNT commit(s) carrying file content absent from $SOURCE"
  # Named, because a verdict someone must go re-derive is not a verdict.
  [ "$JSON" -eq 0 ] && printf '%s\n' "$FOREIGN" | while IFS=$'\t' read -r sha subject files; do
    [ -n "$sha" ] || continue
    echo "      $sha $subject"
    echo "        changed: $files"
  done
elif [ "$BEHIND" -gt 0 ]; then
  say "  of the $BEHIND behind, 0 carry foreign content (our own publish bookkeeping)"
fi

if [ "$AHEAD" -eq 0 ] && [ -z "$BLOCKING" ]; then
  say "  nothing to publish — the mirror is current"
  [ "$JSON" -eq 1 ] && printf '{"has_drift":false,"commits_ahead":0,"commits_behind":%s,"commits_behind_foreign":0,"files_changed":0,"secrets_scan":"skipped","source_sha":"%s","scanned_sha":null,"newly_public":0,"newly_public_files":[],"blocking":""}\n' "$BEHIND" "$SOURCE_SHA"
  exit 0
fi

# ---------------------------------------------------------------------
# 2. SECRETS — the existing tree-wide gate, self-testing, run over the
# tree being PUBLISHED.
# ---------------------------------------------------------------------
# Until 2026-09-25 this ran `infra/lint/no-secrets.sh` in the working
# tree while everything above measured SOURCE_REF (backlog 63c82d2f).
# Measured by run 701ccb98: the dev pod checkout was 2 commits (18
# files) behind origin/main, so its `clean` vouched for a tree nobody
# was publishing — and a publish to a public repo cannot be taken back.
#
# So the tree is taken from git, never from disk: `git archive` of the
# resolved commit into a scratch dir, indexed there so the lint's own
# `git ls-files` lists exactly that tree, and the lint run is the
# REF's copy with the ref's allow-list — the pair that ships together,
# as publish-github-pr.sh --measure already runs it. The extraction is
# then hashed and must equal the commit's tree: `git archive` honours an
# `export-ignore` committed in the tree it archives, so a file could be
# published without being scanned, and "the scan read that tree" is a
# fact to check, not to assume.
SCAN_DIR=$(mktemp -d) || refuse "could not create a scratch dir for the tree of $SOURCE"
trap 'rm -rf "$SCAN_DIR"' EXIT
SCAN_TREE="$SCAN_DIR/tree"
mkdir -p "$SCAN_TREE"
git archive --format=tar "$SOURCE_SHA" | tar -x -C "$SCAN_TREE" \
  || refuse "could not extract the tree of $SOURCE ($SOURCE_SHA) with git archive"
git -C "$SCAN_TREE" init -q 2>/dev/null \
  && git -C "$SCAN_TREE" add -A -f 2>/dev/null \
  || refuse "could not index the extracted tree of $SOURCE ($SOURCE_SHA)"
SCANNED_TREE=$(git -C "$SCAN_TREE" write-tree) \
  || refuse "could not hash the extracted tree of $SOURCE ($SOURCE_SHA)"
WANT_TREE=$(git rev-parse "$SOURCE_SHA^{tree}")
if [ "$SCANNED_TREE" != "$WANT_TREE" ]; then
  refuse "the tree extracted for the scan ($SCANNED_TREE) is not the tree of $SOURCE ($SOURCE_SHA, tree $WANT_TREE) — an export-ignore or export-subst attribute on that tree changes what git archive writes, so a scan of it would not vouch for what is published"
fi
if ( cd "$SCAN_TREE" && bash infra/lint/no-secrets.sh ) >/dev/null 2>&1; then
  SECRETS="clean"
else
  SECRETS="FAILED"
  BLOCKING="${BLOCKING}secrets-lint-failed "
fi
SCANNED_SHA="$SOURCE_SHA"
# The name is read again: a verdict about the commit SOURCE_REF named at
# the start says nothing about the one it names now.
NOW_SHA=$(git rev-parse --verify --quiet "$SOURCE^{commit}")
if [ "$NOW_SHA" != "$SCANNED_SHA" ]; then
  refuse "source ref $SOURCE moved during the measurement: scanned $SCANNED_SHA, it now names ${NOW_SHA:-nothing}"
fi
say "  secrets scan  : $SECRETS"
say "  scanned       : $SCANNED_SHA (the tree of $SOURCE, from git, not the working tree)"

# ---------------------------------------------------------------------
# 3. NEWLY PUBLIC SURFACE — files that exist on the source ref and have
# never existed on the public mirror. These are the ones a human should
# actually look at: everything else is a diff to something already out
# there. Runbooks and infra topology are called out by name because
# they describe how to reach and recover the live system, which is a
# different kind of disclosure from source code.
# ---------------------------------------------------------------------
NEW_FILES=$(git diff --name-only --diff-filter=A "$TARGET" "$SOURCE_SHA" 2>/dev/null)
NEW_COUNT=$(printf '%s' "$NEW_FILES" | grep -c . || true)
SENSITIVE=$(printf '%s\n' "$NEW_FILES" | grep -E '^(docs/runbooks/|infra/(cluster|caddy|forge)/)' || true)
SENS_COUNT=$(printf '%s' "$SENSITIVE" | grep -c . || true)

say "  newly public  : $NEW_COUNT file(s), $SENS_COUNT touching runbooks/infra"
if [ "$SENS_COUNT" -gt 0 ] && [ "$JSON" -eq 0 ]; then
  printf '%s\n' "$SENSITIVE" | sed 's/^/      review: /'
fi

# Not blocking — RFC1918 addresses and k8s manifests are not secrets,
# and the repo is open source by intent. It is surfaced so the decision
# is made deliberately rather than by omission.

if [ "$JSON" -eq 1 ]; then
  LIST=$(printf '%s\n' "$SENSITIVE" | grep . | sed 's/.*/"&"/' | paste -sd, - 2>/dev/null || true)
  printf '{"has_drift":true,"commits_ahead":%s,"commits_behind":%s,"commits_behind_foreign":%s,"files_changed":%s,"secrets_scan":"%s","source_sha":"%s","scanned_sha":"%s","newly_public":%s,"newly_public_files":[%s],"blocking":"%s"}\n' \
    "$AHEAD" "$BEHIND" "$FOREIGN_COUNT" "$FILES" "$SECRETS" "$SOURCE_SHA" "$SCANNED_SHA" "$SENS_COUNT" "${LIST:-}" "$(echo "$BLOCKING" | xargs)"
fi

if [ -n "$BLOCKING" ]; then
  say ""
  say "  DO NOT PUSH — blocking: $(echo "$BLOCKING" | xargs)"
  exit 1
fi

say ""
say "  safe to publish. On approval the forge opens the PR by machine"
say "  (ops verb publish-github-pr: a snapshot commit of $SOURCE on"
say "  $(printf '%s' "$SLUG") main, pushed to dauld:$PR_BRANCH, then gh pr create)."
say "  The merge on GitHub is yours."
exit 0
