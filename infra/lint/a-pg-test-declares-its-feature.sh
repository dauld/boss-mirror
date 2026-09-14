#!/usr/bin/env bash
# a-pg-test-declares-its-feature — every integration test that reaches
# for a crate's Postgres adapter is declared to Cargo with
# `required-features = ["postgres"]`, so `cargo test -p <crate>` with
# default features compiles instead of failing on an unresolved import.
#
# WHY. The gate runs `--all-features`, so it never saw this: on
# 2026-09-14 five `*_pg.rs` tests in boss-jobs (a_completion_names_its_
# actor_pg, a_day_of_jobs_lists_newest_first_pg, a_node_declares_its_
# roles_pg, job_edges_pg, station_flow_pg) had no [[test]] entry, and a
# builder's plain `cargo test -p boss-jobs` failed to compile on main
# with `unresolved import boss_jobs::PgJobs` — a red that says nothing
# about the tree. Sixteen sibling tests had the entry; the five were
# the ones added after the last person remembered. A rule remembered is
# not a mechanism (CLAUDE.md §9a); this is the mechanism.
#
# WHAT IT CHECKS. For every crate under crates/ whose Cargo.toml declares
# a `postgres` feature: each tests/*_pg.rs, and each tests/*.rs that
# mentions the crate's Pg adapter (`Pg[A-Z]` or `postgres::`), has a
# `[[test]]` entry naming it with `required-features` including
# "postgres". Reads the tree only; runs on every gate.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

fail=0
checked=0
while IFS= read -r toml; do
    crate_dir="$(dirname "$toml")"
    grep -qE '^\s*postgres\s*=' "$toml" || continue
    [ -d "$crate_dir/tests" ] || continue
    for t in "$crate_dir"/tests/*.rs; do
        [ -f "$t" ] || continue
        name="$(basename "$t" .rs)"
        case "$name" in
            *_pg) needs=1 ;;
            *) grep -qE '\bPg[A-Z][A-Za-z]*\b|::postgres::' "$t" && needs=1 || needs=0 ;;
        esac
        [ "$needs" -eq 1 ] || continue
        checked=$((checked + 1))
        # The [[test]] block naming it must carry required-features with postgres.
        if ! awk -v n="$name" '
            /^\[\[test\]\]/ { inblock=1; named=0; next }
            /^\[/ { inblock=0 }
            inblock && $0 ~ "^name *= *\"" n "\"" { named=1 }
            inblock && named && /required-features/ && /postgres/ { found=1 }
            END { exit found ? 0 : 1 }' "$toml"; then
            echo "a-pg-test-declares-its-feature: $t reaches the Postgres adapter but $toml has no [[test]] name = \"$name\" with required-features = [\"postgres\"] — cargo test with default features fails to compile on it" >&2
            fail=1
        fi
    done
done < <(find crates -name Cargo.toml -not -path '*/target/*')

[ "$fail" -eq 0 ] || exit 1
echo "a-pg-test-declares-its-feature: ok — $checked Postgres-reaching test(s) declare required-features = [\"postgres\"]"
