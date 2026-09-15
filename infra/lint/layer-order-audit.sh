#!/usr/bin/env bash
#
# layer-order-audit — the network layers must stack, not tangle.
#
# THE PRINCIPLE
# -------------
# BOSS is described as a stack of layers: BossMemory (the append-only
# log and the projections that are pure functions of it), BossNET (the
# substrate — packets, stations, routes, admission), BossProtocols
# (the operating model, carried as registry data), BossActors (the
# contract by which a human or agent attaches, proves capability,
# claims, escalates) and BossApps (thin lenses over all of it).
#
# The framing is only worth the words if it constrains something. Each
# name carries a prohibition:
#
#   BossMemory   must not depend on anything above it
#   BossNET      must not know what work MEANS
#   BossProtocols must not require a deploy to change
#   BossActors   is the attach contract, not where logic lives
#   BossApps     must stay thin
#
# This checks the two of those that are statically checkable from the
# repo: the dependency ORDER between layers, and two SHAPE rules that
# catch the layer confusions found by hand on 2026-08-13.
#
# WHAT THIS IS NOT
# ----------------
# This is not the tier check. `tier-import-audit.sh` answers "how
# domain-specific is this crate?" (core / modules / tenants) and is
# enforced across all 55 crates. Layer answers "which layer of the
# network is this?" and is defined only for the network machinery
# inside crates/core — about a third of that tier. The two axes are
# orthogonal and both are load-bearing; see
# docs/design/crates-and-layers.md.
#
# Crates with no layer are not violations. Most of core is shared
# services (calendar, search, docs, locations) or Subject kinds, and
# those ride ON the network rather than being part of it. An
# unclassified crate is simply not checked.
#
# SECTION A — dependency order
# ----------------------------
# A crate at layer N must not path-depend on a crate at a layer above
# N. When this was first run (2026-08-13) it was already clean: the
# layering had emerged without anyone enforcing it. Section A is
# therefore a ratchet against future drift, not a cleanup.
#
# SECTION B — layer shape, ratcheted
# ----------------------------------
# Section A works on Cargo.toml edges, so it is blind to a file in the
# WRONG CRATE — which is exactly how both known confusions look. The
# memory crate contains `claude_dispatcher.rs` (spawns `claude
# --print`, tracks per-agent concurrency and cost: an Actors concern)
# and `tail_http.rs` (an HTTP read surface: an Apps concern). Neither
# creates a backward package edge, so neither is visible to A.
#
# Section B checks two shapes inside memory-layer crates:
#   memory-no-executor — must not implement AgentDispatcher
#   memory-no-http-server — must not serve HTTP (axum routing)
#
# A ratchet, not a ban, following idempotence-ratchet and
# dispatcher-rules-ratchet: today's two known files are allow-listed
# with their reason, and the count must not grow. Fixing one means
# deleting its allow-list line.
#
# Usage: infra/lint/layer-order-audit.sh [--self-test]
# Exit:  0 clean / 1 violations or self-test failure

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# --- the layer map ---------------------------------------------------------
# Rank order. A crate may depend downward and sideways, never upward.
#   0 foundation  primitives and thin client/type crates; no layer of
#                 their own, depended on by everything
#   1 memory      BossMemory — the log and its projections
#   2 net         BossNET — substrate: packets, stations, routes
#   3 protocols   BossProtocols — the operating model as data
#   4 actors      BossActors — the attach contract
#   5 apps        BossApps — thin lenses
#
# `boss-jobs` is deliberately marked `net` although it currently holds
# BOTH the substrate and the protocol registry (30k lines, the largest
# crate in the repo). Marking it at the lower of the two layers is the
# conservative choice: it keeps the check honest about what boss-jobs
# is allowed to depend ON, and it is the seam a future split follows.
layer_of() {
    case "$1" in
        boss-core|boss-ports|boss-expr) echo foundation ;;
        *-client)                       echo foundation ;;
        # boss-nats is a broker driver, NOT BossNET. The first draft of
        # this map ranked it `net` and the check immediately flagged
        # boss-events -> boss-nats as a backward edge, which is how the
        # naming hazard surfaced: "NET" reads as networking-the-wire,
        # but BossNET is the PACKET substrate — stations, routes,
        # admission. The wire sits underneath everything, including the
        # log. Expect this confusion to recur; it is the one place the
        # throwback name costs something.
        boss-nats)                      echo foundation ;;
        boss-events)                    echo memory ;;
        boss-jobs)                      echo net ;;
        boss-policy)                    echo protocols ;;
        boss-dispatcher)                echo actors ;;
        boss-gateway|boss-views)        echo apps ;;
        *)                              echo "" ;;
    esac
}

rank_of() {
    case "$1" in
        foundation) echo 0 ;;
        memory)     echo 1 ;;
        net)        echo 2 ;;
        protocols)  echo 3 ;;
        actors)     echo 4 ;;
        apps)       echo 5 ;;
        *)          echo -1 ;;
    esac
}

# --- Section B allow-list --------------------------------------------------
# One line per known exception: "<crate>/<file> <rule-id> <reason>".
# Deleting a line is how a fix is recorded. Adding one requires a
# reviewer to agree the layer boundary genuinely does not apply.
read -r -d '' SHAPE_ALLOW <<'ALLOW' || true
boss-events/claude_dispatcher.rs memory-no-executor Actors concern in the memory crate; eviction proposed in docs/design/crates-and-layers.md
boss-events/dispatcher.rs memory-no-executor StubDispatcher — same eviction as claude_dispatcher.rs; found by this check, not by the hand pass
boss-events/tail_http.rs memory-no-http-server Apps concern in the memory crate; eviction proposed in docs/design/crates-and-layers.md
ALLOW

allow_count=$(printf '%s\n' "$SHAPE_ALLOW" | grep -c . || true)

# --- Section A -------------------------------------------------------------
# $1 = crates root to scan
check_order() {
    local root="$1" violations=0
    local toml crate layer rank dep dlayer drank
    for toml in $(find "$root" -name Cargo.toml -type f | sort); do
        crate=$(basename "$(dirname "$toml")")
        layer=$(layer_of "$crate")
        [ -z "$layer" ] && continue
        rank=$(rank_of "$layer")
        for dep in $(grep -oE '^boss-[a-z-]+' "$toml" | sort -u); do
            [ "$dep" = "$crate" ] && continue
            dlayer=$(layer_of "$dep")
            [ -z "$dlayer" ] && continue
            drank=$(rank_of "$dlayer")
            if [ "$drank" -gt "$rank" ]; then
                echo "VIOLATION [order]: $crate ($layer) depends on $dep ($dlayer) — layers must not point upward"
                violations=$((violations+1))
            fi
        done
    done
    return "$violations"
}

# --- Section B -------------------------------------------------------------
# $1 = crates root to scan
check_shape() {
    local root="$1" violations=0
    local dir crate f rel hit
    for dir in $(find "$root" -name Cargo.toml -type f -exec dirname {} \; | sort); do
        crate=$(basename "$dir")
        [ "$(layer_of "$crate")" = "memory" ] || continue
        for f in $(find "$dir/src" -name '*.rs' 2>/dev/null | sort); do
            rel="$crate/$(basename "$f")"

            # memory-no-executor: implementing the dispatcher port means
            # this file decides how work RUNS, which is Actors.
            if grep -qE 'impl[[:space:]].*AgentDispatcher[[:space:]]+for' "$f"; then
                hit="$rel memory-no-executor"
                if grep -q "^$rel memory-no-executor " <<< "$SHAPE_ALLOW"; then
                    :
                else
                    echo "VIOLATION [memory-no-executor]: $rel implements AgentDispatcher — the memory layer must not run work"
                    violations=$((violations+1))
                fi
            fi

            # memory-no-http-server: serving routes makes this a door,
            # which is Apps. Reading the log over HTTP is fine — it just
            # belongs on the other side of the boundary.
            if grep -qE '^use axum::|axum::Router|Router::new\(\)' "$f"; then
                if grep -q "^$rel memory-no-http-server " <<< "$SHAPE_ALLOW"; then
                    :
                else
                    echo "VIOLATION [memory-no-http-server]: $rel serves HTTP — the door belongs in the apps layer"
                    violations=$((violations+1))
                fi
            fi
        done
    done
    return "$violations"
}

# --- self-test -------------------------------------------------------------
# A check that has never been seen to fail is indistinguishable from a
# check that cannot fail. Section A is expected to be clean against the
# real tree, so its only proof is a planted violation.
self_test() {
    local tmp rc fails=0 out
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' RETURN

    # Fixture 1: a backward edge — memory depending on apps.
    mkdir -p "$tmp/a/boss-events"
    cat > "$tmp/a/boss-events/Cargo.toml" <<'EOF'
[package]
name = "boss-events"
[dependencies]
boss-gateway = { path = "../boss-gateway" }
EOF
    mkdir -p "$tmp/a/boss-gateway"
    printf '[package]\nname = "boss-gateway"\n' > "$tmp/a/boss-gateway/Cargo.toml"

    out=$(check_order "$tmp/a" 2>&1) && rc=0 || rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "SELF-TEST FAIL: planted backward edge (memory -> apps) was not caught"
        fails=$((fails+1))
    elif ! printf '%s' "$out" | grep -q 'VIOLATION \[order\]'; then
        echo "SELF-TEST FAIL: backward edge caught but not reported as an order violation"
        fails=$((fails+1))
    fi

    # Fixture 2: a legal downward edge must NOT be caught.
    mkdir -p "$tmp/b/boss-gateway"
    cat > "$tmp/b/boss-gateway/Cargo.toml" <<'EOF'
[package]
name = "boss-gateway"
[dependencies]
boss-events = { path = "../boss-events" }
EOF
    mkdir -p "$tmp/b/boss-events"
    printf '[package]\nname = "boss-events"\n' > "$tmp/b/boss-events/Cargo.toml"

    if ! check_order "$tmp/b" >/dev/null 2>&1; then
        echo "SELF-TEST FAIL: legal downward edge (apps -> memory) was reported as a violation"
        fails=$((fails+1))
    fi

    # Fixture 3: an un-allow-listed shape violation in a memory crate.
    mkdir -p "$tmp/c/boss-events/src"
    printf '[package]\nname = "boss-events"\n' > "$tmp/c/boss-events/Cargo.toml"
    printf 'impl AgentDispatcher for Thing {}\n' > "$tmp/c/boss-events/src/runner.rs"

    out=$(check_shape "$tmp/c" 2>&1) && rc=0 || rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "SELF-TEST FAIL: planted AgentDispatcher impl in a memory crate was not caught"
        fails=$((fails+1))
    fi

    # Fixture 4: the same shape, but allow-listed, must be suppressed.
    mkdir -p "$tmp/d/boss-events/src"
    printf '[package]\nname = "boss-events"\n' > "$tmp/d/boss-events/Cargo.toml"
    printf 'impl AgentDispatcher for Thing {}\n' > "$tmp/d/boss-events/src/claude_dispatcher.rs"

    if ! check_shape "$tmp/d" >/dev/null 2>&1; then
        echo "SELF-TEST FAIL: allow-listed shape exception was still reported"
        fails=$((fails+1))
    fi

    if [ "$fails" -eq 0 ]; then
        echo "self-test: 4/4 fixtures behaved as specified"
        return 0
    fi
    return 1
}

if [ "${1:-}" = "--self-test" ]; then
    self_test
    exit $?
fi

self_test >/dev/null || { echo "layer-order-audit: SELF-TEST FAILED — refusing to report on the tree"; exit 1; }

total=0
check_order crates || total=$((total+$?))
check_shape crates || total=$((total+$?))

if [ "$total" -eq 0 ]; then
    echo "layer-order-audit: clean (order + shape); $allow_count known shape exception(s) allow-listed"
    exit 0
fi
echo "layer-order-audit: $total violation(s)"
exit 1
