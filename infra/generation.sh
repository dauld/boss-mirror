#!/usr/bin/env bash
# generation.sh — the ONE definition of the deploy generation store.
# Sourced (not executed) by infra/gcp/install-cli-from-image.sh — and,
# until the bare-metal deploy path was deleted on 2026-09-18, by the
# three deploy scripts — so the paths and the atomic-flip mechanics cannot
# drift between the three (CLAUDE.md §9a — a fact that lives twice).
#
# Layout (docs/design/deployment-as-network.md, Q1):
#
#   /usr/local/boss/
#     releases/<sha>/        one generation, keyed by the deployed
#       bin/                 HEAD short sha: service + helper binaries
#       web-dist/            the SPA bundle (rolls back with the code)
#       step-plugins/        step-plugin JS bundles
#       .boss-src-fingerprint  stamp of the sources the bins came from
#     current -> releases/<sha>    the live generation (atomic flip)
#     previous -> releases/<sha>   the one before it (revert target)
#     state/
#       deploy-confirm.pending   marker armed at flip, cleared by the
#                                confirm verdict (boss-deploy-confirm)
#       deploy-history.log       append-only activate/confirm/revert log
#
# Units exec through the symlink (ExecStart=/usr/local/boss/current/
# bin/<name>), and every /usr/local/bin/boss-* the deploy manages is a
# symlink through `current` too — so one flip re-points binaries, web
# dist and step-plugins together, and a revert is seconds, not a build.
# shellcheck shell=bash

BOSS_GEN_ROOT="${BOSS_GEN_ROOT:-/usr/local/boss}"
GEN_RELEASES="$BOSS_GEN_ROOT/releases"
GEN_STATE="$BOSS_GEN_ROOT/state"
GEN_PENDING="$GEN_STATE/deploy-confirm.pending"
GEN_HISTORY="$GEN_STATE/deploy-history.log"
# Q1/Q3: retain the 3 newest generations; prune the rest (with sizes —
# this box has had its disk-full day).
GEN_KEEP=3

# Atomically re-point a symlink. `ln -sfn` alone unlinks then re-creates
# (a reader can catch the gap); symlink-to-temp + `mv -T` is one rename,
# which is the atomic activation step the design leans on.
gen_atomic_link() {
    local target="$1" link="$2"
    local tmp="${link}.tmp.$$"
    ln -sfn "$target" "$tmp"
    mv -Tf "$tmp" "$link"
}

# The generation key for a checkout: the deployed HEAD short sha —
# what the conductor records after pull; the fingerprint pre-flight
# verifies HEAD's content. Empty output when there is no git metadata
# (tarball deploy); callers decide their own fallback.
gen_head_key() {
    git -C "${1:?gen_head_key needs a repo root}" rev-parse --short HEAD 2>/dev/null || true
}

# Key (basename) of what a store symlink points at, or "" if unset.
gen_link_key() {
    local t
    t="$(readlink "$BOSS_GEN_ROOT/$1" 2>/dev/null || true)"
    [[ -n "$t" ]] && basename "$t"
    return 0
}

# Append one timestamped line to the deploy history — the on-disk
# record the confirm verdicts and reverts land in.
gen_log() {
    mkdir -p "$GEN_STATE"
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >> "$GEN_HISTORY"
}
