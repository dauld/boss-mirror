#!/usr/bin/env bash
#
# a-boot-check-cannot-fail-the-boot — the system of record starts even
# when a boot-time check does not like what it reads.
#
# THE INCIDENT
# ------------
# 2026-09-07, twice in one night (packet 7752e636). The boot-time
# Workflow viability check found one active row unviable, refused to
# quarantine it because an open Job was pinned to it, and then refused
# to start the jobs API. The gateway never bound, the pod crash-looped,
# and the system of record was dark from 22:26 to 22:33 — then again
# from 23:16 to 23:24, because the fix-forward's own train could not
# land while the record it needed was down. One registry row took the
# whole service off the air. The same check had already done it once
# before, on 2026-08-13, with a bare exit(1).
#
# The contract that came out of it, in CLAUDE.md's words: an arm that
# needs the patient is not an arm. A boot check CHECKS and LOGS. It
# never writes, and it never decides the service may not start.
#
# WHY A LINT AND NOT A COMMENT
# -----------------------------
# That contract was written into the doc comments of both checks the
# day it was agreed, and a doc comment is not a mechanism (CLAUDE.md
# §9a). The signature is. A function that returns nothing cannot hand
# `main` a failure to propagate, so the shape of the declaration is
# what holds the promise — and that is what this reads.
#
# WHAT THIS CHECKS
# ----------------
# In the jobs-API binary, every `async fn verify_*_viability`:
#   1. declares no return type — nothing to propagate, nothing to `?`;
#   2. contains no exit, panic, unwrap or expect;
#   3. names a `*_quarantine` module, and that module is likewise free
#      of exit, panic, unwrap and expect.
# And the binary as a whole must not say it is refusing to start.
#
# It FAILS when it finds no boot check at all. A check that matches
# nothing passes trivially, and a green that covers nothing is the
# failure mode this packet is about.
#
# WHAT THIS DOES NOT CHECK
# -------------------------
# That a boot check performs no WRITE. There is no honest grep for
# that — the registries' write methods are ordinary trait calls, and
# an enumeration of their names would go stale silently, which is the
# defect class rather than a fix for it. That half is pinned per pass
# by tests that hand the check a registry and assert it recorded no
# events (`station_boot_quarantine`, `workflow_boot_quarantine`).
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
NAME=a-boot-check-cannot-fail-the-boot

# Anything that ends the process or unwinds out of a boot check. `?`
# is deliberately absent: a function with no return type cannot use
# it, so rule 1 already forbids it.
FATAL='std::process::exit|process::exit|panic!|unreachable!|todo!|\.unwrap\(|\.expect\('

# Strip line comments before grepping, so the prose ABOUT the incident
# — which naturally names exit and panic — is not read as code.
code_only() { sed 's://.*::' "$1"; }

# judge <api-file> <src-dir> — prints `ok`, or the reason it is not.
# Always exits 0; the caller decides what a reason means.
judge() {
    local api="$1" src="$2" checks fn sig body module path
    [ -f "$api" ] || { echo "no such file: $api"; return 0; }

    checks=$(grep -oE 'async fn verify_[a-z_]*_viability' "$api" | sed 's/async fn //' | sort -u)
    if [ -z "$checks" ]; then
        echo "no 'async fn verify_*_viability' found"
        return 0
    fi

    for fn in $checks; do
        # The signature runs from the declaration to the line that
        # opens the body; a return type may sit on any of them.
        sig=$(awk -v fn="async fn $fn" '
            index($0, fn) { inside = 1 }
            inside { print }
            inside && /\{[[:space:]]*$/ { exit }
        ' "$api")
        if printf '%s' "$sig" | grep -q -- '->'; then
            echo "$fn declares a return type"
            return 0
        fi

        body=$(awk -v fn="async fn $fn" '
            index($0, fn) { inside = 1 }
            inside && /\{[[:space:]]*$/ { started = 1 }
            inside { print }
            started && /^\}/ { exit }
        ' "$api" | sed 's://.*::')
        if printf '%s' "$body" | grep -qE "$FATAL"; then
            echo "$fn can end the process: $(printf '%s' "$body" | grep -oE "$FATAL" | sort -u | tr '\n' ' ')"
            return 0
        fi

        for module in $(printf '%s' "$body" | grep -oE '[a-z_]+_quarantine' | sort -u); do
            path="$src/$module.rs"
            if [ ! -f "$path" ]; then
                echo "$fn calls into $module, which is not at $path"
                return 0
            fi
            if code_only "$path" | grep -qE "$FATAL"; then
                echo "$path can end the process: $(code_only "$path" | grep -oE "$FATAL" | sort -u | tr '\n' ' ')"
                return 0
            fi
        done
    done

    if code_only "$api" | grep -qi 'refusing to start'; then
        echo "$(basename "$api") still says it is refusing to start"
        return 0
    fi

    echo ok
}

# Every shape this lint claims to catch, caught on a fixture, on every
# run. A lint that has never been shown to fail is a green that means
# nothing — which is the whole subject of the incident above.
self_test() {
    local fx api src r
    fx="$(mktemp -d)"; trap 'rm -rf "$fx"' RETURN
    mkdir -p "$fx/src/bin"
    api="$fx/src/bin/boss_jobs_api.rs"
    src="$fx/src"

    clean_module() {
        printf 'pub async fn check(r: &dyn R) -> Result<Report, E> {\n    r.list_active().await\n}\n' \
            > "$src/thing_quarantine.rs"
    }
    good_api() {
        {
            printf '/// Never exits and never writes. It used to panic! and exit.\n'
            printf 'async fn verify_thing_viability(r: &dyn R) {\n'
            printf '    match boss_jobs::thing_quarantine::check(r).await {\n'
            printf '        Ok(_) => {}\n'
            printf '        Err(e) => tracing::error!(error = %%e, "starting anyway"),\n'
            printf '    }\n'
            printf '}\n'
        } > "$api"
    }

    clean_module; good_api
    r=$(judge "$api" "$src")
    [ "$r" = ok ] || { echo "self-test FAILED: a correct boot check was refused: $r" >&2; return 1; }

    clean_module; good_api
    sed -i.bak 's/async fn verify_thing_viability(r: &dyn R) {/async fn verify_thing_viability(r: \&dyn R) -> anyhow::Result<()> {/' "$api"
    r=$(judge "$api" "$src")
    [ "$r" != ok ] || { echo "self-test FAILED: a boot check returning Result passed" >&2; return 1; }

    clean_module; good_api
    sed -i.bak 's/        Ok(_) => {}/        Ok(_) => std::process::exit(1),/' "$api"
    r=$(judge "$api" "$src")
    [ "$r" != ok ] || { echo "self-test FAILED: a boot check that exits passed" >&2; return 1; }

    clean_module; good_api
    printf 'pub async fn check(r: &dyn R) -> Result<Report, E> {\n    panic!("no");\n}\n' > "$src/thing_quarantine.rs"
    r=$(judge "$api" "$src")
    [ "$r" != ok ] || { echo "self-test FAILED: a quarantine module that panics passed" >&2; return 1; }

    clean_module; good_api
    printf 'async fn something_else() {}\n' > "$api"
    r=$(judge "$api" "$src")
    [ "$r" != ok ] || { echo "self-test FAILED: a file with no boot check at all passed" >&2; return 1; }

    clean_module; good_api
    printf 'tracing::error!("refusing to start");\n' >> "$api"
    r=$(judge "$api" "$src")
    [ "$r" != ok ] || { echo "self-test FAILED: a binary that refuses to start passed" >&2; return 1; }

    echo "$NAME: self-test ok — a returning check, an exit, a panicking module, an empty file and a refusal to start are each refused"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

cd "$here/../.."
API=crates/core/boss-jobs/src/bin/boss_jobs_api.rs
verdict=$(judge "$API" crates/core/boss-jobs/src)
if [ "$verdict" != ok ]; then
    echo "$NAME: FAIL — $verdict" >&2
    echo "  A boot check reports and starts. On 2026-09-07 one that decided" >&2
    echo "  otherwise took the system of record down twice (packet 7752e636)." >&2
    exit 1
fi

n=$(grep -oE 'async fn verify_[a-z_]*_viability' "$API" | sort -u | wc -l | tr -d ' ')
echo "$NAME: $n boot check(s) return nothing and cannot end the process"
