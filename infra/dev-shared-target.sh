#!/usr/bin/env bash
# dev-shared-target.sh — point every cargo build on this machine at ONE
# target directory, instead of one per worktree.
#
# WHY, measured on 2026-08-16/17. An agent creates a git worktree per
# task and each one grows its own `target/`. Four of them reached 49GB
# combined and took the laptop to 324MB free, which killed a full gate
# mid-run and produced a failure that had nothing to do with the code
# (packet 865992c1 is the gate's half of that; this is the disk's).
# The main checkout's `target/` reached 81GB on its own. There are 35
# worktrees on this machine today.
#
# WHAT IS ACTUALLY SHARED, measured rather than assumed. In a debug
# tree: `deps/` is 2.7G of 4.3G (63%) and holds 486 rlibs, of which
# **461 are third-party and 25 are ours** — 95% of the compiled
# artifacts come from one Cargo.lock and are byte-identical across
# worktrees. So the saving is roughly the deps fraction for every
# worktree after the first.
#
# WHAT IS NOT SHARED, and why this is not a 100% win: `incremental/`
# is 1.5G (35%) and is per-build state, not a cache two trees can
# meaningfully pool.
#
# THE REAL TRADEOFF, and the reason this is opt-in rather than a
# committed default. Cargo takes an EXCLUSIVE lock on a target
# directory for the duration of a build, so two gates running against
# a shared one serialize. On the night this was filed there were
# frequently two or three gates running at once and that concurrency
# was worth having. Space or parallelism; pick per machine, per day.
#
# WHY NOT `.cargo/config.toml` IN THE REPO: the setting needs an
# ABSOLUTE path (a relative one resolves per-worktree and shares
# nothing), and an absolute path is machine-specific, so committing it
# is wrong. It also must never reach CI, where each job gets a fresh
# volume and sharing means nothing. Both problems disappear if the
# setting lives in the user's own cargo config, which is what this
# writes.
#
# WHAT IS SHARED THAT SHOULD NOT BE, measured 2026-09-10. Cargo's unit
# hash does not include the worktree path, so two checkouts of the same
# commit share one rlib AND one build-script OUT_DIR. That is exactly
# the saving above for the 461 third-party rlibs — they are
# byte-identical — but it is wrong for any artifact a build script
# GENERATES from the tree, because those are not identical: they carry
# the generating worktree's paths and its files. Measured case:
# boss-testing's build.rs compiles the migration list from
# `infra/postgres/schema/`; a checkout holding a NEW migration was
# reported `Finished` with nothing recompiled, linked to the list its
# neighbour generated, and every DB-backed test there would have run
# against a schema missing the migration under test. No
# `rerun-if-changed` can see it — the script is not run at all. That
# crate now refuses at run time instead (see the fingerprint check in
# `crates/core/boss-testing/src/test_db.rs`), and a build script added
# here that generates from the tree needs the same kind of guard.
#
# ESCAPE HATCH: `CARGO_TARGET_DIR=... cargo …` on a single invocation
# overrides the config, so deliberately-parallel work does not need
# this turned off globally.
#
# Usage:
#   infra/dev-shared-target.sh --status   # what is in force right now
#   infra/dev-shared-target.sh --on       # share (default ~/.cargo/boss-target)
#   infra/dev-shared-target.sh --on DIR   # share, at DIR
#   infra/dev-shared-target.sh --off      # back to per-worktree target/

set -uo pipefail

CONFIG_DIR="${CARGO_HOME:-$HOME/.cargo}"
CONFIG="$CONFIG_DIR/config.toml"
MARK_BEGIN="# >>> boss dev-shared-target >>>"
MARK_END="# <<< boss dev-shared-target <<<"

# CI must never share: each job gets its own volume, so a shared dir
# buys nothing and a surprise absolute path is a way to lose a build.
if [ -n "${CI:-}" ] || [ -n "${GITHUB_ACTIONS:-}" ] || [ -n "${FORGEJO_ACTIONS:-}" ]; then
    echo "dev-shared-target: refusing to run in CI — each job already has its own volume." >&2
    exit 2
fi

current_target_dir() {
    [ -f "$CONFIG" ] || return 0
    awk '
        /^\[build\]/ { in_build = 1; next }
        /^\[/        { in_build = 0 }
        in_build && /^[[:space:]]*target-dir[[:space:]]*=/ {
            sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); print; exit
        }
    ' "$CONFIG"
}

status() {
    local dir
    dir="$(current_target_dir)"
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        echo "dev-shared-target: CARGO_TARGET_DIR=$CARGO_TARGET_DIR is set in the environment"
        echo "  that overrides the config file for this shell."
    fi
    if [ -z "$dir" ]; then
        echo "dev-shared-target: OFF — every worktree builds its own target/"
        return 0
    fi
    echo "dev-shared-target: ON — $dir"
    if [ -d "$dir" ]; then
        echo "  size: $(du -sh "$dir" 2>/dev/null | cut -f1)"
    else
        echo "  (not created yet; the next build makes it)"
    fi
}

remove_block() {
    [ -f "$CONFIG" ] || return 0
    # Drop a previously written block, leaving anything else intact.
    awk -v b="$MARK_BEGIN" -v e="$MARK_END" '
        $0 == b { skip = 1; next }
        $0 == e { skip = 0; next }
        !skip   { print }
    ' "$CONFIG" > "$CONFIG.tmp" && mv "$CONFIG.tmp" "$CONFIG"
}

case "${1:---status}" in
    --status)
        status
        ;;
    --on)
        dir="${2:-$CONFIG_DIR/boss-target}"
        case "$dir" in
            /*) ;;
            *) echo "dev-shared-target: DIR must be absolute — a relative target-dir resolves per-worktree and shares nothing." >&2; exit 2 ;;
        esac
        existing="$(current_target_dir)"
        if [ -n "$existing" ] && ! grep -qF "$MARK_BEGIN" "$CONFIG" 2>/dev/null; then
            echo "dev-shared-target: $CONFIG already sets build.target-dir = $existing and this script did not write it." >&2
            echo "  Refusing to edit someone else's setting. Remove it by hand, or keep it." >&2
            exit 2
        fi
        mkdir -p "$CONFIG_DIR"
        [ -f "$CONFIG" ] && cp "$CONFIG" "$CONFIG.bak.$$" && echo "dev-shared-target: backed up $CONFIG -> $CONFIG.bak.$$"
        remove_block
        {
            echo "$MARK_BEGIN"
            echo "# Written by infra/dev-shared-target.sh. Remove with --off."
            echo "[build]"
            echo "target-dir = \"$dir\""
            echo "$MARK_END"
        } >> "$CONFIG"
        mkdir -p "$dir"
        echo "dev-shared-target: ON — every cargo build on this machine now uses $dir"
        echo "  Existing per-worktree target/ dirs are NOT removed; they are simply no longer written to."
        echo "  Reclaim them with:  du -sh */target | sort -hr"
        echo "  One build at a time: cargo locks a target dir exclusively, so concurrent gates serialize."
        ;;
    --off)
        if ! grep -qF "$MARK_BEGIN" "$CONFIG" 2>/dev/null; then
            echo "dev-shared-target: nothing written by this script in $CONFIG — leaving it alone."
            status
            exit 0
        fi
        cp "$CONFIG" "$CONFIG.bak.$$" && echo "dev-shared-target: backed up $CONFIG -> $CONFIG.bak.$$"
        remove_block
        echo "dev-shared-target: OFF — worktrees build their own target/ again."
        echo "  The shared directory is left on disk; delete it yourself if you want the space back."
        ;;
    *)
        echo "dev-shared-target: unknown arg: $1 (accepts --status, --on [DIR], --off)" >&2
        exit 2
        ;;
esac
