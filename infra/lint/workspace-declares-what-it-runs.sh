#!/usr/bin/env bash
# workspace-declares-what-it-runs.sh — say out loud which tests this
# workspace CANNOT run, so a green local run is never mistaken for a
# green gate.
#
# A REPORT, NEVER A REFUSAL. Developing on a machine without Postgres is
# allowed; what is not allowed is that being invisible. This exits 0 in
# every case (see the bottom) — a rule that has to be right about intent
# is a rule that will be wrong about it, and "you may not work here" is
# exactly that kind of rule.
#
# WHY IT EXISTS, measured 2026-08-29. A car adding one `job_edges` row
# passed 285 local tests on the workstation and then failed the cluster
# gate on `job_edges_pg.rs::registry_seeds_the_four_real_edges` — a
# roster test asserting the exact edge set. It never ran locally:
# `*_pg.rs` tests need Postgres on 127.0.0.1:5432, which this machine
# deliberately does not have (connection-refused is the SAFE state — a
# port-forward there would point the suite at production, which is the
# 2026-08-14 incident). The failure cost eleven minutes and a receipt to
# chase, in a different system, for something a line of output could
# have said before the push.
#
# `dev-node-checkout.md` already states the guarantee this makes legible:
# "the image is the one CI runs, so 'works on the dev box' and 'passes
# the gate' cannot drift." That is a property of a WORKSPACE. Until it is
# declared where work happens, it protects nobody. Design packet
# 775f0b35 (Q3): record the laptop as a workspace like any other, with
# its capabilities declared false, because treating off-pool work as
# unrecorded makes it invisible precisely when it explains a failure.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

PGHOST_="${BOSS_TEST_PG_HOST:-127.0.0.1}"
PGPORT_="${BOSS_TEST_PG_PORT:-5432}"

# Count the coverage that is gated on a database. Both shapes count:
# integration tests under tests/, and `mod db_tests` inside src/.
mapfile -t PG_TESTS < <(grep -rl 'TestDb::new\|BOSS_TEST_POSTGRES' \
    --include='*.rs' crates/*/*/tests/ 2>/dev/null | sort)
mapfile -t PG_MODS < <(grep -rl 'mod db_tests' --include='*.rs' crates 2>/dev/null | sort)
total=$(( ${#PG_TESTS[@]} + ${#PG_MODS[@]} ))

# A TCP connect, not `psql` — the point is whether the socket answers,
# and shelling to a client the workspace may not have would report the
# client's absence as the database's.
reachable=0
if command -v nc >/dev/null 2>&1; then
    nc -z -w 2 "$PGHOST_" "$PGPORT_" >/dev/null 2>&1 && reachable=1
elif command -v timeout >/dev/null 2>&1; then
    timeout 2 bash -c "cat < /dev/null > /dev/tcp/$PGHOST_/$PGPORT_" 2>/dev/null && reachable=1
else
    (bash -c "cat < /dev/null > /dev/tcp/$PGHOST_/$PGPORT_") 2>/dev/null && reachable=1
fi

if [ "$reachable" = 1 ]; then
    echo "workspace-declares-what-it-runs: Postgres answers on $PGHOST_:$PGPORT_ — \
all $total database-backed test targets can run here"
    exit 0
fi

echo "workspace-declares-what-it-runs: NO Postgres on $PGHOST_:$PGPORT_ —"
echo "  $total database-backed test targets CANNOT run in this workspace."
printf '  by crate: '
{
    printf '%s\n' "${PG_TESTS[@]}" | sed 's|crates/[^/]*/||; s|/tests/.*||'
    printf '%s\n' "${PG_MODS[@]}" | sed 's|crates/[^/]*/||; s|/src/.*||'
} | grep -v '^$' | sort | uniq -c | sort -rn | head -6 |
    awk '{printf "%s(%s) ", $2, $1}'
echo
echo
echo "  A GREEN RUN HERE IS NOT A GREEN GATE. Anything touching a seeded"
echo "  registry row, a migration, or a projection is exactly what these"
echo "  cover — and they fail on the runner, minutes later, in another"
echo "  system. Build that work in a checked-out dev workspace, where the"
echo "  image is the one CI runs and Postgres is a sidecar."
echo
echo "  This is a report, not a refusal: gate.sh --quick says it is not a"
echo "  gate, and this says what that costs here."
exit 0
