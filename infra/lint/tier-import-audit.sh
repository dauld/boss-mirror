#!/usr/bin/env bash
# Cross-tier import audit.
#
# Rule: Tier-1 (core) library crates must NOT depend on Tier-2
# (modules) or Tier-3 (tenants) crates. Orchestrators (under
# crates/orchestrators/) are explicitly permitted to fan out
# across tiers — that's their purpose (boss-rebuild calls every
# domain rebuilder, boss-cli exposes operator commands across
# domains, etc).
#
# Walks every Cargo.toml under crates/core/ and reports any
# `path = "../../modules/..."` or `path = "../../tenants/..."`
# reference. Exit code 0 = clean, 1 = violations.
#
# Wire into CI by adding to the PR-time check matrix; today this
# script is invoked manually + on-demand.

set -euo pipefail

cd "$(dirname "$0")/../.."

violations=0
for toml in $(find crates/core -name Cargo.toml -type f); do
  src_crate=$(basename "$(dirname "$toml")")
  hits=$(grep -nE 'path\s*=\s*"\.\./\.\./(modules|tenants)/' "$toml" 2>/dev/null || true)
  if [ -n "$hits" ]; then
    echo "VIOLATION: core crate \"$src_crate\" depends on a non-core crate"
    echo "$hits" | sed 's/^/  /'
    violations=$((violations+1))
  fi
done

# --- DB-level core->module FK check ----------------------------------------
# The schema is split into per-module files (infra/postgres/schema/, applied
# in manifest order). A CORE file (00-09) must not hard-FK a table owned by a
# Tier-2 MODULE file (10-40) — the DB analogue of the crate-import rule above.
# B1-B4 of the split removed the historical core->module FKs (scheduling moved
# to Tier-2 + soft-ref'd, webauthn moved into people, ledger out-FKs demoted);
# this check keeps them from creeping back.
SCHEMA_DIR="infra/postgres/schema"
if [ -d "$SCHEMA_DIR" ]; then
  module_tables=$(grep -hoE 'CREATE TABLE (IF NOT EXISTS )?"?[a-zA-Z_][a-zA-Z0-9_]*' "$SCHEMA_DIR"/[1-4][0-9]-*.sql 2>/dev/null \
    | sed -E 's/CREATE TABLE (IF NOT EXISTS )?"?//' | sort -u || true)
  for f in "$SCHEMA_DIR"/0[0-9]-*.sql; do
    [ -e "$f" ] || continue
    refs=$(grep -oE 'REFERENCES +"?[a-zA-Z_][a-zA-Z0-9_]*' "$f" 2>/dev/null \
      | sed -E 's/REFERENCES +"?//' | sort -u || true)
    for r in $refs; do
      if grep -qx "$r" <<< "$module_tables"; then
        echo "VIOLATION: core schema file \"$(basename "$f")\" has FK REFERENCES \"$r\" (a Tier-2 module table)"
        violations=$((violations+1))
      fi
    done
  done
fi

if [ "$violations" -eq 0 ]; then
  echo "tier-import-audit: clean ($(find crates/core -name Cargo.toml | wc -l) core crates; no cross-tier crate imports or core->module schema FKs)"
  exit 0
else
  echo
  echo "tier-import-audit: $violations violation(s)"
  exit 1
fi
