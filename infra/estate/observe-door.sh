#!/usr/bin/env bash
# observe-door — the estate's doors, probed from OUTSIDE what they open
# onto (backlog e6406701).
#
# WHY. Incident 55d001b0: both of the dev pod's ssh doors — the LAN VIP
# and dev.algedonic.dev — were dark for ~36 hours from
# 2026-09-22T23:23Z, and a person found it. The pod's postStart judges
# the door once, as sshd starts ('ssh door: sshd started on :22' /
# 'CLOSED …'), but nothing read that line and nothing probed :22
# afterwards. A door judged only from inside its pod is judged by its
# patient (CLAUDE.md §Diagnosis: an arm that needs the patient is not an
# arm), so this runs on the forge — a LAN host outside the cluster —
# as the second line of cluster-watchdog.service, every five minutes.
#
# WHAT IT DOES. For each door infra/estate/doors.toml declares:
#   lan    — TCP connect to address:port, then ssh-keyscan it. Open
#            only when BOTH answer: a port that accepts and offers no
#            host key is not a door anyone can use.
#   public — resolve the public name (getent hosts). Open when it does.
# A dark half's FIRST dark reading is kept under DOOR_STATE_DIR (one
# file per door and half, holding the time), and every later reading
# carries it forward as `dark_since`; an open reading deletes it. The
# observer holds this because it is the instrument that saw the
# transition — the cluster watchdog counts its own dark checks the same
# way. It judges nothing: `estate.compare` judges each dark half
# against the door's band (`band_s`, from doors.toml), `estate.alarm`
# files ONE urgent packet per half dark past it, `estate.recover`
# closes it once the half answers again, and `boss orient` prints the
# DOOR line from the same comparison.
#
# THE RECORD. One `door` observation per run, POSTed through the estate
# door (observe-lib.sh's post_observation) and RETAINED in its own
# spool when the system of record will not take it, replayed oldest
# first on the next run that reaches it — so an outage of the record
# shows in the door series as readings that arrived late, not as a
# hole. Every run also prints one line per door to the forge's journal,
# which is readable over the journal gateway with the API dark.
#
# Env: JOBS_API (required to post; from /etc/boss/sor.env),
#      DOORS_FILE (default: doors.toml beside this script),
#      DOOR_STATE_DIR (default /var/tmp/boss-door-state — persists
#      across reboots, writable by the unit's user),
#      DOOR_SPOOL_DIR (default /var/tmp/boss-estate-spool-door — its
#      own directory: the host observer's spool names files by
#      observed_at, and two observers posting in one second must not
#      overwrite each other's reading),
#      DOOR_TIMEOUT_S (default 5 — per probe).
#
# `--print` probes and prints the observation instead of posting it.
# `--declared` prints the parsed declaration and probes nothing — the
# one question worth asking the file without touching the network.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOORS_FILE="${DOORS_FILE:-$here/doors.toml}"
DOOR_STATE_DIR="${DOOR_STATE_DIR:-/var/tmp/boss-door-state}"
DOOR_TIMEOUT_S="${DOOR_TIMEOUT_S:-5}"
SPOOL_DIR="${DOOR_SPOOL_DIR:-/var/tmp/boss-estate-spool-door}"
# shellcheck source=/dev/null
. "$here/observe-lib.sh"

mode=post
case "${1:-}" in
    --print) mode=print ;;
    --declared) mode=declared ;;
    '') ;;
    *) echo "usage: observe-door.sh [--print | --declared]" >&2; exit 2 ;;
esac

refuse() { echo "observe-door: $DOORS_FILE: $*" >&2; exit 1; }

# The declaration, one door per line as `id|lan|public|band_minutes`.
# `key = "value"` per line under each [[door]] table — the shell-read
# rule estate.toml and roles.toml live under. A key missing from a
# table comes out empty, and is refused below by name.
declared() {
    awk '
        function val(s) { sub(/^[^=]*=[ \t]*/, "", s); sub(/[ \t]+$/, "", s); gsub(/"/, "", s); return s }
        function emit() { print id "|" lan "|" pub "|" band }
        /^[ \t]*#/ { next }
        /^\[\[door\]\][ \t]*$/ { if (open) emit(); open = 1; id = lan = pub = band = ""; next }
        /^\[/ { if (open) emit(); open = 0; next }
        open && /^id[ \t]*=/ { id = val($0) }
        open && /^lan[ \t]*=/ { lan = val($0) }
        open && /^public[ \t]*=/ { pub = val($0) }
        open && /^dark_band_minutes[ \t]*=/ { band = val($0) }
        END { if (open) emit() }
    ' "$DOORS_FILE"
}

[ -r "$DOORS_FILE" ] || refuse "cannot be read"
doors=$(declared) || refuse "could not be parsed"
# Nothing to watch is refused, never reported: an observer that posts
# nothing reads exactly like a quiet door.
[ -n "$doors" ] || refuse "declares no door — refusing: an observer with nothing to watch reports nothing, which reads as a door nobody needs to look at"
while IFS='|' read -r id lan pub band; do
    [ -n "$id" ] || refuse "a [[door]] table has no id"
    case "$lan" in *:*[0-9]) ;; *) refuse "door $id: lan must be address:port, got '$lan'" ;; esac
    [ -n "$pub" ] || refuse "door $id: no public name"
    case "${band:-empty}" in empty|*[!0-9]*|0) refuse "door $id: dark_band_minutes must be a whole number of minutes above zero, got '$band'" ;; esac
done <<< "$doors"

if [ "$mode" = declared ]; then
    while IFS='|' read -r id lan pub band; do
        echo "$id $lan $pub band_s=$(( band * 60 ))"
    done <<< "$doors"
    exit 0
fi
if [ "$mode" = post ]; then
    : "${JOBS_API:?JOBS_API is required — the system of record this observation is posted to (/etc/boss/sor.env)}"
fi

now=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# dark_since ID HALF OPEN — the first dark reading of this half, kept
# across runs; forgotten once the half is open again.
dark_since() {
    local f="$DOOR_STATE_DIR/$1.$2"
    if [ "$3" = true ]; then rm -f "$f"; return 0; fi
    mkdir -p "$DOOR_STATE_DIR"
    [ -s "$f" ] || printf '%s\n' "$now" > "$f"
    sed -n 1p "$f"
}

# probe_lan HOST PORT — sets lan_tcp, lan_key, lan_open, lan_reason.
probe_lan() {
    local host="$1" port="$2" err rc
    lan_tcp=false lan_key='' lan_open=false lan_reason=''
    err=$(timeout "$DOOR_TIMEOUT_S" bash -c 'exec 3<>"/dev/tcp/$1/$2"' _ "$host" "$port" 2>&1)
    rc=$?
    if [ "$rc" -eq 124 ]; then
        lan_reason="tcp connect timed out after ${DOOR_TIMEOUT_S}s"
        return
    elif [ "$rc" -ne 0 ]; then
        lan_reason="tcp connect failed: $(printf '%s\n' "$err" | sed -n 's/.*connect: //p' | sed -n 1p)"
        [ "$lan_reason" != "tcp connect failed: " ] || lan_reason="tcp connect failed (exit $rc)"
        return
    fi
    lan_tcp=true
    if ! command -v ssh-keyscan >/dev/null 2>&1; then
        lan_reason="the port accepts, but ssh-keyscan is not installed on this observer, so no host key could be asked for"
        return
    fi
    # awk reads to the end (no early exit), so nothing upstream is cut
    # off mid-write under pipefail.
    lan_key=$(timeout "$(( DOOR_TIMEOUT_S * 2 ))" ssh-keyscan -T "$DOOR_TIMEOUT_S" -p "$port" "$host" 2>/dev/null \
        | awk '!/^#/ && NF >= 3 && k == "" { k = $2 } END { if (k != "") print k }')
    if [ -n "$lan_key" ]; then
        lan_open=true
    else
        lan_reason="the port accepts, but offered no ssh host key — sshd is not answering behind it"
    fi
}

# probe_public NAME — sets pub_addr, pub_open, pub_reason.
probe_public() {
    pub_addr=$(timeout "$DOOR_TIMEOUT_S" getent hosts "$1" 2>/dev/null | awk 'a == "" { a = $1 } END { if (a != "") print a }')
    if [ -n "$pub_addr" ]; then
        pub_open=true pub_reason=''
    else
        pub_open=false pub_reason="does not resolve (getent hosts answered nothing within ${DOOR_TIMEOUT_S}s)"
    fi
}

# say LABEL OPEN SINCE REASON — one half, as the journal line prints it.
say() { if [ "$2" = true ]; then echo "$1 open"; else echo "$1 DARK since $3 ($4)"; fi; }

nodes='[]'
while IFS='|' read -r id lan pub band; do
    probe_lan "${lan%:*}" "${lan##*:}"
    probe_public "$pub"
    lan_since=$(dark_since "$id" lan "$lan_open")
    pub_since=$(dark_since "$id" public "$pub_open")
    node=$(jq -n \
        --arg id "$id" --argjson band_s "$(( band * 60 ))" \
        --arg lan "$lan" --argjson lan_open "$lan_open" --argjson lan_tcp "$lan_tcp" \
        --arg lan_key "$lan_key" --arg lan_reason "$lan_reason" --arg lan_since "$lan_since" \
        --arg pub "$pub" --argjson pub_open "$pub_open" --arg pub_addr "$pub_addr" \
        --arg pub_reason "$pub_reason" --arg pub_since "$pub_since" \
        'def nz: if . == "" then null else . end;
         { id: $id, band_s: $band_s, halves: [
             { half: "lan", target: $lan, open: $lan_open, tcp: $lan_tcp,
               keyscan: ($lan_key | nz), reason: ($lan_reason | nz),
               dark_since: ($lan_since | nz) },
             { half: "public", target: $pub, open: $pub_open,
               address: ($pub_addr | nz), reason: ($pub_reason | nz),
               dark_since: ($pub_since | nz) } ] }')
    nodes=$(jq -c --argjson n "$node" '. + [$n]' <<< "$nodes")
    echo "door $id: $(say "lan $lan" "$lan_open" "$lan_since" "$lan_reason") · $(say "public $pub" "$pub_open" "$pub_since" "$pub_reason") — band ${band}m"
done <<< "$doors"

observation=$(jq -cn --arg at "$now" --argjson nodes "$nodes" \
    '{ observed_at: $at, observer: "boss-door-observe", scope: "door", nodes: $nodes }')

if [ "$mode" = print ]; then
    printf '%s\n' "$observation"
    exit 0
fi

if post_observation "$observation"; then
    spool_replay || true
else
    spool_put "$observation"
    echo "retained: door observation spooled ($(spool_count) waiting) — replays when the jobs api answers"
    exit 1
fi
