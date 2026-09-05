#!/usr/bin/env bash
# alert-lib — an alert is a PACKET in David's queue (David, 2026-09-05:
# "Let's use the ops-request packets for alerts for now"). Sourced by
# cluster-watchdog.sh; tested by infra/lint/alerts-are-packets.sh.
#
# WHY A SPOOL. The alert that matters most is "the system of record is
# dark", and a packet can only be filed through the system of record.
# So an alert that cannot be filed is kept (one file per alert under
# ALERT_SPOOL) and filed by the next run that reaches the API, with its
# original `raised_at` in the body — the same retain-and-replay shape as
# the estate observer's readings. The forge journal carries every alert
# the moment it is raised, whatever the API is doing.
#
# Shape: an urgent backlog-item owned by emp-david, titled from the
# alert, with the facts in metadata. Not an ops-request: that kind asks
# a host to run a verb; this one asks a person to look.

ALERT_SPOOL="${ALERT_SPOOL:-/var/tmp/boss-alert-spool}"
ALERT_API="${JOBS_API:-http://10.20.0.34:7900}"
ALERT_USER='{"id":"automation:cluster-watchdog","role":"platform-admin","access_tier":"operator"}'

# alert_body TITLE DETAIL — the packet JSON, raised_at now.
alert_body() {
    local title="$1" detail="$2" at
    at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    python_free_json "$title" "$detail" "$at"
}
# JSON without jq or python (the forge units must not need either):
# escape backslashes, quotes and newlines by hand.
_esc() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr '\n' ' '; }
python_free_json() {
    local title detail at
    title=$(_esc "$1"); detail=$(_esc "$2"); at="$3"
    printf '{"kind":"backlog-item","status":"open","owner_id":"emp-david","priority":"urgent","tags":["alert","cluster"],"subject":{"subject_kind":"custom","id":"boss-cluster"},"title":"%s","metadata":{"area":"alert","filed_by":"automation:cluster-watchdog","raised_at":"%s","detail":"%s"}}' "$title" "$at" "$detail"
}

# alert_post JSON — POST one alert packet. Returns 0 on 201.
alert_post() {
    local code
    code=$(printf '%s' "$1" | curl -s -o /dev/null -w '%{http_code}' --max-time 15 \
        -X POST -H 'content-type: application/json' -H "x-boss-user: $ALERT_USER" \
        --data-binary @- "$ALERT_API/api/jobs") || return 1
    [ "$code" = "201" ]
}

# alert_count — alerts waiting to be filed.
alert_count() { [ -d "$ALERT_SPOOL" ] || { echo 0; return; }; ls -1 "$ALERT_SPOOL" 2>/dev/null | grep -c '\.json$'; }

# alert TITLE DETAIL — raise an alert: journal it, file it, or keep it.
alert() {
    local body; body=$(alert_body "$1" "$2")
    echo "ALERT: $1 — $2" >&2
    if alert_post "$body"; then
        echo "alert: filed as a packet for emp-david" >&2
    else
        mkdir -p "$ALERT_SPOOL"
        # Unique per alert even within one second and one process
        # (two alerts in a burst must not overwrite each other).
        printf '%s' "$body" > "$ALERT_SPOOL/$(date -u +%Y-%m-%dT%H:%M:%SZ)-$$-$(( ALERT_SEQ=${ALERT_SEQ:-0}+1 ))-$RANDOM.json"
        echo "alert: the jobs API did not take it — kept ($(alert_count) waiting); filed on the next run that reaches the API" >&2
    fi
}

# alert_replay — file every kept alert, oldest first; stop at the first failure.
alert_replay() {
    local n f; n=$(alert_count); [ "$n" -gt 0 ] || return 0
    echo "alert: filing $n kept alert(s), oldest first" >&2
    for f in $(ls -1 "$ALERT_SPOOL" | grep '\.json$' | sort); do
        if alert_post "$(cat "$ALERT_SPOOL/$f")"; then rm -f "$ALERT_SPOOL/$f"; else echo "alert: replay stopped at $f — $(alert_count) still waiting" >&2; return 1; fi
    done
    echo "alert: all kept alerts filed" >&2
}
