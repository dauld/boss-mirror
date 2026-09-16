#!/usr/bin/env bash
# boss — the BOSS CLI on boss-gcp, as installed by boss-gcp-converge.
#
# This file is copied to <store>/boss (the store is /opt/boss-cli on the
# host) and /usr/local/bin/boss is a symlink to it. It execs the CLI of
# the generation `current` points at, and it exists for ONE reason: the
# binary in the cluster image is compiled with BOSS_CLI_BUILT_FROM=unknown
# (no git in the build stage, and no BOSS_BUILD_COMMIT at compile time
# since 2026-09-12, so a train without a Rust change ships without a
# Rust build) and reads BOSS_BUILD_COMMIT at RUNTIME instead
# (crates/orchestrators/boss-cli/src/built_from.rs). A binary copied out
# of the image and run bare says `built from unknown`, and every host
# verb that shells to the CLI refuses that by name (publish-workflow's
# floor check, exit 78 — backlog 6f58e9a1). So the wrapper supplies the
# commit, and it supplies it from the ONE place the sha is defined: the
# generation directory's name, which install-cli-from-image.sh created
# for exactly that sha and confirmed through this wrapper before linking
# `current` at it. Nothing else carries the sha — no sidecar file, no
# text baked into this script — so nothing can disagree with it
# (CLAUDE.md §9a).
#
# Resolved relative to THIS file's real location, the way the pod's
# infra/dev/boss reads its sor-url: /usr/local/bin/boss is a symlink
# here, and readlink -f follows it to the store.
#
# It does not default BOSS_JOBS_URL: every unit on this host pins the
# system of record inline with env(1) (the ops runner's drop-in, the
# converge's Exec lines) because this host's 127.0.0.1 is the legacy
# second stack, and a default here would be a second spelling of the
# system of record that could point at the wrong one. A bare `boss` verb
# with no BOSS_JOBS_URL refuses and names the fix, which is right.
store="$(dirname "$(readlink -f "$0")")"
gen="$(readlink -f "$store/current" 2>/dev/null)"
if [ -z "$gen" ] || [ ! -x "$gen/boss" ]; then
    echo "boss: no CLI generation is linked at $store/current — boss-gcp-converge installs one from the cluster image on each tick and records cli_result on its packet (maintenance-boss-gcp-converge); that packet says why there is none" >&2
    exit 127
fi
exec env BOSS_BUILD_COMMIT="$(basename "$gen")" "$gen/boss" "$@"
