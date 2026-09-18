#!/usr/bin/env bash
# rule-tests-seed-the-directory — a test that reads `dispatcher_rules`
# through `load_active_rules` seeds `infra/dispatcher/rules/` first, in
# the same file, or it is named here and refused.
#
# WHY THIS EXISTS (backlog 488cadca). Since the one-file-per-rule
# collapse (41ba00cd) the authored directory is the registry's
# definition and the table is DERIVED from it by the dispatcher's boot
# step, `boss_dispatcher::rules::seed::seed_authored_rules`. A rule
# change is a `version` bump in its file and writes no migration
# (`no-migration-writes-a-dispatcher-rule.sh`), so a fresh TestDb holds
# only the LAST MIGRATED version of every rule. A pg-backed test that
# calls `load_active_rules` alone therefore passes against rows the tree
# no longer ships and says nothing about the files the dispatcher boots
# from. Measured 2026-09-14: feedback_obligation_rules.rs pinned
# `complete-feedback-branch-on-car-merged` at the migration's v3 while
# the file said v4 (a second route the test never saw); it was fixed by
# hand on #370, and rule_payload_contract.rs had the same shape. The
# shape is invisible until a rule file changes, which is exactly when the
# test is supposed to speak — so a mechanism, not a note.
#
# THE CHECKED PROPERTY. Every Rust file under a crate's `tests/`
# directory whose code mentions `load_active_rules` also mentions
# `seed_authored_rules`. Whole-line comments are not code: a docstring
# may describe the load without seeding anything. File granularity is
# the mechanism the packet asked for — the seeded shape lives in ONE
# helper (crates/core/boss-dispatcher/tests/common/mod.rs, which carries
# both names), and a test that goes through the helper mentions neither.
#
# THE SECOND CHECKED PROPERTY (backlog 94f150f9, 2026-09-14). No Rust
# file under a crate's `tests/` directory carries an INLINE COPY of an
# authored rule: a code line `name = "<x>"` (the TOML key, as it sits
# inside a raw-string fixture) where `infra/dispatcher/rules/<x>.toml`
# exists. A copy tests the copy: five selection tests carried one, and
# the day this was measured `spawn-car-on-sweep-remediated`'s copy
# declared no version while its file said 3 — the same shape
# backlog_item_advance_rule.rs had, whose v3 copy passed against a v4
# file until replaced. A test about one authored rule reads its file
# (`common::authored_rule`); a SYNTHETIC rule — a name with no file,
# which rules_reload_pg / rules_wait_pg / the authoring e2e write on
# purpose — is not a copy of anything and passes. Named by file:line.
#
# WHAT THIS DOES NOT SEE, stated rather than discovered: a
# `#[cfg(test)]` module under `src/` (none reads the table today — the
# runtime callers in http.rs and registry.rs are the boot path itself),
# and a file that seeds in one test function and loads unseeded in
# another. Either would be a new shape; widen the check when one appears.
#
# EXIT STATUS: 0 clean, 1 a test file is named (by either property). Reads the working tree
# with `find`, never git, so there is nothing here that can refuse
# (lib/git-answer.sh's exit 3 is for lints that ask git a question).
#
# Usage:  infra/lint/rule-tests-seed-the-directory.sh

set -uo pipefail

NAME="rule-tests-seed-the-directory"
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh

LOAD="load_active_rules"
SEED="seed_authored_rules"

# Does $1's CODE mention $2? Whole-line `//` comments are skipped so the
# prose of a test's header can tell the story this lint enforces.
code_mentions() { # file ident
    LC_ALL=C awk -v ident="$2" '
        $0 ~ /^[ \t]*\/\// { next }
        index($0, ident) > 0 { found = 1; exit }
        END { exit found ? 0 : 1 }
    ' "$1"
}

# Every offending file under $1, one path per line. Empty = clean. Also
# reports on stderr how many files were judged, so a run that judged
# nothing reads as one.
check_dir() { # root
    local root="$1" f judged=0 named=0
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        code_mentions "$f" "$LOAD" || continue
        judged=$((judged + 1))
        if ! code_mentions "$f" "$SEED"; then
            named=$((named + 1))
            printf '%s\n' "$f"
        fi
    done <<EOF
$(find "$root" -path '*/tests/*' -name '*.rs' -type f -not -path '*/target/*' | LC_ALL=C sort)
EOF
    echo "$NAME: $judged test file(s) call $LOAD, $named without a seed" >&2
    [ "$named" -eq 0 ]
}

RULES_DIR="infra/dispatcher/rules"

# Every inline copy of an authored rule under $1, as `file:line:name`,
# one per line. A copy is a CODE line `name = "<x>"` (double or single
# TOML quotes) whose `<x>` is a file in $RULES_DIR. Whole-line comments
# are skipped, as above; `const RULE: &str = "<x>";` — the fixed shape —
# is not a TOML key and does not match.
inline_copies() { # root
    local root="$1" candidates f line name
    candidates="$(find "$root" -path '*/tests/*' -name '*.rs' -type f -not -path '*/target/*' \
        | LC_ALL=C sort | while IFS= read -r f; do
        [ -n "$f" ] || continue
        LC_ALL=C awk '
            $0 ~ /^[ \t]*\/\// { next }
            match($0, /^[ \t]*name[ \t]*=[ \t]*["\x27][^"\x27]+["\x27]/) {
                s = substr($0, RSTART, RLENGTH)
                sub(/^[^"\x27]*["\x27]/, "", s); sub(/["\x27]$/, "", s)
                print FILENAME ":" FNR ":" s
            }
        ' "$f"
    done)"
    [ -n "$candidates" ] || return 0
    while IFS=: read -r f line name; do
        [ -n "$name" ] || continue
        [ -f "$RULES_DIR/$name.toml" ] && printf '%s:%s:%s\n' "$f" "$line" "$name"
    done <<EOF
$candidates
EOF
    return 0
}

# ---------------------------------------------------------------------------
# Self-test — fixtures in a temp directory this run owns, never in
# infra/lint/ or under crates/, where a file is discovered as real.
# ---------------------------------------------------------------------------
tmp="$(mktemp -d)" || exit 1
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/crate/tests/common" "$tmp/crate/src"

# Seeded in the same file: the fixed shape.
printf 'async fn shipped(db: &TestDb) -> Registry {\n    seed_authored_rules(&db.pool, RULES_DIR).await.unwrap();\n    Registry::from_raw(load_active_rules(&db.pool).await.unwrap()).unwrap()\n}\n' \
    > "$tmp/crate/tests/common/mod.rs"
# The load named only in prose: not a read of the table.
printf '//! Loads the registry the way the service does: seed, then load_active_rules.\nmod common;\n#[test] fn t() { let _ = common::shipped; }\n' \
    > "$tmp/crate/tests/through_the_helper.rs"
# A runtime caller under src/ is the boot path, not a test.
printf 'pub async fn serve(pool: &PgPool) { let _ = load_active_rules(pool).await; }\n' \
    > "$tmp/crate/src/http.rs"
if ! check_dir "$tmp" 2>/dev/null; then
    echo "$NAME: SELF-TEST FAILED — a seeded helper, a prose mention and a src/ caller must all pass" >&2
    exit 1
fi
# The refused shape: a test reading the table with no seed in the file.
printf 'use boss_dispatcher::rules::registry::load_active_rules;\n#[tokio::test] async fn t() { let db = TestDb::new().await; let _ = load_active_rules(&db.pool).await; }\n' \
    > "$tmp/crate/tests/pins_the_migration.rs"
hits="$(check_dir "$tmp" 2>/dev/null)"
case "$hits" in
    "$tmp/crate/tests/pins_the_migration.rs") ;;
    *) echo "$NAME: SELF-TEST FAILED — an unseeded load must be refused BY FILE; got: [$hits]" >&2; exit 1 ;;
esac
rm -rf "$tmp/crate"

# The second property. An authored name is read from the directory, not
# typed here, so the fixture stays true as rules come and go.
authored="$(find "$RULES_DIR" -maxdepth 1 -name '*.toml' -printf '%f\n' | LC_ALL=C sort | sed -n 1p | sed 's/\.toml$//')"
[ -n "$authored" ] || { echo "$NAME: SELF-TEST FAILED — no rule files under $RULES_DIR" >&2; exit 1; }
mkdir -p "$tmp/crate/tests"
# The fixed shape, a synthetic rule, a template, and a comment: all pass.
printf 'mod common;\nconst RULE: &str = "%s";\n// name = "%s" is what this used to inline\nconst SYNTHETIC: &str = r#"\n[[rule]]\nname = "a-rule-no-file-declares"\non_event = "x"\n"#;\nconst TEMPLATE: &str = r#"\n[[rule]]\nname = "{name}"\n"#;\n' \
    "$authored" "$authored" > "$tmp/crate/tests/reads_the_file.rs"
hits="$(inline_copies "$tmp")"
if [ -n "$hits" ]; then
    echo "$NAME: SELF-TEST FAILED — the fixed shape, a synthetic rule, a template and a comment must all pass; got: [$hits]" >&2
    exit 1
fi
# The refused shape: the authored rule's TOML copied into a fixture.
printf 'const RULE: &str = r#"\n[[rule]]\nname = "%s"\non_event = "x"\n[[rule.do]]\nhandler = "y"\n"#;\n' \
    "$authored" > "$tmp/crate/tests/carries_a_copy.rs"
hits="$(inline_copies "$tmp")"
case "$hits" in
    "$tmp/crate/tests/carries_a_copy.rs:3:$authored") ;;
    *) echo "$NAME: SELF-TEST FAILED — an inline copy of $authored must be refused by file:line; got: [$hits]" >&2; exit 1 ;;
esac
rm -rf "$tmp/crate"

# ---------------------------------------------------------------------------
# The tree.
# ---------------------------------------------------------------------------
[ -d crates ] || { echo "$NAME: crates/ does not exist" >&2; exit 1; }
if ! hits="$(check_dir crates)"; then
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        echo "$NAME: $f reads dispatcher_rules through $LOAD without seeding infra/dispatcher/rules first" >&2
    done <<EOF
$hits
EOF
    cat >&2 <<'MSG'

  A fresh TestDb holds the LAST MIGRATED version of every dispatcher
  rule, and a rule change is a version bump in its file with no migration
  — so a test that reads the table alone pins a row the tree no longer
  ships (backlog 488cadca; feedback_obligation_rules.rs pinned v3 while
  the file said v4). Load the registry the way the dispatcher boots:

    seed_authored_rules(&db.pool, boss_testing::dispatcher_rules_dir()) then load_active_rules

  or go through crates/core/boss-dispatcher/tests/common/mod.rs, which
  does exactly that (`shipped_registry` / `shipped_raw_rules`).
MSG
    exit 1
fi

copies="$(inline_copies crates)"
if [ -n "$copies" ]; then
    while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        echo "$NAME: $hit is an inline copy of an authored rule" >&2
    done <<EOF
$copies
EOF
    cat >&2 <<'MSG'

  A selection test that carries its own copy of a rule tests the copy:
  when the file changes (a when-clause, an arg, a version) the copy keeps
  passing against text the dispatcher no longer boots from (backlog
  94f150f9; spawn-car-on-sweep-remediated's copy was two versions
  behind). Read the file instead:

    mod common;
    let reg = common::authored_rule("<name>");

  (crates/core/boss-dispatcher/tests/common/mod.rs). A synthetic rule —
  a name with no file under infra/dispatcher/rules/ — is not a copy and
  is not what this refuses.
MSG
    exit 1
fi

lint_scanned "$NAME" "$(find crates -path '*/tests/*' -name '*.rs' -type f -not -path '*/target/*' | wc -l | tr -d ' ')" "test file(s) under crates/"
echo "$NAME: self-test ok — a seeded helper, a prose mention and a src/ caller pass; an unseeded test load is refused by file; an inline copy of an authored rule is refused by file:line"
echo "$NAME: ok — every test file that calls $LOAD seeds the authored directory in the same file, and none carries an inline copy of an authored rule"
exit 0
