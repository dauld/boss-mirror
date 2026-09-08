#!/bin/sh
# observe-lib — post an observation, or keep it until the system of
# record can take it. Sourced by observe-host.sh; exercised under stubs
# by infra/lint/the-observer-retains-and-replays.sh on every gate.
#
# WHY. The estate observers report THROUGH the jobs API, so when the
# system of record is down they are silent about an outage — and the
# series simply has a hole nobody can tell from "the timer did not
# fire" (CLAUDE.md §Diagnosis: an alarm that reports through its
# subject dies with it; packet ec50db46). Retaining what was observed
# and replaying it once the API answers makes the gap VISIBLE
# afterwards: the replayed readings carry their original observed_at,
# so the series shows the outage as readings that arrived late, not as
# readings that never were. The operator channel for the outage
# itself is a separate decision; this half needs none.
#
# Spool: one file per observation, named by observed_at, under
# SPOOL_DIR (default /var/tmp/boss-estate-spool — persists across
# reboots, writable by any unit user). Replay is oldest first and stops
# at the first failure, keeping the rest. The spool is capped: past
# SPOOL_MAX files (default 200 — fifty hours at the 15-minute cadence)
# the oldest is dropped, so a long outage cannot fill a disk that is
# probably the thing being observed.
#
# POSIX sh + curl only (directive 26d61c97: no python on hosts); the
# one field this file reads out of a reading, observed_at, is read
# with sed so the spool needs nothing the observer does not.

SPOOL_DIR="${SPOOL_DIR:-/var/tmp/boss-estate-spool}"
SPOOL_MAX="${SPOOL_MAX:-200}"

# post_observation JSON — POST to $JOBS_API/api/estate/observation.
# Prints "jobs api: <code> <body>" on an answer. Returns 0 only on 202.
post_observation() {
    resp=$(printf '%s' "$1" | curl -s -w '\n%{http_code}' \
      -X POST -H 'content-type: application/json' \
      -H 'x-boss-user: {"id":"automation:estate-observer-host","role":"platform-admin","access_tier":"operator"}' \
      --data-binary @- \
      "$JOBS_API/api/estate/observation") \
      || { rc=$?; echo "jobs api: curl failed (exit $rc, target $JOBS_API)"; return 1; }
    code=${resp##*
}
    body=${resp%
*}
    echo "jobs api: $code $body"
    test "$code" = "202"
}

# spool_count — how many observations are waiting.
spool_count() {
    [ -d "$SPOOL_DIR" ] || { echo 0; return; }
    ls -1 "$SPOOL_DIR" 2>/dev/null | grep -c '\.json$'
}

# spool_put JSON — keep an observation for later, oldest dropped past
# the cap. The file name is the observation's own observed_at, so the
# spool sorts into the order the readings were taken.
spool_put() {
    mkdir -p "$SPOOL_DIR"
    at=$(printf '%s' "$1" | sed -n 's/.*"observed_at":"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$at" ] || at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    printf '%s' "$1" > "$SPOOL_DIR/$at.json"
    while [ "$(spool_count)" -gt "$SPOOL_MAX" ]; do
        oldest=$(ls -1 "$SPOOL_DIR" | grep '\.json$' | sort | head -n 1)
        rm -f "$SPOOL_DIR/$oldest"
        echo "spool: over $SPOOL_MAX waiting — dropped the oldest ($oldest)"
    done
}

# spool_replay — post every waiting observation, oldest first; a
# posted one is deleted, and the first failure stops the replay with
# the rest kept. Prints what happened either way. Returns 0 when the
# spool is empty afterwards.
spool_replay() {
    n=$(spool_count)
    [ "$n" -gt 0 ] || return 0
    echo "spool: replaying $n retained observation(s), oldest first"
    for f in $(ls -1 "$SPOOL_DIR" | grep '\.json$' | sort); do
        if post_observation "$(cat "$SPOOL_DIR/$f")"; then
            rm -f "$SPOOL_DIR/$f"
        else
            echo "spool: replay stopped at $f — $(spool_count) still waiting"
            return 1
        fi
    done
    echo "spool: replayed, nothing waiting"
    return 0
}
