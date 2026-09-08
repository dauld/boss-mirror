#!/bin/sh
# observe-units — the unit half of the host observation loop.
#
# observe-host.sh (beside this) records what the MACHINE is; this
# records what its UNITS are doing. It exists because of one night
# (packet 729329c6): the conductor went quiet and a CI-green train sat
# unmerged for 2+ hours with no signal anywhere — "is
# boss-train.service alive" was a question only a human with ssh could
# answer. Now it is a SoR read:
#   GET /api/estate/observations?scope=host-units
#
# Same design as the host observer, deliberately: observe and POST,
# never write anything; sh + jq, no python (directive 26d61c97). Per
# unit it records ActiveState, SubState, Result and ExecMainStatus,
# and for a unit that is not healthy, the last ~20 journal lines — the
# evidence rides the event, because by the time someone reads the
# observation the journal may have rotated, or the host may be the
# thing that is down.
#
# WHAT HEALTHY MEANS — derived from what systemd declares, never
# hardcoded per unit name:
#   - the unit file must be loaded (a not-found unit is a broken watch
#     list, and the watcher screams instead of skipping);
#   - ActiveState=failed is unhealthy, as are a non-success Result and
#     a non-zero ExecMainStatus;
#   - a .timer must be active (armed) — a disarmed timer is the same
#     quiet-conductor class, one hop earlier;
#   - a .service must be active unless Type=oneshot, whose resting
#     state between firings is inactive.
#
# Env: HOST_ID (required — must match the estate node row id),
#      JOBS_API (required),
#      UNITS   (optional, space-separated; the default is the union
#      across hosts, and each host's unit file or drop-in narrows it
#      to what that host actually runs — see the .service header).
#
# THE ALARM MUST NOT DEPEND ONLY ON THE API IT REPORTS TO (a7a19a1a).
# Every failure here is LOUD in two places: a failed POST or an
# unhealthy unit ends in stderr + a non-zero exit, so systemd shows a
# failed boss-estate-observe-units.service on the host itself, while
# the maintenance packet the timer opened stays unclosed on the SoR.
# Local half and remote half of the same alarm; losing the API costs
# only the remote one.
set -eu

: "${HOST_ID:?HOST_ID is required and must match the estate node id}"
: "${JOBS_API:?JOBS_API is required}"

UNITS="${UNITS:-boss-train.service cluster-deploy-runner.timer cluster-deploy-runner.service forgejo.service}"

# Word-split once; an observer with nothing to watch is a
# misconfiguration, not a vacuously healthy host.
# shellcheck disable=SC2086
set -- $UNITS
if [ $# -eq 0 ]; then
    echo "observe-units: UNITS is empty — nothing to watch is a config fault" >&2
    exit 78 # EX_CONFIG
fi

units_json='[]'
unhealthy=""

for unit in "$@"; do
    # `systemctl show` answers for ANY name (not-found included) and
    # omits properties a unit kind does not have (timers carry no Type
    # or ExecMainStatus), so every extraction tolerates absence.
    props=$(systemctl show "$unit" \
        --property=LoadState,ActiveState,SubState,Result,ExecMainStatus,Type)
    load=$(printf '%s\n' "$props" | sed -n 's/^LoadState=//p')
    active=$(printf '%s\n' "$props" | sed -n 's/^ActiveState=//p')
    sub=$(printf '%s\n' "$props" | sed -n 's/^SubState=//p')
    result=$(printf '%s\n' "$props" | sed -n 's/^Result=//p')
    exec_status=$(printf '%s\n' "$props" | sed -n 's/^ExecMainStatus=//p')
    svc_type=$(printf '%s\n' "$props" | sed -n 's/^Type=//p')

    healthy=true
    if [ "$load" != "loaded" ]; then healthy=false; fi
    if [ "$active" = "failed" ]; then healthy=false; fi
    case "$result" in "" | success) ;; *) healthy=false ;; esac
    case "$exec_status" in "" | 0) ;; *) healthy=false ;; esac
    # Expected-active, derived: oneshot services rest inactive between
    # firings; everything else watched here (daemons, armed timers)
    # must be active. `activating` is deliberately unhealthy — a
    # crash-looping conductor is not a running one, and one flapped
    # 5-minute reading clears on the next firing.
    expect_active=true
    case "$unit" in
    *.service) if [ "$svc_type" = "oneshot" ]; then expect_active=false; fi ;;
    esac
    if [ "$expect_active" = "true" ] && [ "$active" != "active" ]; then
        healthy=false
    fi

    journal=""
    if [ "$healthy" = "false" ]; then
        unhealthy="$unhealthy $unit"
        journal=$(journalctl -u "$unit" -n 20 --no-pager 2>&1 || true)
    fi

    units_json=$(printf '%s' "$units_json" | jq \
        --arg unit "$unit" --arg load "$load" --arg active "$active" \
        --arg sub "$sub" --arg result "$result" --arg exec "$exec_status" \
        --arg journal "$journal" --argjson healthy "$healthy" \
        '. + [{
            unit: $unit, load_state: $load, active_state: $active,
            sub_state: $sub, result: $result,
            exec_main_status: (if $exec == "" then null else ($exec | tonumber) end),
            healthy: $healthy
        } + (if $journal == "" then {} else { journal: $journal } end)]')
done

if [ -z "$unhealthy" ]; then node_healthy=true; else node_healthy=false; fi

# One node — this host — carrying its units. The estate door refuses
# an observation with no nodes (a probe that saw nothing is a failed
# probe), and every estate consumer finds the host id where the other
# scopes put it.
observation=$(jq -n \
    --arg id "$HOST_ID" \
    --arg observer "boss-estate-observe-units" \
    --argjson healthy "$node_healthy" \
    --argjson units "$units_json" \
    '{
      observed_at: (now | todate),
      observer: $observer,
      scope: "host-units",
      nodes: [{ id: $id, healthy: $healthy, units: $units }]
    }')

echo "observing $HOST_ID units: $# watched, unhealthy:${unhealthy:- none}"

# No temp file, body and status in one capture — the same lesson
# observe-host.sh carries (its first scheduled firing turned a curl -o
# write error into an UNREACHABLE lie).
resp=$(printf '%s' "$observation" | curl -s -w '\n%{http_code}' \
    -X POST -H 'content-type: application/json' \
    -H 'x-boss-user: {"id":"automation:estate-observer-units","role":"platform-admin","access_tier":"operator"}' \
    --data-binary @- \
    "$JOBS_API/api/estate/observation") ||
    { rc=$?; echo "jobs api: curl failed (exit $rc, target $JOBS_API)" >&2; exit 1; }
code=${resp##*
}
body=${resp%
*}

echo "jobs api: $code $body"
if [ "$code" != "202" ]; then
    echo "jobs api: refused the observation ($code, target $JOBS_API)" >&2
    exit 1
fi

# The observation is recorded; NOW fail, so the alarm has a local half
# that survives losing the API: stderr names the units, systemd shows
# a failed observer, and the maintenance packet stays open on the SoR
# until a healthy run closes it.
if [ -n "$unhealthy" ]; then
    echo "unhealthy:$unhealthy — observation recorded; failing loud locally too" >&2
    exit 1
fi
