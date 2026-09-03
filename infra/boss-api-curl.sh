#!/usr/bin/env bash
# boss-api-curl.sh — curl with the deploy-roll posture for cron chores
# (packet 25e518c0).
#
# A TRANSPORT error is a try-again: a routine deploy roll takes the
# jobs API away for ~45s (strategy=Recreate), and every chore that
# treated connection-refused as fatal left an Error pod that looked
# like a defect and masked real ones — audit-integrity, estate-observe,
# search-reindex and views-catchup all died this way on 2026-09-02.
# `boss gate --wait` already holds the right posture ("a deploy rolls
# it briefly"); this is that posture as the ONE helper the chores
# share, so the retry rule and the exit-code list live once.
#
# An HTTP ANSWER is a real result and stays fatal-or-handled exactly
# as the caller wrote it: with -f a 4xx/5xx is curl exit 22 (no
# retry); without -f it is exit 0 and the caller reads the code it
# captured. Only connection-level exits retry:
#   6 DNS, 7 refused, 28 timeout, 35 TLS handshake, 52 empty reply,
#   55 send, 56 recv.
#
# The deadline is bounded WELL UNDER the tightest chore interval
# (estate-observe fires every 15 min) so a retrying run can never
# overlap the next firing: BOSS_API_RETRY_DEADLINE seconds total
# (default 150), backoff 5s doubling. Chatter goes to stderr; stdout
# belongs to curl, so command substitutions capture exactly what they
# always did.
set -euo pipefail

DEADLINE="${BOSS_API_RETRY_DEADLINE:-150}"
start=$(date +%s)
delay=5
while :; do
    rc=0
    curl "$@" || rc=$?
    case "$rc" in
        0) exit 0 ;;
        6 | 7 | 28 | 35 | 52 | 55 | 56) ;;
        *) exit "$rc" ;;
    esac
    now=$(date +%s)
    left=$((DEADLINE - (now - start)))
    if [ "$left" -le 0 ]; then
        echo "boss-api-curl: transport failure persisted past ${DEADLINE}s (curl exit $rc) — not a deploy roll, giving up" >&2
        exit "$rc"
    fi
    sleep_for=$((delay < left ? delay : left))
    echo "boss-api-curl: transport error (curl exit $rc) — retrying in ${sleep_for}s (${left}s of deadline left)" >&2
    sleep "$sleep_for"
    delay=$((delay * 2))
done
