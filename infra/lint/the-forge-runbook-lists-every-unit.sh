#!/usr/bin/env bash
# the-forge-runbook-lists-every-unit — the forge's operational topology
# stays written down, not carried in someone's head.
#
# 4d5f158a: "The forge is operated from memory." Its units, containers
# and remediation live in one place — infra/forge/OPERATIONS.md — and
# the failure mode is drift: install.sh grows a unit, OPERATIONS.md does
# not, and the runbook silently stops describing the host. That is the
# manifest.txt lesson (CLAUDE.md §9a) applied to prose: two lists of the
# same fact (install.sh's UNITS array + the runbook's unit table) that
# nothing keeps in step.
#
# This does not add a THIRD copy — it PINS the two that exist. install.sh
# is the authority on what is installed (its UNITS array + the
# boss-ops-runner it installs via a drop-in); the runbook's "The BOSS
# units" table must name every one. A unit installed but undocumented
# fails this loudly, so the runbook cannot fall behind the host.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

INSTALL="infra/forge/install.sh"
RUNBOOK="infra/forge/OPERATIONS.md"
for f in "$INSTALL" "$RUNBOOK"; do
    [ -f "$f" ] || { echo "the-forge-runbook-lists-every-unit: $f not found" >&2; exit 1; }
done

# The installed units: the UNITS=( ... ) array plus boss-ops-runner,
# which install.sh installs separately via its own drop-in.
installed=$(
    { sed -n '/^UNITS=(/,/^)/p' "$INSTALL" | grep -oE '^[[:space:]]+[a-z0-9-]+' | tr -d ' '
      echo boss-ops-runner
    } | sort -u
)

# The documented units: backtick-quoted names in the first column of the
# runbook's unit table (rows like "| `forge-converge` | ...").
documented=$(grep -oE '^\| `[a-z0-9-]+`' "$RUNBOOK" | tr -d '|` ' | sort -u)

missing=$(comm -23 <(printf '%s\n' "$installed") <(printf '%s\n' "$documented"))
if [ -n "$missing" ]; then
    echo "the-forge-runbook-lists-every-unit: installed but NOT in $RUNBOOK:" >&2
    printf '    %s\n' $missing >&2
    echo "    install.sh installs these; the runbook's unit table must name each" >&2
    echo "    (add a row), or the forge is operated from memory again." >&2
    exit 1
fi
count=$(printf '%s\n' "$installed" | grep -c .)
echo "the-forge-runbook-lists-every-unit: ok — all $count installed units are in the runbook"
