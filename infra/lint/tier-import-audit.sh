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
# Walks every Cargo.toml under the core tier's root and reports any
# `path = "../../<a modules or tenants directory>/..."` reference.
# Exit code 0 = clean, 1 = violations.
#
# WHERE THE ROOTS COME FROM (design 01c3cc3f, 2026-09-19). Until then
# this script carried the core root and the forbidden directories as
# its own text — the only place in the tree that could answer "which
# tier is this path in", and about to be joined by two more copies
# (the arrival classifier, the hosting edit level). The map is now
# infra/platform/tiers.toml, read here through lib/tiers.sh: the core
# root is the `core` tier's prefixes, and the forbidden edges are the
# `modules` and `tenants` tiers' directories under the crates root,
# relative to a core crate. A collapse, not a pin — this file holds no
# prefix of its own, and boss-testing/tests/tiers_sh.rs refuses one.
#
# Wire into CI by adding to the PR-time check matrix; today this
# script is invoked manually + on-demand.

set -euo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3
# shellcheck source=infra/lint/lib/tiers.sh
. infra/lint/lib/tiers.sh || exit 3

# The core tier's roots, from the map; a map that names no core tier
# cannot be audited against, and says so (exit 3: the machine could
# not answer, lib/git-answer.sh's meaning).
core_roots=$(tier_paths core) || { echo "tier-import-audit: the tier map names no core tier" >&2; exit 3; }
crates_root=${core_roots%%/*}
# The directories a core crate must not reach by `../../<dir>/`: every
# forbidden tier's prefix under the same crates root, relative to it,
# joined into one alternation for the grep below.
forbidden=
for tier in modules tenants; do
  for prefix in $(tier_paths "$tier"); do
    case "$prefix" in
      "$crates_root"/*) dir=${prefix#*/}; forbidden="${forbidden:+$forbidden|}${dir%/}" ;;
    esac
  done
done
[ -n "$forbidden" ] || { echo "tier-import-audit: the tier map names no forbidden tier under $crates_root/" >&2; exit 3; }
crate_tomls() { find $core_roots -name Cargo.toml -type f; }

violations=0
for toml in $(crate_tomls); do
  src_crate=$(basename "$(dirname "$toml")")
  hits=$(grep -nE "path\\s*=\\s*\"\\.\\./\\.\\./($forbidden)/" "$toml" 2>/dev/null || true)
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
  lint_scanned tier-import-audit "$(crate_tomls | wc -l | tr -d ' ')" "core crate(s)"
  echo "tier-import-audit: clean ($(crate_tomls | wc -l | tr -d ' ') core crates; no cross-tier crate imports or core->module schema FKs)"
  exit 0
else
  echo
  echo "tier-import-audit: $violations violation(s)"
  exit 1
fi
