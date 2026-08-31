#!/bin/sh
# observe-host — the estate observation loop, for hosts the cluster
# observer cannot see.
#
# boss-estate-observe (CronJob) covers scope kubernetes-nodes. The two
# non-cluster machines — the conductor VM and the forge host — had no
# observed series at all: boss-gcp's own registry note reads "48GB is
# the smallest disk in the estate and nothing watches it", and the
# 2026-08-30 disk-headroom sweep measured both BY HAND, which is what
# spawned the class car this script answers (a5d14977, sweep 3ff8f240).
#
# Same design as the cluster observer, deliberately: observe and POST,
# never write the registry (declared vs observed stay apart, or nothing
# can be found missing); capacity rounds to nearest GiB — THE one
# stated rounding rule; free space is observed state, so it rides the
# event, not a column. sh + jq, no python (directive 26d61c97).
#
# Env: HOST_ID (required — must match the estate node row id),
#      JOBS_API (required), ADDRESS (optional; default: first global IP).
#
# A failed POST fails LOUDLY with the target named — the cluster
# observer shipped silent-on-curl-failure once (3ddd8333) and that
# class does not get a second landing.
set -eu

: "${HOST_ID:?HOST_ID is required and must match the estate node id}"
: "${JOBS_API:?JOBS_API is required}"

ADDRESS="${ADDRESS:-$(hostname -I 2>/dev/null | awk '{print $1}')}"

cpu=$(nproc)
mem_kb=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)
mem_gb=$(( (mem_kb + 524288) / 1048576 ))
# Root filesystem: total and available, nearest GiB.
disk_kb=$(df -k / | awk 'NR==2 {print $2}')
free_kb=$(df -k / | awk 'NR==2 {print $4}')
disk_gb=$(( (disk_kb + 524288) / 1048576 ))
free_gb=$(( (free_kb + 524288) / 1048576 ))
up_s=$(awk '{print int($1)}' /proc/uptime)

observation=$(jq -n \
  --arg id "$HOST_ID" \
  --arg address "$ADDRESS" \
  --arg observer "boss-estate-observe-host" \
  --argjson cpu "$cpu" \
  --argjson memory_gb "$mem_gb" \
  --argjson disk_gb "$disk_gb" \
  --argjson disk_free_gb "$free_gb" \
  --argjson uptime_s "$up_s" \
  '{
    observed_at: (now | todate),
    observer: $observer,
    scope: "host",
    nodes: [{
      id: $id, address: $address, cpu: $cpu, memory_gb: $memory_gb,
      disk_gb: $disk_gb, disk_free_gb: $disk_free_gb,
      uptime_s: $uptime_s, ready: true
    }]
  }')

echo "observing $HOST_ID: cpu=$cpu mem=${mem_gb}G disk=${disk_gb}G free=${free_gb}G"

# No temp file: the first scheduled firing failed curl exit 23 (a WRITE
# error) under the unit while the POST itself had succeeded server-side,
# and the old message called that UNREACHABLE - two lies from one -o.
# Body and status ride one capture instead; the label states only what
# curl's exit actually says.
resp=$(printf '%s' "$observation" | curl -s -w '\n%{http_code}' \
  -X POST -H 'content-type: application/json' \
  -H 'x-boss-user: {"id":"automation:estate-observer-host","role":"platform-admin","access_tier":"operator"}' \
  --data-binary @- \
  "$JOBS_API/api/estate/observation") \
  || { rc=$?; echo "jobs api: curl failed (exit $rc, target $JOBS_API)"; exit 1; }
code=${resp##*
}
body=${resp%
*}

echo "jobs api: $code $body"
test "$code" = "202"
