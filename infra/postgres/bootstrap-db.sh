#!/usr/bin/env bash
# Create (or re-create) a Boss Postgres database from scratch.
#
# Does the whole init flow in one pass:
#   1. Drop + create the database
#   2. Apply the per-module schema
#   3. Grant table/sequence/function privileges to the boss role
#   4. Seed the Workflow registry (24 system-owned kinds)
#
# Without step 4 every `POST /api/jobs` fails with
# "unknown or inactive job kind" and the whole app looks broken.
# Folding the seed into the bootstrap is the single-step "get me
# a working Boss database" path that the sim, replay, and
# Playwright harnesses all depend on.
#
# Usage:
#   sudo ./infra/postgres/bootstrap-db.sh DB_NAME          # idempotent drop + recreate
#   sudo ./infra/postgres/bootstrap-db.sh DB_NAME --init   # no-op if DB already exists
#   sudo ./infra/postgres/bootstrap-db.sh DB_NAME --no-seed  # skip the Workflow seed

set -euo pipefail

MIGRATE="$(dirname "$0")/migrate.sh"
WITHOUT_ARGS=""
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DB_USER="boss"
DB_NAME=""
INIT_ONLY=false
SEED=true
AUDIT_LOG_SEED=""
RUN_REBUILD=false

# Resolve a Boss binary: PATH first, then $REPO_ROOT/target/release/.
# Lets `cargo build --release --workspace` produce a working bare-
# metal install without requiring a separate `cargo install` step.
# Echoes the resolved path on stdout, or returns 1 if missing.
find_boss_bin() {
    local name="$1"
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return 0
    fi
    local fallback="$REPO_ROOT/target/release/$name"
    if [ -x "$fallback" ]; then
        echo "$fallback"
        return 0
    fi
    return 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --init)    INIT_ONLY=true; shift ;;
        --no-seed) SEED=false; shift ;;
        --rebuild) RUN_REBUILD=true; shift ;;
        --audit-log-seed)
            shift
            AUDIT_LOG_SEED="$1"
            RUN_REBUILD=true
            shift
            ;;
        --without)
            shift
            WITHOUT_ARGS="$WITHOUT_ARGS --without $1"
            shift
            ;;
        --*)       echo "Unknown flag: $1" >&2; exit 2 ;;
        *)
            if [ -z "$DB_NAME" ]; then
                DB_NAME="$1"
            else
                echo "Unexpected positional argument: $1" >&2
                exit 2
            fi
            shift
            ;;
    esac
done

if [ -z "$DB_NAME" ]; then
    echo "Usage: $0 DB_NAME [--init] [--no-seed] [--rebuild] [--audit-log-seed PATH]" >&2
    echo "  --rebuild              run boss-rebuild-all after schema applies" >&2
    echo "  --audit-log-seed PATH  load audit_log dump (.sql or .sql.gz) before rebuild" >&2
    exit 2
fi

if [ ! -x "$MIGRATE" ]; then
    echo "migrate.sh not found/executable at $MIGRATE" >&2
    exit 1
fi

echo "==> bootstrapping $DB_NAME"

exists=$(sudo -n -u postgres psql -tAc \
    "SELECT 1 FROM pg_database WHERE datname='$DB_NAME'" 2>/dev/null || echo "")

if [ -n "$exists" ]; then
    if $INIT_ONLY; then
        echo "  database already exists — --init mode exits without touching it"
        exit 0
    fi
    echo "  dropping existing $DB_NAME (kicks any active connections first)"
    sudo -n -u postgres psql -d postgres -c \
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname='$DB_NAME' AND pid <> pg_backend_pid()" > /dev/null
    # WITH (FORCE) atomically terminates any connection that reconnects between
    # the pg_terminate_backend above and the drop (e.g. clock-api's 2s sim_clock
    # refresher) — without it, that reconnect race intermittently fails the drop.
    sudo -n -u postgres psql -d postgres -c "DROP DATABASE $DB_NAME WITH (FORCE)" > /dev/null
fi

echo "  creating $DB_NAME"
sudo -n -u postgres psql -d postgres -c "CREATE DATABASE $DB_NAME" > /dev/null

echo "  applying per-module schema (migrate.sh, manifest order)"
# migrate.sh applies the manifest entries (honoring any --without) and
# records them in schema_migrations. It streams each file to psql over
# stdin rather than `-f`, so the postgres OS user never needs to traverse
# the calling user's home dir — on GCP / OS Login the home is mode 0750,
# which makes `-f` fail with `Permission denied` when the path crosses it
# (the dominant case for evaluators cloning into ~/boss).
"$MIGRATE" $WITHOUT_ARGS -- sudo -n -u postgres psql -d "$DB_NAME" > /dev/null

echo "  granting table/sequence/function privileges to $DB_USER"
sudo -n -u postgres psql -d "$DB_NAME" <<SQL > /dev/null
GRANT ALL ON ALL TABLES IN SCHEMA public TO $DB_USER;
GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO $DB_USER;
GRANT ALL ON ALL FUNCTIONS IN SCHEMA public TO $DB_USER;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO $DB_USER;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO $DB_USER;
SQL

if $SEED; then
    # Platform-shipped Workflows (`platform_kinds()` — currently just
    # `workflow-design`) auto-reconcile on every `boss-jobs-api` start
    # via `WorkflowRegistry::bootstrap_reconcile`. Tenant-shipped
    # Workflows + policy + data load from each tenant's `seeds/` via the
    # per-tenant converged prepare step (for the brewery,
    # `boss-brewery-sim prepare`). The generic `boss-jobs-seed-kinds`
    # bin retired 2026-05-03 (step 9a of the HumanWorker generator
    # retirement; see docs/design/platform-vs-tenant-jobkinds.md).
    # Nothing to do here.
    echo "  Workflow registry seeding handled by boss-jobs-api startup +"
    echo "  the per-tenant prepare step (brewery: boss-brewery-sim prepare,"
    echo "  which also seeds tenant policy rules + accounts/vendors/data)"
fi

BUILD_URL="postgres://$DB_USER:$DB_USER@127.0.0.1/$DB_NAME"

# Operator-baseline hires must land in audit_log BEFORE the
# rebuild runs, so the people rebuilder's TRUNCATE+replay path
# projects them like any other employee. Idempotent — second run
# skips operators already hired. See
# `examples/brewery/seeds/operator_hires.toml` for the source of
# truth and `crates/modules/boss-people/src/bin/boss_operator_baseline_seed.rs`
# for the loader.
# Platform Workflow rows that live as DATA rather than as Rust
# literals (docs/design/protocols-as-data.md). INSERT-ONLY: a kind
# already present is left exactly as it is, including an operator's
# edits to it. Deliberately NOT bootstrap_reconcile, which republishes
# the code default over a drifted row and is how two protocol edits
# were silently undone on 2026-08-14 (68331085).
#
# Runs before the reconcile in boss-jobs-api's boot: a kind supplied
# here is absent from platform_workflows(), so reconcile never
# considers it.
if PLATFORM_WF_BIN="$(find_boss_bin boss-platform-workflow-seed)"; then
    PLATFORM_WF_TOML="$(dirname "$0")/../platform/workflows.toml"
    if [ -f "$PLATFORM_WF_TOML" ]; then
        echo "  seeding platform Workflow bundle"
        # Filter the happy path's noise but NEVER the failure: the old
        # `| grep ... || true` swallowed a failing seed's error text AND
        # its exit code — the third silence in this one code path (after
        # the missing file and the missing binary), each found the hard
        # way. On failure everything the seed said is shown.
        if SEED_OUT="$("$PLATFORM_WF_BIN" \
            --database-url "$BUILD_URL" \
            --seed-path "$PLATFORM_WF_TOML" 2>&1)"; then
            printf '%s\n' "$SEED_OUT" | grep -E "inserted|already present|seed:" || true
        else
            echo "  WARNING: platform Workflow bundle seed FAILED — full output follows" >&2
            printf '%s\n' "$SEED_OUT" >&2
        fi
    else
        # LOUD, deliberately. This exact silence cost two days: the
        # image lacked infra/platform/, the [ -f ] guard skipped the
        # seed without a word on every cluster boot, ten bundle kinds
        # never reached the registry, and every timer unit posting one
        # of them died on a 400 nobody saw (2026-08-19). A missing
        # bundle is a packaging fault and must read like one.
        echo "  WARNING: platform Workflow bundle NOT FOUND at $PLATFORM_WF_TOML — the seed" >&2
        echo "  is being SKIPPED; bundle-supplied kinds will be missing from this deployment" >&2
    fi
else
    # The OTHER half of the same invariant, which the warning above
    # missed: the 2026-08-19 fix made a missing BUNDLE loud but left a
    # missing BINARY silent, and that is exactly how every fresh
    # install shipped broken — the seed bin is feature-gated and the
    # image's workspace build skipped it, so this outer guard fell
    # through without a word (packet c93e115a, caught by the public
    # mirror's install smoke). A boot-read binary is packaging, and
    # absent packaging must read like a fault, not like silence.
    echo "  WARNING: boss-platform-workflow-seed binary NOT FOUND on PATH or in" >&2
    echo "  target/release — the platform Workflow seed is being SKIPPED;" >&2
    echo "  bundle-supplied kinds will be missing from this deployment" >&2
fi

# THE THREE SILENCES, CLOSED BY ASKING ABOUT THE OUTCOME INSTEAD.
#
# Above there are three ways the platform bundle can fail to arrive —
# missing binary, missing file, failing seed — and each was found the
# hard way and made loud in turn. All three still only WARN, which was
# survivable while the bundle carried three idle kinds.
#
# It no longer is. `ship-a-change` is in the bundle now, so a skipped
# seed is a deployment where NO CHANGE CAN SHIP, and the symptom is a
# 400 on the first car rather than anything resembling a packaging
# fault — precisely the 2026-08-19 shape, where ten bundle kinds were
# missing and every timer posting one died on a 400 nobody saw.
#
# So this asks the only question that matters, once, in terms of the
# outcome rather than the mechanism: is the delivery protocol actually
# in the registry? It catches all three failure paths and any future
# fourth, and it fails the bootstrap rather than warning into a log
# nobody reads.
REQUIRED_KINDS="ship-a-change"
for kind in $REQUIRED_KINDS; do
    present=$(psql "$BUILD_URL" -tAc \
        "SELECT COUNT(*) FROM workflows WHERE kind = '$kind' AND status = 'active'" \
        2>/dev/null || echo 0)
    if [ "${present:-0}" -lt 1 ]; then
        echo "  FATAL: workflow kind '$kind' is not active in the registry after seeding." >&2
        echo "  It lives in infra/platform/workflows.toml, so one of these is true:" >&2
        echo "    - boss-platform-workflow-seed was not found (see the warning above)" >&2
        echo "    - infra/platform/ is missing from the image" >&2
        echo "    - the seed ran and failed" >&2
        echo "  A deployment without this kind cannot ship a change, including the fix." >&2
        exit 1
    fi
    echo "  verified: workflow kind '$kind' is active"
done

if OPERATOR_SEED_BIN="$(find_boss_bin boss-operator-baseline-seed)"; then
    OPERATOR_HIRES_TOML="$(dirname "$0")/../operator-baseline/operator_hires.toml"
    if [ -f "$OPERATOR_HIRES_TOML" ]; then
        echo "  seeding operator-baseline hires into audit_log"
        "$OPERATOR_SEED_BIN" \
            --database-url "$BUILD_URL" \
            --seed-path "$OPERATOR_HIRES_TOML" 2>&1 | grep -E "operator hired|seed complete|skipping" || true

        # Project the 6 baseline employees into the `employees`
        # table so downstream FKs (account_team_members.employee_id,
        # accounts.territory_rep_id mirror, etc.) resolve. The seed
        # binary writes only to audit_log per the canonical pattern;
        # this --only people rebuild reproduces the projection from
        # those events. Cheap (~6 events) so it's unconditional —
        # not gated on --rebuild like the full rebuild path below.
        if REBUILD_BIN="$(find_boss_bin boss-rebuild-all)"; then
            echo "  projecting operator-baseline employees via --only people"
            "$REBUILD_BIN" \
                --database-url "$BUILD_URL" \
                --only people 2>&1 | tail -3 || true
        else
            echo "  WARN: boss-rebuild-all not found — operator employees won't appear in projection" >&2
        fi
    else
        echo "  WARN: operator_hires.toml not found at $OPERATOR_HIRES_TOML" >&2
    fi
else
    echo "  WARN: boss-operator-baseline-seed not built — operator logins may not resolve" >&2
    echo "        Run: cargo build --release --workspace --features postgres" >&2
fi

if $RUN_REBUILD; then
    if ! REBUILD_BIN="$(find_boss_bin boss-rebuild-all)"; then
        echo "  ERROR: --rebuild requested but boss-rebuild-all is not built" >&2
        echo "         Run: cargo build --release --workspace --features postgres" >&2
        exit 1
    fi
    if [ -n "$AUDIT_LOG_SEED" ]; then
        echo "  loading audit_log seed from $AUDIT_LOG_SEED + rebuilding all projections"
        "$REBUILD_BIN" --database-url "$BUILD_URL" --audit-log-seed "$AUDIT_LOG_SEED"
    else
        echo "  rebuilding all projections from existing audit_log"
        "$REBUILD_BIN" --database-url "$BUILD_URL"
    fi

    # boss-rebuild-all projects audit_log -> per-service tables + ledger
    # facts, but the journal-entry projection (financial_facts ->
    # gl_journal_entries) is a separate function exposed via the boss
    # CLI's `ledger rebuild` subcommand. Without this step the GL stays
    # empty after a fresh bootstrap and the trial balance shows zero
    # activity even though the audit_log has every event.
    if BOSS_BIN="$(find_boss_bin boss)"; then
        echo "  rebuilding gl_journal_entries from financial_facts"
        "$BOSS_BIN" ledger rebuild --postgres-url "$BUILD_URL" 2>&1 | tail -3 ||             echo "    (ledger rebuild failed — trial balance may stay empty until manually rebuilt)" >&2
    else
        echo "  WARN: boss CLI not found — skipping ledger rebuild; trial balance will be empty" >&2
    fi

    # Post-rebuild integrity check. Verifies the audit_log hash chain
    # plus dangling-FK rules so a silent drop during seed-load or
    # rebuild surfaces here, not weeks later. Informational only —
    # the chain-hash race documented in validate-brewery-sim.sh
    # can produce anomalies that don't actually break replay (the
    # rebuilders walk by id and reconstruct cleanly), so we report
    # but don't exit on a non-zero code.
    if command -v boss-audit-integrity-check >/dev/null 2>&1; then
        echo "  running post-rebuild audit_log integrity check"
        boss-audit-integrity-check --database-url "$BUILD_URL" || \
            echo "    (integrity check reported anomalies — informational only)"
    else
        echo "  WARN: boss-audit-integrity-check not on PATH — skipping post-rebuild check" >&2
    fi
fi

table_count=$(sudo -n -u postgres psql -d "$DB_NAME" -tAc \
    "SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")
kind_count=$(sudo -n -u postgres psql -d "$DB_NAME" -tAc \
    "SELECT COUNT(*) FROM workflows" 2>/dev/null || echo "?")

echo "done. $DB_NAME has $table_count tables, $kind_count seeded Workflows, and $DB_USER has full privileges."
