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
# anywhere. Add it once: `git remote add github https://github.com/algedonic-dev/boss.git`.
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
#         2 = refused: a ref did not resolve; nothing was measured
set -uo pipefail

REMOTE="${GITHUB_REMOTE:-github}"
BRANCH="${GITHUB_BRANCH:-main}"
SOURCE="${SOURCE_REF:-origin/main}"
# Dated so each day's sync is its own reviewable PR rather than a
# moving branch whose diff changes under the reviewer.
PR_BRANCH="${PR_BRANCH:-publish/$(date -u +%Y-%m-%d)}"
JSON=0
[ "${1:-}" = "--json" ] && JSON=1

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

if ! git remote get-url "$REMOTE" >/dev/null 2>&1; then
  refuse "remote '$REMOTE' is not configured (the mirror ref $REMOTE/$BRANCH cannot resolve); add it: git remote add $REMOTE https://github.com/algedonic-dev/boss.git"
fi
git fetch -q "$REMOTE" "$BRANCH" 2>/dev/null
TARGET="$REMOTE/$BRANCH"
if ! git rev-parse --verify --quiet "$TARGET^{commit}" >/dev/null; then
  refuse "mirror ref $TARGET does not resolve (remote $REMOTE = $(git remote get-url "$REMOTE" 2>/dev/null))"
fi
if ! git rev-parse --verify --quiet "$SOURCE^{commit}" >/dev/null; then
  refuse "source ref $SOURCE does not resolve; set SOURCE_REF to a ref this clone has (e.g. origin/main after git fetch origin)"
fi
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
AHEAD=$(git rev-list --count "$TARGET".."$SOURCE") || refuse "git rev-list $TARGET..$SOURCE failed"
BEHIND=$(git rev-list --count "$SOURCE".."$TARGET") || refuse "git rev-list $SOURCE..$TARGET failed"
FILES=$(git diff --name-only "$TARGET" "$SOURCE" | wc -l | tr -d ' ')

say "  commits ahead : $AHEAD"
say "  commits behind: $BEHIND"
say "  files changed : $FILES"

BLOCKING=""

# A mirror that is BEHIND has commits the source ref does not contain —
# a push would either fail or, forced, destroy them. Always a human call.
if [ "$BEHIND" -gt 0 ]; then
  BLOCKING="${BLOCKING}mirror-has-unmerged-commits "
  say "  BLOCKING: $TARGET has $BEHIND commit(s) absent from $SOURCE"
fi

if [ "$AHEAD" -eq 0 ] && [ -z "$BLOCKING" ]; then
  say "  nothing to publish — the mirror is current"
  [ "$JSON" -eq 1 ] && printf '{"has_drift":false,"commits_ahead":0,"commits_behind":%s,"files_changed":0,"secrets_scan":"skipped","newly_public":0,"newly_public_files":[],"blocking":""}\n' "$BEHIND"
  exit 0
fi

# ---------------------------------------------------------------------
# 2. SECRETS — the existing tree-wide gate, self-testing.
# ---------------------------------------------------------------------
if bash infra/lint/no-secrets.sh >/dev/null 2>&1; then
  SECRETS="clean"
else
  SECRETS="FAILED"
  BLOCKING="${BLOCKING}secrets-lint-failed "
fi
say "  secrets scan  : $SECRETS"

# ---------------------------------------------------------------------
# 3. NEWLY PUBLIC SURFACE — files that exist on the source ref and have
# never existed on the public mirror. These are the ones a human should
# actually look at: everything else is a diff to something already out
# there. Runbooks and infra topology are called out by name because
# they describe how to reach and recover the live system, which is a
# different kind of disclosure from source code.
# ---------------------------------------------------------------------
NEW_FILES=$(git diff --name-only --diff-filter=A "$TARGET" "$SOURCE" 2>/dev/null)
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
  printf '{"has_drift":true,"commits_ahead":%s,"commits_behind":%s,"files_changed":%s,"secrets_scan":"%s","newly_public":%s,"newly_public_files":[%s],"blocking":"%s"}\n' \
    "$AHEAD" "$BEHIND" "$FILES" "$SECRETS" "$SENS_COUNT" "${LIST:-}" "$(echo "$BLOCKING" | xargs)"
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
