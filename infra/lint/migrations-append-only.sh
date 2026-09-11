#!/usr/bin/env bash
#
# migrations-append-only — an applied migration is history, including
# its prose.
#
# THE INCIDENT
# ------------
# 2026-08-13. A doc-reference flatten edited three lines of COMMENT in
# infra/postgres/schema/111-gateway-audit-events.sql. No SQL changed.
# It merged on train #21, deployed, and `boss-init` refused to start:
# migrate.sh hashes the whole file, so the recorded checksum
# (f7cf9c874953…) no longer matched what was on disk (18902fd80ceb…).
# The Deployment's strategy is `Recreate`, so the healthy pod had
# already been terminated. BOSS lost its own system of record — no
# packets, no stations, no audit trail — for an hour, over a comment.
#
# migrate.sh caught it correctly. It caught it at DEPLOY time, in
# production, after the merge. This check exists to catch the same
# thing at gate time, before it can be merged at all.
#
# WHY CI COULD NOT ALREADY SEE IT
# -------------------------------
# The suite applies migrations to a scratch database created per test.
# A scratch database has no history, so "this file changed after it
# was applied" is unreachable by construction: CI tests migration from
# empty, production applies them incrementally. This check needs
# neither — it asks git, not a database.
#
# THE CHECKED PROPERTY
# --------------------
# Against the merge-base with the trunk, no file under
# infra/postgres/schema/ may be MODIFIED or DELETED. New files may be
# added freely — that is how a schema change is supposed to arrive.
#
# manifest.txt was exempt while it existed, because adding a migration
# necessarily appended to it. It was REMOVED on 2026-08-14: the ordered
# list is now the directory itself, sorted by the NNN- prefix, so adding
# a migration touches no shared file and two cars carrying migrations no
# longer conflict. Its allow-list entry stays below as the record.
#
# THE ESCAPE HATCH
# ----------------
# A migration that is genuinely wrong and has NEVER been applied
# anywhere can be edited in place — but say so out loud, in a diff a
# reviewer sees, by adding the filename to ALLOW below with the reason
# and the date. Deleting the entry afterwards is not required; the
# entry IS the record that someone checked.
#
# Usage: infra/lint/migrations-append-only.sh [--self-test]
# Exit:  0 clean / 1 violations, a self-test failure, or no trunk ref in
#        this checkout / 3 git could not answer, so NOTHING was read —
#        an infrastructure refusal, see lib/git-answer.sh

set -uo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$LINT_DIR/lib/trunk-ref.sh"

LINT=migrations-append-only
SCHEMA_DIR="infra/postgres/schema"

# One entry per line: "<filename> <YYYY-MM-DD> <reason>"
read -r -d '' ALLOW <<'ALLOWLIST' || true
manifest.txt 2026-08-14 removed; the schema directory is now the ordered list (migrate.sh migration_order), so nothing is appended to add a migration
ALLOWLIST

is_allowed() {
    printf '%s\n' "$ALLOW" | grep -q "^$1 "
}

# The trunk to compare against is resolved by lib/trunk-ref.sh — the
# same walk all four baseline-comparing lints used to carry a copy of,
# with the reason for the order and the absent-vs-unreadable split
# written there. A missing trunk is a hard error HERE, not a silent
# pass: a check that quietly skips is the under-covering gate this repo
# has already shipped twice.

# $1 = base ref, $2 = tree-ish to compare (default HEAD)
#
# Both git reads below go through `git_answer`, which exits this script
# with $LINT_CANNOT_ANSWER rather than returning: this function's return
# value IS the violation count, so a status could not carry "I could not
# look" without colliding with "three violations". `git diff
# --name-status` used to run in a process substitution under
# `2>/dev/null`, which made an unreadable repo indistinguishable from a
# branch that touched no migration — zero lines read, zero violations,
# `clean`.
check_against() {
    local base="$1" head="${2:-HEAD}" violations=0 status file
    local mb diff_out
    mb=$(resolve_merge_base "$LINT" "$base" "$head") || exit $?
    diff_out=$(git_answer "$LINT" 0 diff --name-status "$mb" "$head" -- "$SCHEMA_DIR") || exit $?

    while read -r status file; do
        [ -z "${file:-}" ] && continue
        case "$status" in
            M*|D*|R*) ;;
            *) continue ;;
        esac
        local name
        name=$(basename "$file")
        if is_allowed "$name"; then
            continue
        fi
        echo "VIOLATION: $file was ${status:0:1}-changed against $base"
        echo "    An applied migration is history, including its comments. migrate.sh"
        echo "    hashes the whole file, so even a prose edit stops every deploy that"
        echo "    has already applied it (2026-08-13: this cost the system of record"
        echo "    for an hour). Put the change in a NEW migration file. If this one"
        echo "    has never been applied anywhere, add it to ALLOW in this script"
        echo "    with the reason."
        violations=$((violations+1))
    done <<EOF
$diff_out
EOF

    return "$violations"
}

self_test() {
    # The whole self-test runs in one subshell that first DROPS any
    # inherited git context. A pre-push hook exports GIT_DIR — and with
    # GIT_DIR set, the fixture's `git init` in a tmp dir re-targets the
    # INVOKING repo and the tmp dir becomes its work tree, so the
    # fixture commits landed on the real repo, reset its `main`, and
    # created its branches (5b65c2a8; reproduced 2026-08-30 with a
    # victim repo: base/edit/del/add all appeared in the victim's log).
    # A self-test that can write to the repo under test is strictly
    # worse than no self-test, so this boundary fails closed below.
    (
    unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY
    local tmp rc fails=0 out
    tmp=$(mktemp -d) || return 1

    (
        set -e
        cd "$tmp"
        # Tripwire BEFORE the first mutating command: with no repo here
        # yet, git must see NO repository at all. If it answers with
        # one, inherited context is aiming the fixture at somebody
        # else's repo — refuse before touching anything. This catches
        # vectors the unset above doesn't know about yet.
        if leaked=$(git rev-parse --absolute-git-dir 2>/dev/null); then
            echo "fixture would operate on $leaked — inherited git context" >&2
            exit 90
        fi
        git init -q .
        git config user.email t@t; git config user.name t
        mkdir -p "$SCHEMA_DIR"
        printf 'CREATE TABLE a();\n' > "$SCHEMA_DIR/100-a.sql"
        printf '100-a.sql\n' > "$SCHEMA_DIR/manifest.txt"
        git add -A; git commit -qm base
        # -B, not `git branch main`: git's default initial branch name
        # varies by version, so `main` may or may not already exist.
        git checkout -q -B main
    ) || { echo "SELF-TEST FAIL: fixture repo setup"; rm -rf "$tmp"; return 1; }

    # Fixture 1: modifying an existing migration must be caught.
    ( cd "$tmp" && git checkout -q -b edit main \
        && printf -- '-- a comment\nCREATE TABLE a();\n' > "$SCHEMA_DIR/100-a.sql" \
        && git commit -qam edit ) >/dev/null 2>&1
    out=$( cd "$tmp" && SCHEMA_DIR="$SCHEMA_DIR" LINT="$LINT" bash -c "$(declare -f is_allowed check_against resolve_merge_base git_answer _git_answer_refuse); ALLOW=''; check_against main" 2>&1 ) && rc=0 || rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "SELF-TEST FAIL: a modified migration was not caught"; fails=$((fails+1))
    elif ! printf '%s' "$out" | grep -q "VIOLATION"; then
        echo "SELF-TEST FAIL: modification caught but not reported as a VIOLATION"; fails=$((fails+1))
    fi

    # Fixture 2: adding a new migration must NOT be caught.
    ( cd "$tmp" && git checkout -q -b add main \
        && printf 'CREATE TABLE b();\n' > "$SCHEMA_DIR/101-b.sql" \
        && printf '100-a.sql\n101-b.sql\n' > "$SCHEMA_DIR/manifest.txt" \
        && git add -A && git commit -qm add ) >/dev/null 2>&1
    out=$( cd "$tmp" && SCHEMA_DIR="$SCHEMA_DIR" LINT="$LINT" bash -c "$(declare -f is_allowed check_against resolve_merge_base git_answer _git_answer_refuse); ALLOW='manifest.txt 2026-08-13 x'; check_against main" 2>&1 ) && rc=0 || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "SELF-TEST FAIL: adding a new migration (+ manifest append) was reported: $out"; fails=$((fails+1))
    fi

    # Fixture 3: deleting a migration must be caught.
    ( cd "$tmp" && git checkout -q -b del main \
        && git rm -q "$SCHEMA_DIR/100-a.sql" && git commit -qm del ) >/dev/null 2>&1
    out=$( cd "$tmp" && SCHEMA_DIR="$SCHEMA_DIR" LINT="$LINT" bash -c "$(declare -f is_allowed check_against resolve_merge_base git_answer _git_answer_refuse); ALLOW=''; check_against main" 2>&1 ) && rc=0 || rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "SELF-TEST FAIL: a deleted migration was not caught"; fails=$((fails+1))
    fi

    # Fixture 4: the hook environment itself. Re-run this script's
    # self-test with GIT_DIR aimed at a decoy repo — as a pre-push
    # hook aims it at the real one — and assert the decoy never moves.
    # This is the regression test for the incident: without the unset
    # and tripwire above, this exact shape committed fixture garbage
    # onto the invoking repo and reset its main. Guarded against
    # recursing into itself via MIGRATIONS_LINT_INNER.
    if [ -z "${MIGRATIONS_LINT_INNER:-}" ]; then
        ( set -e; mkdir "$tmp/decoy"; cd "$tmp/decoy"; git init -q .
          git config user.email t@t; git config user.name t
          printf 'x\n' > f; git add -A; git commit -qm seed ) >/dev/null 2>&1 \
            || { echo "SELF-TEST FAIL: decoy repo setup"; fails=$((fails+1)); }
        before=$(git -C "$tmp/decoy" rev-parse HEAD 2>/dev/null)
        MIGRATIONS_LINT_INNER=1 GIT_DIR="$tmp/decoy/.git" \
            bash "${BASH_SOURCE[0]}" --self-test >/dev/null 2>&1 || true
        after=$(git -C "$tmp/decoy" rev-parse HEAD 2>/dev/null)
        refs=$(git -C "$tmp/decoy" for-each-ref | wc -l | tr -d ' ')
        if [ "$before" != "$after" ] || [ "$refs" != "1" ]; then
            echo "SELF-TEST FAIL: an inherited GIT_DIR reached the fixture (decoy $before -> $after, refs=$refs)"
            fails=$((fails+1))
        fi
    fi

    rm -rf "$tmp"
    if [ "$fails" -eq 0 ]; then
        echo "self-test: fixtures behaved as specified"
        return 0
    fi
    return 1
    )
}

if [ "${1:-}" = "--self-test" ]; then
    self_test
    exit $?
fi

# CAPTURED, not discarded. `self_test >/dev/null` threw away the only
# copy of why it failed: with git refusing every command (measured under
# GIT_TEST_ASSUME_DIFFERENT_OWNER=1) the fixtures cannot be built at all,
# and the operator saw five words that named neither the fixture nor git.
# Quiet on success, the whole record on failure (CLAUDE.md §Diagnosis).
# Ask git the one question whose answer is already known FIRST. The
# self-test builds git fixtures in a temp directory, so on a machine where
# git refuses everything it fails there — and "SELF-TEST FAILED" blames
# this script's detectors for a broken machine. Measured under
# GIT_TEST_ASSUME_DIFFERENT_OWNER=1, which is what a gate workspace looked
# like on 2026-09-11.
git_can_answer "$LINT" || exit $?

if ! self_test_out=$(self_test 2>&1); then
    # A refusal INSIDE the self-test is still a refusal: the fixtures are
    # driven through the same `git_answer`, so one git call failing
    # mid-fixture reaches here as a self-test failure. Told apart by the
    # marker the refusal carries, not by guessing.
    if printf '%s' "$self_test_out" | grep -qF "$LINT_CANNOT_ANSWER_MARKER"; then
        echo "$LINT: CANNOT ANSWER — the self-test's git fixtures could not be built," >&2
        echo "  so the detectors were never proven and the tree was never read." >&2
        printf '%s\n' "$self_test_out" | sed 's/^/  /' >&2
        exit "$LINT_CANNOT_ANSWER"
    fi
    echo "$LINT: SELF-TEST FAILED — refusing to report on the tree" >&2
    printf '%s\n' "$self_test_out" | sed 's/^/  /' >&2
    exit 1
fi

# Two different answers, and telling them apart is the whole point. git
# saying "that ref is not here" is a fact about the CHECKOUT and "fetch
# the trunk" is the right advice. git saying nothing at all — exit 128 on
# a dubious-ownership refusal, which is what a gate workspace produced on
# 2026-09-11 — is a fact about the MACHINE, and printing "fetch the
# trunk" there sent an operator after a ref that was already present
# (backlog 6b2f4a1a). resolve_trunk_ref returns 1 for the first and 3 for
# the second, having already printed git's own words.
BASE=$(resolve_trunk_ref "$LINT"); rc=$?
if [ "$rc" -eq "$LINT_CANNOT_ANSWER" ]; then
    exit "$LINT_CANNOT_ANSWER"
elif [ "$rc" -ne 0 ]; then
    echo "$LINT: no trunk ref found (tried $(trunk_candidates))" >&2
    echo "  Cannot tell which migrations are history without one. Fetch the trunk." >&2
    exit 1
fi

if check_against "$BASE"; then
    echo "migrations-append-only: clean — no existing migration modified against $BASE"
    exit 0
fi
echo "migrations-append-only: applied migrations were edited; see above"
exit 1
