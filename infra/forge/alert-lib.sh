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
# Shape: an urgent backlog-item owned by the PLATFORM OWNER, titled
# from the alert, with the facts in metadata. Not an ops-request: that
# kind asks a host to run a verb; this one asks a person to look.
#
# WHO THE OWNER IS (backlog 3c23662d — until 2026-09-18 this file wrote
# a named person into every alert). BOSS_PLATFORM_OWNER when the unit
# carries it; else the people registry's first active platform-admin
# hire, read the way this file reads the system of record (the people
# service is the same address on boss-ports' people port — sor-ports.env
# beside this file, the table the unattended prove door hands its reader); else
# NOBODY, an empty owner_id the jobs API resolves from the kind's
# owner_role or refuses by name. Never a literal. The read is
# best-effort with a short timeout on purpose: the alert that matters
# most is "the system of record is dark", and a dark registry must not
# stop the alert being kept.

ALERT_SPOOL="${ALERT_SPOOL:-/var/tmp/boss-alert-spool}"
# The system of record, from /etc/boss/sor.env (infra/lib/sor.sh) — no
# fallback address: an alert filed against a wrong instance is an alert
# nobody reads (backlog 5222163e).
# shellcheck source=infra/lib/sor.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
sor_require JOBS_API
ALERT_API="$JOBS_API"
# Who files: the watchdog unless the sourcing script names itself
# (the cluster converge signs its tenant-check refusals as
# automation:cluster-deploy-runner, backlog 1af5119d) — an alert that
# credits the wrong loop sends a reader to the wrong journal.
ALERT_ACTOR="${ALERT_ACTOR:-automation:cluster-watchdog}"
ALERT_USER='{"id":"'"$ALERT_ACTOR"'","role":"platform-admin","access_tier":"operator"}'
ALERT_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# alert_people_api — the people door: ALERT_API's host on the people
# port from sor-ports.env (read as data, never sourced). Empty when the
# table is missing or names no people port: no guessed port, no read.
alert_people_api() {
    local table port scheme hostport
    [ -r "$ALERT_LIB_DIR/sor-ports.env" ] || return 0
    table=$(grep -Ev '^[[:space:]]*(#|$)' "$ALERT_LIB_DIR/sor-ports.env" | tr -s '[:space:]' ' ')
    port=$(printf ' %s ' "$table" | sed -n 's/.* people=\([0-9][0-9]*\) .*/\1/p')
    [ -n "$port" ] || return 0
    scheme="${ALERT_API%%://*}"; hostport="${ALERT_API#*://}"; hostport="${hostport%%/*}"
    printf '%s://%s:%s' "$scheme" "${hostport%%:*}" "$port"
}

# alert_owner — the owner_id an alert is filed with. Pure shell + awk
# (the forge units must not need jq or python): the roster is split at
# object boundaries, each object's id and hire_date are lifted off its
# quoted fields (a null hire_date sorts last, as `~`), and the earliest
# hire wins with the id as the tie-break — the same rule as
# boss_core::platform_owner::first_hire.
alert_owner() {
    if [ -n "${BOSS_PLATFORM_OWNER:-}" ]; then printf '%s' "$BOSS_PLATFORM_OWNER"; return; fi
    local api; api=$(alert_people_api)
    [ -n "$api" ] || return 0
    curl -s --max-time 5 -H "x-boss-user: $ALERT_USER" \
        "$api/api/people?role=platform-admin&status=active" 2>/dev/null \
        | tr '}' '\n' \
        | awk -F'"' '{ id=""; hd="~"
            for (i = 1; i < NF; i++) {
                if ($i == "id" && $(i+1) == ":") id = $(i+2)
                if ($i == "hire_date" && $(i+1) == ":") hd = $(i+2)
            }
            if (id != "") print hd "\t" id }' \
        | sort | awk -F'\t' 'NR == 1 { printf "%s", $2 }'
    # Best-effort by design: a dark registry is empty output (nobody),
    # never a failed alert — under a caller's `set -e` the alert must
    # still be raised and kept.
    return 0
}

# alert_body TITLE DETAIL — the packet JSON, raised_at now, owned by
# whoever alert_owner names (nobody when it names no one).
alert_body() {
    local title="$1" detail="$2" at owner
    at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    owner=$(alert_owner)
    python_free_json "$title" "$detail" "$at" "$owner"
}
# JSON without jq or python (the forge units must not need either):
# escape backslashes, quotes and newlines by hand.
_esc() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr '\n' ' '; }
python_free_json() {
    local title detail at owner
    title=$(_esc "$1"); detail=$(_esc "$2"); at="$3"; owner=$(_esc "${4:-}")
    printf '{"kind":"backlog-item","status":"open","owner_id":"%s","priority":"urgent","tags":["alert","cluster"],"subject":{"subject_kind":"custom","id":"boss-cluster"},"title":"%s","metadata":{"area":"alert","filed_by":"%s","raised_at":"%s","detail":"%s"}}' "$owner" "$title" "$ALERT_ACTOR" "$at" "$detail"
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
        echo "alert: filed as a packet for the platform owner" >&2
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
