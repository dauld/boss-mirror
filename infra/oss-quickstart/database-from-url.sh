#!/bin/sh
# database-from-url — the database name a postgres URL names, for the
# one container that speaks to postgres by PG* variables (boss-init:
# infra/oss-quickstart/init.sh) while every service opens the Secret's
# DATABASE_URL. Until 2026-09-16 boss-init took PGDATABASE from a
# manifest literal beside that URL — two names for one fact — and the
# day they disagree (Option 3, design e652c7c6: the instance's database
# is repointed by switch-instance-database, 063dba4e) the schema would
# converge into one database while the services opened the other.
#
#   database-from-url.sh "$DATABASE_URL"   → prints the name, exit 0
#   an empty path (postgres://u:p@h:5432/)  → nothing, exit 78 (EX_CONFIG)
#   no argument                              → nothing, exit 78
#
# A query string and a trailing slash are stripped. Never defaults to
# the user name: a guessed database is exactly the disagreement this
# exists to end.
set -eu
url="${1:-}"
[ -n "$url" ] || { echo "database-from-url: no URL given" >&2; exit 78; }
db="${url##*/}"
db="${db%%\?*}"
[ -n "$db" ] || { echo "database-from-url: the URL names no database (empty path)" >&2; exit 78; }
printf '%s\n' "$db"
