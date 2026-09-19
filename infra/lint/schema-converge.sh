#!/usr/bin/env bash
#
# schema-converge — every deploy path applies the tree's schema.
#
# THE PRINCIPLE
# -------------
# Code, config and schema converge from the tree on every deploy. Two of
# the three had mechanisms: images roll from forge main, manifests are
# `kubectl apply`-ed from the tree on every converge. Schema did not. The
# cluster's init container applied the manifest only to an EMPTY database
# — "Database already initialized (schema present). Nothing to do." — and
# nothing else in the deploy path touched an existing one.
#
# On 2026-08-13 that let FOUR migrations (112, 113, 114, 116) accumulate
# unapplied while every other layer rolled forward. Train #10 shipped the
# station registry, the image rolled, the code went live, and
# `GET /api/stations` answered 500 `relation "stations" does not exist`
# off a deploy that reported success. It was contained only because the
# missing tables backed new endpoints; an ALTER on a live table, or a
# boot-time schema check, makes the same gap a total outage.
#
# THE CHECKED PROPERTY
# --------------------
# 1. Every deploy entry point below invokes the migration runner.
# 2. In the init container, the converge is not skippable: no `exit`
#    precedes it, so no "already initialized" shortcut can grow back.
#
# This is the CLAUDE.md §9a rule applied to a property rather than a
# constant — it lives in two files that cannot be collapsed into one,
# so it gets a test that names the file when it drifts.
set -euo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

RUNNER="infra/postgres/migrate.sh"
FAIL=0

# Deploy entry points, each with what it deploys. A new one belongs here
# the day it is written — a deploy path that cannot converge the schema
# is the defect this lint exists to catch. ONE since 2026-09-18: the
# conductor's own deploy hop (train.rs pulling a host tree and running
# the bare-metal scripts) left with that path (train #443, backlog
# ed64f852); the container converges on every train, and this is the
# path it converges through.
PATHS=(
    "infra/oss-quickstart/init.sh|cluster initContainer + compose init (boss-init)"
)

for entry in "${PATHS[@]}"; do
    IFS='|' read -r path what <<<"$entry"
    if [ ! -f "$path" ]; then
        echo "schema-converge: $path is in the roster but not in the tree" >&2
        FAIL=1
        continue
    fi
    if ! grep -q "$RUNNER" "$path"; then
        echo "schema-converge: $path ($what) never runs $RUNNER" >&2
        echo "    A deploy that ships code and config but not schema leaves the" >&2
        echo "    database behind the tree, silently, until an endpoint 500s." >&2
        FAIL=1
    fi
done

# The converge in the init container must not sit behind an early exit.
# The 2026-08-13 gap was exactly one `exit 0` above this line.
INIT="infra/oss-quickstart/init.sh"
if [ -f "$INIT" ]; then
    migrate_line=$(grep -n "$RUNNER" "$INIT" | sed -n 1p | cut -d: -f1)
    exit_line=$(grep -n '^[[:space:]]*exit[[:space:]]' "$INIT" | sed -n 1p | cut -d: -f1)
    if [ -n "$migrate_line" ] && [ -n "$exit_line" ] && [ "$exit_line" -lt "$migrate_line" ]; then
        echo "schema-converge: $INIT exits at line $exit_line, before it converges the" >&2
        echo "    schema at line $migrate_line. The converge must be unconditional:" >&2
        echo "    an init that skips a database it finds non-empty is how migrations" >&2
        echo "    accumulate unapplied. Gate the first-start-only steps instead." >&2
        FAIL=1
    fi
fi

if [ "$FAIL" -ne 0 ]; then
    exit 1
fi

lint_scanned schema-converge "${#PATHS[@]}" "deploy path(s)"
echo "schema-converge: ok — ${#PATHS[@]} deploy paths converge the schema from the tree"
