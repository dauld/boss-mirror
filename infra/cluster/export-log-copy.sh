#!/usr/bin/env bash
# The migration IS a copy of the log — the executable form of
# docs/design/dev-cluster.md §"The migration is a copy of the log".
#
# Produces a restore-ready tarball of everything that does NOT
# regenerate from `git clone` + `migrate.sh` + `boss-rebuild-all`:
#
#   - audit_log            — the system of record; self-verifying (the
#                            hash chain travels with the rows, so the
#                            integrity check green on the DESTINATION
#                            proves the copy faithful end to end)
#   - workflows, step_plugins, classes, policy_rules,
#     policy_rule_audit, dispatcher_rules
#                          — authored registry tables with no rebuilder
#                            (workflow publishes land in the log but
#                            nothing consumes them; classes writes are
#                            eventless today)
#   - design_recorded_decisions
#                          — non-event-sourced by design, and the only
#                            thing keeping an answered design-doc
#                            question from re-opening (boss-docs reads
#                            it on every reindex). Was two tables,
#                            `design_pending_decisions` and
#                            `design_flush_jobs`, until the flush
#                            pipeline was deleted on 2026-09-10
#                            (f5da586c): the second is gone and the
#                            first was renamed to what it now is.
#   - sim_clock            — the epoch baseline row; its audit-id
#                            references copy verbatim with audit_log
#   - messages_events      — the messages retention log (history beyond
#                            the rebuilt projection; the operator inbox
#                            is real operational record now)
#   - /var/lib/boss/auth/credentials.toml
#                          — the one file outside both git and Postgres
#
# Seed-only tables (job_edges, the migration-shipped dispatcher rule
# rows, …) are deliberately absent: migrate.sh recreates them from the
# schema files on the destination.
#
# Procedure (the doc's, verbatim): quiesce writers, drain the outbox
# to verified-empty (outbox rows are pre-log — dumping around a
# non-empty outbox loses staged events), chain-check the source, dump,
# restart writers. Restore on the destination is:
#   git clone && migrate.sh from empty
#   psql < copy-set dumps
#   boss-rebuild-all && boss-audit-integrity-check   # must be green
#   deploy-services + deploy-web
#
# Usage:
#   sudo ./infra/cluster/export-log-copy.sh --check   # preconditions only
#   sudo ./infra/cluster/export-log-copy.sh           # full export

set -euo pipefail

DB="${BOSS_DB:-boss}"
OUT_DIR="${BOSS_EXPORT_DIR:-/var/backups/boss}"
CREDENTIALS="/var/lib/boss/auth/credentials.toml"
TIMESTAMP=$(date -u +%Y%m%d-%H%M%S)

# Writers to quiesce: the sim is the load generator, the dispatcher is
# the side-effect generator. Everything else only writes when a human
# (or the two of them) acts — after these two stop, the outbox drains
# to empty within relay lag (~200ms/poll). boss-event-relay itself
# stays UP: it is the drainer.
WRITERS=(boss-brewery-sim boss-dispatcher)

COPY_TABLES=(
    audit_log
    workflows
    step_plugins
    classes
    policy_rules
    policy_rule_audit
    dispatcher_rules
    design_recorded_decisions
    sim_clock
    messages_events
)

psql_boss() { sudo -u postgres psql -d "$DB" -tAc "$1"; }

fail() { echo "FAIL: $*" >&2; exit 1; }

# --- preconditions (both modes) --------------------------------------
command -v pg_dump >/dev/null || fail "pg_dump not on PATH"
[[ -f "$CREDENTIALS" ]] || fail "$CREDENTIALS missing"
[[ $EUID -eq 0 ]] || fail "run with sudo (stops services, reads $CREDENTIALS)"

for t in "${COPY_TABLES[@]}"; do
    exists=$(psql_boss "SELECT to_regclass('public.$t') IS NOT NULL")
    [[ "$exists" == "t" ]] || fail "copy-set table '$t' does not exist — the script drifted from the schema"
done

echo "==> chain integrity check (source)"
/usr/local/bin/boss-audit-integrity-check --config /etc/boss-audit-integrity-check.toml \
    || fail "source audit_log chain is broken — do NOT migrate; investigate first"

if [[ "${1:-}" == "--check" ]]; then
    outbox=$(psql_boss "SELECT count(*) FROM event_outbox WHERE delivered_at IS NULL")
    echo "==> check mode: copy-set present, chain green, outbox pending $outbox (drains at export time)"
    echo "OK"
    exit 0
fi

# --- quiesce ----------------------------------------------------------
echo "==> quiescing writers: ${WRITERS[*]}"
for w in "${WRITERS[@]}"; do systemctl stop "$w"; done
restart_writers() {
    echo "==> restarting writers"
    for w in "${WRITERS[@]}"; do systemctl start "$w" || echo "WARN: $w failed to start — check it" >&2; done
}
trap restart_writers EXIT

# Drain: PENDING (undelivered) outbox rows must read zero on TWO
# consecutive polls one second apart (a single zero can race a
# straggling in-flight write). Delivered rows are retained in the
# table — counting them would never reach zero; `delivered_at IS
# NULL` is the pending predicate the relay itself indexes on.
echo "==> draining outbox"
zeros=0
for _ in $(seq 1 60); do
    n=$(psql_boss "SELECT count(*) FROM event_outbox WHERE delivered_at IS NULL")
    if [[ "$n" == "0" ]]; then
        zeros=$((zeros + 1))
        [[ $zeros -ge 2 ]] && break
    else
        zeros=0
        echo "    outbox depth $n …"
    fi
    sleep 1
done
[[ $zeros -ge 2 ]] || fail "outbox did not drain within 60s — is boss-event-relay running?"
echo "    outbox empty (verified twice)"

# --- dump -------------------------------------------------------------
WORKDIR=$(mktemp -d)
DEST="$WORKDIR/boss-log-copy-$TIMESTAMP"
mkdir -p "$DEST" "$OUT_DIR"

echo "==> dumping ${#COPY_TABLES[@]} tables"
TABLE_ARGS=()
for t in "${COPY_TABLES[@]}"; do TABLE_ARGS+=(--table "public.$t"); done
# --disable-triggers: `classes` self-references (parent codes), so a
# data-only restore needs FK triggers off while COPY runs. Caught by
# pg_dump's circular-FK warning on the first live run. Means the
# restore runs as a superuser (it does: postgres).
sudo -u postgres pg_dump -d "$DB" --data-only --no-owner --disable-triggers "${TABLE_ARGS[@]}" \
    > "$DEST/copy-set.sql"

echo "==> row counts + checksums → manifest"
{
    echo "# boss log-copy $TIMESTAMP (UTC) — restore verifies against this"
    echo "# after restore: boss-rebuild-all && boss-audit-integrity-check must be green"
    for t in "${COPY_TABLES[@]}"; do
        echo "rows $t $(psql_boss "SELECT count(*) FROM $t")"
    done
    echo "max_audit_id $(psql_boss "SELECT COALESCE(MAX(id),0) FROM audit_log")"
} > "$DEST/manifest.txt"

cp "$CREDENTIALS" "$DEST/credentials.toml"
chmod 600 "$DEST/credentials.toml"

(cd "$DEST" && sha256sum copy-set.sql credentials.toml manifest.txt > SHA256SUMS)

TARBALL="$OUT_DIR/boss-log-copy-$TIMESTAMP.tar.gz"
tar -C "$WORKDIR" -czf "$TARBALL" "boss-log-copy-$TIMESTAMP"
chmod 600 "$TARBALL"
rm -rf "$WORKDIR"

# trap restarts the writers on exit
echo "==> export complete: $TARBALL"
sed 's/^/    /' <(tar -tzf "$TARBALL")
