#!/usr/bin/env bash
# Lint against the "snapshot, not projection" anti-pattern that
# repeatedly bit the SPA — hand-maintained arrays of facts that
# silently drift from their real source of truth (boss-ports::lib
# for the service/port map, the brewery seed for employees, etc.).
#
# Checks:
#
#   1. `apps/web/src/_generated/ports.ts` is present and lists the
#      same service names as `boss-ports-list --json`. Catches the
#      case where someone committed without re-running the SPA
#      build, or edited the generated file by hand.
#
#   2. The two prior known-bad SPA files (`dev-server.ts` and
#      `it/monitoring/MonitoringPage.svelte`) don't reintroduce a
#      hand-maintained service list. Both must import from
#      `_generated/ports`; the lint just greps for the import.
#
# Usage: infra/lint/no-snapshot-arrays.sh
#
# Exit codes: 0 clean / 1 drift.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED="$(printf '\033[31m')"
GREEN="$(printf '\033[32m')"
RESET="$(printf '\033[0m')"

failures=0
fail() { printf "  %sFAIL%s %s\n" "$RED" "$RESET" "$1"; failures=$((failures + 1)); }
ok()   { printf "  %sok%s   %s\n" "$GREEN" "$RESET" "$1"; }

# BOTH generated copies. The fact lives three times — boss-ports (the
# Rust source of truth) and one _generated/ports.ts per SPA — and this
# check used to pin only the web copy. On 2026-08-13 the simulator copy
# was found 20 lines short, missing `search`, `views` and `campaigns`
# entirely, while the web copy was correct: the guarded copy stayed in
# sync and the unguarded one silently rotted. A guard that covers one of
# two duplicates is not a guard on the duplication, which is the whole
# point of CLAUDE.md §9a. Collapsing to a single shared module is the
# real fix; pinning both is the holding action until then.
PORTS_TS_FILES=(
    "apps/web/src/_generated/ports.ts"
    "apps/simulator/src/_generated/ports.ts"
)
# Release first, then debug, then PATH — under CARGO_TARGET_DIR when it
# is set, because the gate-runner builds into /gate-target/target and
# the dev pod into /scratch/target, and a lint that only looked in
# ./target could only ever say "not found" on the two machines that
# had just built everything. The debug fallback is what makes this
# runnable straight after `infra/gate.sh`, whose build phase is
# `cargo build --workspace` (debug); that gate runs this as a check
# once the build has produced the binary.
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
PORTS_BIN="${PORTS_BIN:-$TARGET_DIR/release/boss-ports-list}"
[[ -x "$PORTS_BIN" ]] || PORTS_BIN="$TARGET_DIR/debug/boss-ports-list"
[[ -x "$PORTS_BIN" ]] || PORTS_BIN="$(command -v boss-ports-list 2>/dev/null)"

# --- 1. ports.ts matches the Rust source -------------------------------

printf '\n%s\n' "[1/2] _generated/ports.ts ↔ boss-ports::lib"

if [[ -z "$PORTS_BIN" || ! -x "$PORTS_BIN" ]]; then
    fail "boss-ports-list not found (set PORTS_BIN= or build the workspace)"
else
    # Compare the SET of service names. Order + scratch ports are
    # checked in boss-ports-list's own unit tests; this lint just
    # asserts that the two sources agree on which services exist.
    # jq's sort (codepoint order) so the comparison doesn't depend
    # on the caller's locale collation.
    expected_names="$("$PORTS_BIN" --json |
        jq -r '[.[].name] | sort | .[]')"
    for PORTS_TS in "${PORTS_TS_FILES[@]}"; do
        app="${PORTS_TS#apps/}"; app="${app%%/*}"
        if [[ ! -f "$PORTS_TS" ]]; then
            fail "$PORTS_TS missing — run \`bun run build\` in apps/$app/"
            continue
        fi
        # `[[:space:]]`, never `\s`: BSD sed does not understand `\s`, so
        # on macOS the substitution silently no-opped and every name came
        # through as the raw `"name": "accounts"` match. The set compared
        # equal on Linux and diverged on every Mac — which is how this file
        # got recorded as "stale generated ports.ts" when it was in sync all
        # along, and why the check sat outside the gate. Same portability
        # rule no-secrets.sh states for its patterns.
        actual_names="$(grep -oE '"name":[[:space:]]*"[a-z-]+"' "$PORTS_TS" |
            sed -E 's/.*"name":[[:space:]]*"([a-z-]+)".*/\1/' | sort -u)"
        if [[ "$expected_names" == "$actual_names" ]]; then
            ok "$PORTS_TS lists the same $(echo "$expected_names" | wc -l | tr -d '[:space:]') services as boss-ports-list"
        else
            fail "service-name set diverges in $PORTS_TS:"
            diff <(echo "$expected_names") <(echo "$actual_names") |
                sed 's/^/         /'
            echo "       Fix: \`cd apps/$app && bun run build\` regenerates from boss-ports-list."
        fi
    done
fi

# --- 2. every consumer of the service list imports from _generated ---
#
# dev-server.ts is the one consumer left in the SPA. MonitoringPage.svelte
# was the other until #161 (2026-08-31) deleted it, and this list kept
# naming it — so from that day the lint failed on a missing file, and
# nothing noticed because nothing ran it. A consumer named here must
# exist; a consumer that stops existing is removed here, in the same car.

printf '\n%s\n' "[2/2] known service-list consumers import from _generated/ports"

for f in \
    apps/web/src/dev-server.ts
do
    if [[ ! -f "$f" ]]; then
        fail "$f missing (expected to exist)"
        continue
    fi
    if grep -qE "from '[./]*_generated/ports'" "$f"; then
        ok "$f imports from _generated/ports"
    else
        fail "$f does NOT import _generated/ports — likely reintroduced a hand-maintained service list"
    fi
done

# --- summary -----------------------------------------------------------

printf '\n'
if (( failures == 0 )); then
    printf '%sok%s no snapshot-array drift detected\n' "$GREEN" "$RESET"
    exit 0
else
    printf '%sFAIL%s %d snapshot-array drift issue(s) found\n' "$RED" "$RESET" "$failures"
    exit 1
fi
