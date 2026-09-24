#!/usr/bin/env bash
# boss-api-curl.sh — curl with the deploy-roll posture for cron chores
# (packet 25e518c0).
#
# A refused connect is a try-again: a routine deploy roll takes the
# jobs API away for ~45s (strategy=Recreate), and every chore that
# treated connection-refused as fatal left an Error pod that looked
# like a defect and masked real ones — audit-integrity, estate-observe,
# search-reindex and views-catchup all died this way on 2026-09-02.
# This is that posture as the ONE helper the chores share.
#
# WHICH EXITS ARE RE-SENT is not decided here. It is
# curl_through_a_roll, infra/lib/curl-through-a-roll.sh — the one
# definition the door (infra/dev/boss-api) and the pre-flight lints
# already share — which re-sends only curl exits 6 and 7, the two where
# the request never left. Until 2026-09-23 this file carried its own
# list, 6 7 28 35 52 55 56, and 28, 52, 55 and 56 can each follow a
# request the server already received, so a chore's POST or PUT (a
# maintenance packet opened, a step completed) could land twice
# (backlog c51967b5). No chore needs a GET re-sent on those exits
# either: a Recreate roll's dark minute answers refused, which is 7.
# Pinned by crates/core/boss-testing/tests/a_chore_write_is_sent_once.rs.
#
# An HTTP ANSWER is a real result and stays fatal-or-handled exactly
# as the caller wrote it: with -f a 4xx/5xx is curl exit 22 (no
# re-send); without -f it is exit 0 and the caller reads the code it
# captured. stdout carries the final attempt's answer only; waits and
# refusals go to stderr, so command substitutions capture what they
# always did.
#
# The window is BOSS_API_RETRY_DEADLINE seconds (default 150), bounded
# WELL UNDER the tightest chore interval (estate-observe fires every
# 15 min) so a waiting run can never overlap the next firing.
#
# The lib is found beside this file's REAL location, as lib/: in a
# checkout that is infra/lib/; in the boss image the Dockerfile COPYs
# it to /usr/local/bin/lib/, and the playground crawl copies that
# directory with the helper. Without it this refuses rather than send
# without the wait, or with a second rule.
set -euo pipefail

HERE="$(dirname "$(readlink -f "$0")")"
ROLL_LIB="$HERE/lib/curl-through-a-roll.sh"
[ -r "$ROLL_LIB" ] || {
    echo "boss-api-curl: $ROLL_LIB is missing — a copy of this helper without lib/ beside it cannot wait out a rollout, so nothing was sent (backlog c51967b5)" >&2
    exit 2
}
# shellcheck source=lib/curl-through-a-roll.sh
. "$ROLL_LIB"

# The request, as a phrase for the wait's lines: -X's method (GET when
# none is named) and the first http(s) argument.
method=GET url=
prev=
for arg in "$@"; do
    [ "$prev" = "-X" ] && method=$arg
    case "$arg" in http://* | https://*) [ -n "$url" ] || url=$arg ;; esac
    prev=$arg
done

rc=0
curl_through_a_roll "${BOSS_API_RETRY_DEADLINE:-150}" boss-api-curl 'the jobs API' "$method ${url:-(no url)}" "$@" || rc=$?
exit "$rc"
