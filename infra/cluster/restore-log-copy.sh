#!/usr/bin/env bash
# Restore side of migration-as-log-copy — the counterpart of
# export-log-copy.sh, and the executable form of dev-cluster.md's
# restore sequence:
#
#   migrate.sh from empty → restore the copy-set → boss-rebuild-all →
#   boss-audit-integrity-check green → converge the instance onto it
#
# This script owns everything up to (not including) the deploys. It is
# deliberately shape-independent: all it needs is a Postgres it may
# create a database on, the repo checkout it lives in, and the release
# binaries built (`infra/build-release.sh`).
#
# Two details the first rehearsal surfaced, encoded here so migration
# night does not rediscover them:
#
#   1. The migrations SEED part of the copy-set (dispatcher_rules
#      rows, platform classes …). The tarball carries the
#      authoritative copy, so the copy-set tables are TRUNCATEd after
#      migrate and before restore — the manifest counts then must
#      match exactly, seeds included, because the source ran the same
#      migrations.
#   2. credentials.toml is NOT installed by default. A rehearsal must
#      never touch /var/lib/boss/auth; pass --install-credentials on
#      the real move.
#
# The verification is three-fold and all three must pass: the chain
# integrity check (proves the audit_log copy faithful end to end),
# per-table row counts against the tarball's manifest (proves nothing
# was dropped or doubled), and the rebuild completing (proves the
# projections reproduce from the copied log).
#
# Usage:
#   sudo ./infra/cluster/restore-log-copy.sh <tarball> --db <name> [--install-credentials]
#
# The target database must NOT exist — this restores into a fresh
# database only, and refuses to overwrite one it did not create.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RELEASE="$REPO_ROOT/target/release"

TARBALL="${1:?usage: restore-log-copy.sh <tarball> --db <name> [--install-credentials]}"
shift
DB=""
INSTALL_CREDS=false
while [ $# -gt 0 ]; do
    case "$1" in
        --db) shift; DB="${1:?--db needs a name}"; shift ;;
        --install-credentials) INSTALL_CREDS=true; shift ;;
        *) echo "restore-log-copy.sh: unknown arg: $1" >&2; exit 2 ;;
    esac
done
[ -n "$DB" ] || { echo "restore-log-copy.sh: --db is required" >&2; exit 2; }

fail() { echo "FAIL: $*" >&2; exit 1; }

[ -f "$TARBALL" ] || fail "tarball not found: $TARBALL"
[ -x "$RELEASE/boss-rebuild-all" ] || fail "release binaries missing — run infra/build-release.sh first"
[ -x "$RELEASE/boss-audit-integrity-check" ] || fail "boss-audit-integrity-check missing from $RELEASE"
[[ $EUID -eq 0 ]] || fail "run with sudo (creates the database, restores as postgres)"

psql_db() { sudo -u postgres psql -X -q -A -t -v ON_ERROR_STOP=1 -d "$DB" -c "$1"; }

# --- unpack + checksum ------------------------------------------------
WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT
tar -C "$WORKDIR" -xzf "$TARBALL"
SRC=$(find "$WORKDIR" -maxdepth 1 -mindepth 1 -type d | sed -n 1p)
[ -n "$SRC" ] || fail "tarball has no top-level directory"
echo "==> verifying checksums"
(cd "$SRC" && sha256sum -c SHA256SUMS >/dev/null) || fail "checksum mismatch — the tarball is corrupt"

# --- fresh database ---------------------------------------------------
exists=$(sudo -u postgres psql -X -q -A -t -c "SELECT 1 FROM pg_database WHERE datname = '$DB'" || true)
[ "$exists" != "1" ] || fail "database '$DB' already exists — this restores into a fresh database only"
echo "==> creating database $DB"
sudo -u postgres createdb "$DB"

echo "==> applying schema (migrate.sh from empty)"
"$REPO_ROOT/infra/postgres/migrate.sh" -- sudo -u postgres psql -d "$DB" >/dev/null

# --- restore the copy-set --------------------------------------------
# Read the table list from the manifest — the export is the single
# author of what the copy-set contains; a second list here would be a
# §9a drift pair.
TABLES=()
while read -r _ t _; do TABLES+=("$t"); done < <(grep '^rows ' "$SRC/manifest.txt")
[ ${#TABLES[@]} -gt 0 ] || fail "manifest.txt lists no tables"

# Truncate ONLY the tables the migrations seeded (non-empty after
# migrate-from-empty). An empty table needs nothing — and audit_log
# MUST take this path, because its append-only trigger rejects
# TRUNCATE outright (the rehearsal proved it). CASCADE is safe here
# solely because the database is fresh: every referencing table is
# empty.
echo "==> truncating migration-seeded copy-set tables"
for t in "${TABLES[@]}"; do
    n=$(psql_db "SELECT count(*) FROM $t")
    if [ "$n" != "0" ]; then
        echo "    $t: dropping $n seeded rows (the tarball carries the authoritative copy)"
        psql_db "TRUNCATE $t CASCADE" >/dev/null
    fi
done

# Streamed via stdin: root opens the file, so the postgres user never
# needs read access inside root's 0700 tempdir (the rehearsal's second
# find).
echo "==> restoring copy-set data"
sudo -u postgres psql -X -q -v ON_ERROR_STOP=1 -d "$DB" <"$SRC/copy-set.sql" >/dev/null

echo "==> verifying row counts against the manifest"
while read -r _ t want; do
    got=$(psql_db "SELECT count(*) FROM $t")
    [ "$got" = "$want" ] || fail "table $t: restored $got rows, manifest says $want"
done < <(grep '^rows ' "$SRC/manifest.txt")
want_max=$(awk '/^max_audit_id /{print $2}' "$SRC/manifest.txt")
got_max=$(psql_db "SELECT COALESCE(MAX(id),0) FROM audit_log")
[ "$got_max" = "$want_max" ] || fail "audit_log max id: $got_max != manifest $want_max"
echo "    counts exact (audit head id $got_max)"

# --- rebuild + integrity ---------------------------------------------
# sqlx needs a non-empty authority even over a unix socket; the
# `host=` query param wins for the actual connection.
DB_URL="postgresql://postgres@localhost/$DB?host=/var/run/postgresql"
echo "==> rebuilding every projection from the copied log"
sudo -u postgres "$RELEASE/boss-rebuild-all" --database-url "$DB_URL" \
    || fail "rebuild did not complete"

echo "==> chain integrity check (destination)"
sudo -u postgres "$RELEASE/boss-audit-integrity-check" --database-url "$DB_URL" \
    || fail "destination chain is broken — the copy is not faithful; do NOT cut over"

# --- credentials ------------------------------------------------------
if $INSTALL_CREDS; then
    install -D -m 600 "$SRC/credentials.toml" /var/lib/boss/auth/credentials.toml
    echo "==> credentials.toml installed"
else
    echo "==> credentials.toml NOT installed (rehearsal mode; pass --install-credentials on the real move)"
fi

echo "==> restore complete: $DB carries the log, the registries, and rebuilt projections"
echo "    next on the real move: point the instance's services at $DB and converge"
