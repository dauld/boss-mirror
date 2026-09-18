#!/usr/bin/env bash
# seed-brewery-tenant.sh — publish the brewery tenant onto a running
# BOSS stack via the converged prepare step: classes, Workflows, tenant
# policy grants, and the seed data (operators / employees / accounts /
# vendors / messages / FG + raw + asset opening balances), then stamp
# the reset baseline. The seeding is one `boss-brewery-sim prepare`
# call (the prepare_model lib fn) so this path and the offline regen
# drive identical code instead of drifting.
#
# Called by the container launcher (services-launcher.sh) just before
# the brewery sim starts.
# Under the container launcher it runs AFTER `boss tenant publish`
# (seed-tenant.sh, the door every tenant takes; backlog b644d727,
# 2026-09-17), so prepare's own classes post, calendars batch, company
# mint, policy publish, people posts and workflow walk find their rows
# already there and land nothing new — what it still owns is the sim
# data below and the reset-baseline stamp.
# None of this is carried by the audit_log any more — the demo builds
# itself live from an empty log — so the live sim needs these entities
# to exist up front or its job/invoice posts 404 and the playground
# sits idle.
#
# Binaries are PATH-resolved: docker ships them in /usr/local/bin;
# bare-metal callers prepend target/release. Idempotent — safe to
# re-run. Event time on the seeded rows is pinned to the demo epoch
# (BOSS_DEMO_EPOCH_START) so they land on day 0, not wall-time.

set -euo pipefail

EPOCH_START="${BOSS_DEMO_EPOCH_START:-2025-04-01}"
SEEDS_DIR="${BOSS_SIM_SEEDS_DIR:-/opt/boss/examples/brewery/seeds}"

# Seed the whole brewery tenant model — classes, Workflows, policy
# grants, and data (operators / employees / accounts / vendors /
# messages / FG + raw + asset opening balances) — through the public
# API in one call. `boss-brewery-sim prepare` drives the converged
# prepare_model lib fn (classes → Workflows → policy → data), routing
# to each service's own port. Retry while the stack finishes binding
# (the launcher calls this the moment it reaches the sim in its start
# order). Idempotent + retry-safe: each sub-step skips rows that
# already exist, so a retry after a transient bind failure resumes
# cleanly. BOSS_SIM_SEEDS_DIR points at the tenant bundle;
# BOSS_EPOCH_START pins seeded event time to the demo epoch (the
# seeder rebases the clock to (epoch − 1 day) internally).
# Wait for every service prepare writes through to be reachable before
# seeding. On a cold stack the ~24 services take 30-90s to finish binding +
# startup reconciliation; prepare hard-fails on the first unreachable core
# service (classes/jobs/policy/people/accounts) and silently SKIPS the
# base_reachable-guarded optional seeds (catalog/assets/FG), leaving a
# half-seeded, idle demo. Gating on health up front lets prepare run once
# against a fully-up stack instead of racing it. Ports come from
# boss-ports-list (the canonical table shared with the binaries), so a port
# rename lands here for free. Install-only path (Playwright seeds via
# boss-brewery-data-seed, not this script), so waiting for every service is
# always correct here. Degrades to a no-op if the tooling is absent.
wait_for_stack() {
    command -v boss-ports-list >/dev/null 2>&1 || return 0
    command -v curl >/dev/null 2>&1 || return 0
    # subject-kinds joined the list with Q6: prepare step 1c mints the
    # company identity through it (hard-fail on refusal), so a cold
    # stack must have it bound before prepare starts.
    local svcs="classes subject-kinds jobs policy people accounts inventory messages content calendar products ledger catalog assets"
    declare -A PORT
    while IFS=: read -r name prod _; do [[ -n "$name" ]] && PORT["$name"]="$prod"; done < <(boss-ports-list --paired 2>/dev/null)
    while IFS=: read -r name prod;  do [[ -n "$name" ]] && PORT["$name"]="$prod"; done < <(boss-ports-list --solo 2>/dev/null)
    local deadline=$(( SECONDS + 150 )) svc port code pending
    while :; do
        pending=""
        for svc in $svcs; do
            port="${PORT[$svc]:-}"; [[ -z "$port" ]] && continue
            code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$port/api/$svc/health" 2>/dev/null || echo 000)
            [[ "$code" == "200" ]] || pending="$pending$svc "
        done
        [[ -z "$pending" ]] && { echo "    stack ready — all seed targets healthy"; return 0; }
        (( SECONDS >= deadline )) && { echo "    WARN: stack not fully ready after 150s (pending: $pending) — seeding anyway" >&2; return 0; }
        echo "    waiting for stack (pending: $pending)"; sleep 3
    done
}

if command -v boss-brewery-sim >/dev/null 2>&1; then
    echo "    preparing brewery tenant model (boss-brewery-sim prepare)"
    wait_for_stack
    ok=
    prepare_log="$(mktemp)"
    trap 'rm -f "$prepare_log"' EXIT
    for attempt in 1 2 3; do
        # Capture rather than discard. `prepare >/dev/null 2>&1` threw
        # away the only description of the failure, so a broken reset
        # said nothing more than "prepare failed" and the reason had to
        # be recovered by re-running prepare by hand afterwards.
        if BOSS_SIM_SEEDS_DIR="$SEEDS_DIR" BOSS_EPOCH_START="$EPOCH_START" \
            boss-brewery-sim prepare >"$prepare_log" 2>&1; then
            ok=1; echo "    ✓ brewery tenant prepared"; break
        fi
        echo "    (prepare attempt $attempt failed; retrying)"; sleep 3
    done
    if [[ -z "$ok" ]]; then
        echo "ERROR: brewery prepare failed after 3 attempts — last output:" >&2
        tail -20 "$prepare_log" >&2
        echo "ERROR: aborting BEFORE stamping the reset baseline." >&2
        echo "       A baseline stamped over a failed prepare captures a log with no" >&2
        echo "       published Workflows, and the next restart-epoch trims to it —" >&2
        echo "       destroying the tenant model permanently rather than just idling." >&2
        exit 1
    fi
else
    echo "ERROR: boss-brewery-sim not on PATH — cannot seed the tenant." >&2
    echo "       Refusing to stamp a reset baseline over an unseeded database." >&2
    exit 1
fi

# Stamp the reset baseline: the panel's Reset button (restart-epoch) trims
# the audit_log back to this id, so it must be MAX *after* the tenant seed
# and *before* the sim's first tick — i.e. "seeded day 0". The endpoint
# self-heals if this is unset, but captures too late (after sim activity),
# so set it explicitly now while the log holds only the seed.
#
# Only reachable when prepare succeeded — the branch above exits non-zero
# otherwise. That ordering is the point: the stamp is destructive on the
# NEXT restart, not this one, so a baseline taken over a half-seeded log
# fails silently now and unrecoverably later.
#
# Provenance rides the same UPDATE. The baseline is a build artifact
# the demo then replays forever, so "which revision is this tenant
# pinned to, and how old is that pin?" has to be answerable without
# archaeology on audit_log.created_at — read it back at
# GET /api/clock/baseline.
#
# BOSS_BASELINE_SOURCE_REF overrides for callers that know their
# revision without a working tree (docker images ship no .git). When
# neither that nor git resolves, the column stays NULL and reads as
# "unknown" — deliberately not a guess, and never "current".
source_ref="${BOSS_BASELINE_SOURCE_REF:-}"
if [[ -z "$source_ref" ]] && command -v git >/dev/null 2>&1; then
    source_ref="$(git -C "$(dirname "${BASH_SOURCE[0]}")/.." rev-parse --short HEAD 2>/dev/null || true)"
fi
# Piped, not `-c`: psql only interpolates `:'var'` for input read from
# stdin or a file. Under `-c` the literal `:'ref'` reaches the server
# and the statement dies on a syntax error — which would have taken
# `epoch_baseline_audit_id` down with it, since it is the same UPDATE.
# `:'ref'` also quotes + escapes the value properly, so no hand-rolled
# shell quoting.
if command -v psql >/dev/null 2>&1 && [[ -n "${BOSS_POSTGRES_URL:-}" ]]; then
    if printf '%s\n' \
        "UPDATE sim_clock SET \
            epoch_baseline_audit_id = (SELECT COALESCE(MAX(id),0) FROM audit_log), \
            baseline_cut_at = NOW(), \
            baseline_source_ref = NULLIF(:'ref', '') \
          WHERE id = 1;" \
        | psql "$BOSS_POSTGRES_URL" -v ON_ERROR_STOP=1 -v ref="${source_ref}" \
        >/dev/null 2>&1; then
        echo "    ✓ reset baseline stamped (seeded day 0, source ${source_ref:-unknown})"
    else
        echo "    WARN: reset-baseline stamp failed; restart-epoch will self-heal" >&2
    fi
fi
