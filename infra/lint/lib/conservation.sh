#!/usr/bin/env bash
# infra/lint/lib/conservation.sh — the ONE helper both conservation
# sweeps source (consolidation H12, backlog 236529aa, 2026-09-18):
#
#   infra/lint/conservation-invariants.sh          the platform's eight
#   examples/brewery/conservation-invariants.sh    the brewery's fourteen
#
# Each sweep is a list of `run_invariant "<L>. <label>" "<sql>"` calls
# whose SQL selects the rows that VIOLATE the invariant — no rows is a
# pass — and ends with `conservation_verdict <name> <script>`, which
# exits 1 when anything was flagged and 0 with the count otherwise.
#
# THE CONNECTION. DATABASE_URL when set — the in-cluster chore
# (infra/cluster/manifests/boss-conservation-invariants.yaml) hands it
# over from the instance's Secret, and psql accepts a URL as the dbname
# argument — else the PGHOST / PGUSER / PGDATABASE triple, which is the
# spelling infra/postgres/validate-brewery-sim.sh and a shell by hand
# use. Never a default host that is not the system of record: until
# 2026-09-15 the sweep's `PGHOST=127.0.0.1` on boss-gcp was the retired
# second stack's postgres, and the SoR went unswept while the journal
# read clean hourly.
#
# Sourced, not run: the sweeps `set -euo pipefail` themselves.

violations=0

# psql_query <sql> — the rows, on stdout; psql's own stderr folded in so
# a failed query is reported with what psql said.
psql_query() {
    if [ -n "${DATABASE_URL:-}" ]; then
        psql "$DATABASE_URL" -At -c "$1" 2>&1
    else
        psql -h "${PGHOST:-127.0.0.1}" -U "${PGUSER:-boss}" -d "${PGDATABASE:-boss}" -At -c "$1" 2>&1
    fi
}

# run_invariant <label> <sql> — report any rows; count a violation for
# rows OR for a query that failed (a check that cannot run is not a pass).
run_invariant() {
    local label="$1"
    local sql="$2"
    local rows
    rows=$(psql_query "$sql") || {
        echo "[ERROR] $label — query failed:"
        echo "$rows" | sed 's/^/    /'
        violations=$((violations + 1))
        return
    }
    if [[ -n "$rows" ]]; then
        echo "[VIOLATION] $label"
        head -10 <<< "$rows" | sed 's/^/    /'
        local n
        n=$(echo "$rows" | wc -l)
        if (( n > 10 )); then
            echo "    ...and $((n - 10)) more"
        fi
        violations=$((violations + 1))
    fi
}

# conservation_verdict <sweep name> <script path> — exit 1 naming the
# count of violated invariants, or exit 0 naming how many were checked
# (counted off the script's own run_invariant lines, so the number
# cannot drift from the list).
conservation_verdict() {
    local name="$1" script="$2"
    if (( violations > 0 )); then
        echo "$name: $violations invariant(s) violated."
        cat <<'EOF'

The flagged rows violate a conservation property the state model is
supposed to guarantee. Per docs/design/correctness-protocol.md, the
only acceptable cause is a wrong input event — never the projection
pipeline introducing drift. Fix by recording a compensating event
(NOT by editing the projection table directly).

If the violation is a known stop-gap that's been deliberately
accepted, document it in TODO.md with the next milestone that will
clear it.
EOF
        exit 1
    fi
    echo "$name: clean ($(grep -c '^run_invariant "' "$script") invariants checked)."
    exit 0
}
