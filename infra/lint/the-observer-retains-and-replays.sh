#!/usr/bin/env bash
# the-observer-retains-and-replays.sh — the host observer's spool keeps
# a reading the system of record could not take and replays it, oldest
# first, once it can.
#
# A self-test of infra/estate/observe-lib.sh under a stub
# post_observation: three readings taken while the API is down are
# spooled in observed_at order; the next successful run replays them
# oldest first and empties the spool; a replay that fails midway keeps
# the rest; the cap drops the oldest. Runs on every gate so the
# "alarm dies with its patient" class (packet ec50db46) cannot quietly
# return.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="$here/../estate/observe-lib.sh"
[[ -f "$lib" ]] || { echo "the-observer-retains-and-replays: missing $lib" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export SPOOL_DIR="$tmp/spool" SPOOL_MAX=3 JOBS_API="http://stub"
# shellcheck source=/dev/null
. "$lib"

# Stub: the API answers according to $tmp/answer (202 or down) and
# records every body it was handed, in order.
post_observation() {
    printf '%s\n' "$1" >>"$tmp/posted"
    [[ "$(<"$tmp/answer")" == "202" ]]
}
obs() { printf '{"observed_at":"%s","scope":"host","nodes":[{"id":"forge"}]}' "$1"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# Down: three readings, spooled in order.
echo down >"$tmp/answer"
spool_put "$(obs 2026-09-05T08:00:00Z)"
spool_put "$(obs 2026-09-05T08:15:00Z)"
spool_put "$(obs 2026-09-05T08:30:00Z)"
[[ "$(spool_count)" -eq 3 ]] || fail "expected 3 spooled, got $(spool_count)"
# The cap: a fourth drops the oldest.
spool_put "$(obs 2026-09-05T08:45:00Z)" >/dev/null
[[ "$(spool_count)" -eq 3 ]] || fail "the cap did not hold at 3: $(spool_count)"
[[ ! -f "$SPOOL_DIR/2026-09-05T08:00:00Z.json" ]] || fail "the cap dropped the wrong file"

# Still down: a replay attempt posts the oldest, fails, keeps all.
: >"$tmp/posted"
spool_replay >/dev/null && fail "replay reported success while the API was down"
[[ "$(spool_count)" -eq 3 ]] || fail "a failed replay lost readings: $(spool_count) left"
[[ "$(wc -l <"$tmp/posted")" -eq 1 ]] || fail "a failed replay kept posting past the first failure"

# Up: replay drains oldest first and empties the spool.
echo 202 >"$tmp/answer"; : >"$tmp/posted"
spool_replay >/dev/null || fail "replay failed with the API up"
[[ "$(spool_count)" -eq 0 ]] || fail "the spool is not empty after replay: $(spool_count)"
order=$(sed -n 's/.*"observed_at":"\([^"]*\)".*/\1/p' "$tmp/posted" | tr '\n' ' ')
[[ "$order" == "2026-09-05T08:15:00Z 2026-09-05T08:30:00Z 2026-09-05T08:45:00Z " ]] || fail "replay order was not oldest first: $order"
# Replayed readings carry their ORIGINAL observed_at — the gap shows.
grep -q '"observed_at":"2026-09-05T08:15:00Z"' "$tmp/posted" || fail "a replayed reading lost its observed_at"

echo "the-observer-retains-and-replays: self-test ok — 3 readings spooled in observed_at order, the cap dropped the oldest, a failed replay kept the rest, an answered replay drained oldest first with original timestamps"
exit 0
