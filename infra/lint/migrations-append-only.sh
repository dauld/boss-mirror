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
# Exit:  0 clean / 1 violations or self-test failure

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

SCHEMA_DIR="infra/postgres/schema"

# One entry per line: "<filename> <YYYY-MM-DD> <reason>"
read -r -d '' ALLOW <<'ALLOWLIST' || true
manifest.txt 2026-08-14 removed; the schema directory is now the ordered list (migrate.sh migration_order), so nothing is appended to add a migration
ALLOWLIST

is_allowed() {
    printf '%s\n' "$ALLOW" | grep -q "^$1 "
}

# Resolve the trunk to compare against. In CI the PR branch is checked
# out with main available as a remote ref; on a dev box `main` is
# usually local. A missing trunk is a hard error, not a silent pass —
# a check that quietly skips is the under-covering gate this repo has
# already shipped twice.
# Order matters. The forge is the trunk every car actually merges
# into; `origin` on a dev box is the GitHub mirror, which is a
# periodic backup and was 24 commits stale the day this was written —
# comparing against it reports every already-merged migration as a
# fresh modification. In CI the checkout's `origin` IS the forge, so
# the first hit there is correct. BOSS_TRUNK_REF overrides for the
# case where neither name applies.
resolve_base() {
    local ref
    for ref in ${BOSS_TRUNK_REF:-} forge/main origin/main main; do
        if git rev-parse --verify --quiet "$ref" >/dev/null 2>&1; then
            echo "$ref"; return 0
        fi
    done
    return 1
}

# $1 = base ref, $2 = tree-ish to compare (default HEAD)
check_against() {
    local base="$1" head="${2:-HEAD}" violations=0 status file
    local mb
    mb=$(git merge-base "$base" "$head" 2>/dev/null) || mb="$base"

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
    done < <(git diff --name-status "$mb" "$head" -- "$SCHEMA_DIR" 2>/dev/null)

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
    out=$( cd "$tmp" && SCHEMA_DIR="$SCHEMA_DIR" bash -c "$(declare -f is_allowed check_against); ALLOW=''; check_against main" 2>&1 ) && rc=0 || rc=$?
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
    out=$( cd "$tmp" && SCHEMA_DIR="$SCHEMA_DIR" bash -c "$(declare -f is_allowed check_against); ALLOW='manifest.txt 2026-08-13 x'; check_against main" 2>&1 ) && rc=0 || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "SELF-TEST FAIL: adding a new migration (+ manifest append) was reported: $out"; fails=$((fails+1))
    fi

    # Fixture 3: deleting a migration must be caught.
    ( cd "$tmp" && git checkout -q -b del main \
        && git rm -q "$SCHEMA_DIR/100-a.sql" && git commit -qm del ) >/dev/null 2>&1
    out=$( cd "$tmp" && SCHEMA_DIR="$SCHEMA_DIR" bash -c "$(declare -f is_allowed check_against); ALLOW=''; check_against main" 2>&1 ) && rc=0 || rc=$?
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

self_test >/dev/null || { echo "migrations-append-only: SELF-TEST FAILED — refusing to report on the tree"; exit 1; }

BASE=$(resolve_base) || {
    echo "migrations-append-only: no trunk ref found (tried origin/main, forge/main, main)" >&2
    echo "  Cannot tell which migrations are history without one. Fetch the trunk." >&2
    exit 1
}

if check_against "$BASE"; then
    echo "migrations-append-only: clean — no existing migration modified against $BASE"
    exit 0
fi
echo "migrations-append-only: applied migrations were edited; see above"
exit 1
