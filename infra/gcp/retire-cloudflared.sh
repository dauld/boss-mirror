#!/usr/bin/env bash
#
# retire-cloudflared — stop, disable and remove boss-gcp's hand-written
# `cloudflared.service`, the VM tunnel connector, once the system of
# record says the in-cluster connector carries every hostname — and
# nothing else, ever. It never touches the tunnel in Cloudflare.
#
# WHY IT EXISTS (design 4c565f8c, decided by David 2026-09-16; backlog
# 0b7804f3, car 4)
# ---------------------------------------------------------------------
# boss-gcp has run `cloudflared.service` since 2026-06-09: a unit
# `cloudflared service install` wrote by hand, its tunnel token INLINE
# on the ExecStart line (`tunnel run --token <v>`), declared in no role
# and observed by no estate observer (not a boss-* unit). On 2026-09-16
# the operator read that unit through unit-cat and the token rode the
# packet into the system of record (backlog 9c760dd7) — the mask has
# since learned the flag shape, and the rotation packet e202c7c3 moved
# every hostname onto a NEW tunnel whose connector runs IN the cluster
# (infra/cluster/manifests/cloudflared.yaml, car 1). The converge of
# 08:25Z recorded `cloudflared: connected` and a `tunnel_ingress`
# naming both hostnames. The VM connector is now redundancy nobody
# chose, still attached to the OLD tunnel that no hostname points at —
# and the rotation cannot finish while it is: the broker's revoke phase
# refuses to delete a tunnel with live connections, and this unit IS
# that connection. Retiring it is the last step of the leak's
# rotation, and the leak class is closed on this host by there being
# no token there.
#
# Retiring it by hand is four commands and a `rm` on a console, with
# nothing recording what the unit said the moment before or that the
# hand-over was real. So it is an ops verb through the audited door
# (infra/ops/verbs/retire-cloudflared.json), and this script is its
# whole mechanism.
#
# THE BOUNDS, in the order they are applied — each refuses loudly and
# names the bound:
#
#   1. THE MODE. Exactly one argument, `--dry-run` or `--for-real`. The
#      allowlist admits only those two literals; the script re-checks
#      rather than relying on one layer.
#   2. THE HOSTNAMES, from the tree. Every `hostname` the checkout's
#      infra/cluster/instances.toml declares — the ONE declaration the
#      tunnel ingress is rendered from (render-tunnel-config.sh) — is a
#      hostname the in-cluster connector must be serving. Never a list
#      typed here (CLAUDE.md §9a).
#   3. THE HAND-OVER, read through the system of record BEFORE anything
#      is stopped. The newest maintenance-cluster-converge packet whose
#      `run` step carries a `cloudflared` field is the newest converge
#      that OBSERVED the connector (a no-op tick — `unchanged` — reads
#      nothing and is not evidence either way). It must be under two
#      hours old, must say `cloudflared: connected`, and its
#      `tunnel_ingress` must name every hostname from bound 2 as
#      `<hostname> →`. No such packet, a dark system of record, a stale
#      one, `not-ready`, `skipped (…)`, or a hostname missing from the
#      ingress is a refusal naming the packet and the fact. A bound
#      that cannot be evaluated is not passed. The read is SIGNED (the
#      x-boss-user header the runner itself uses): an unidentified read
#      of the jobs API answers `total: 0`, which is a denied scope, not
#      an empty world, and would read as "no converge" — a refusal,
#      never a pass, but a misleading one.
#   4. THE SNAPSHOT. `systemctl list-units --all 'cloudflared*'` and
#      the unit as systemd holds it, PRINTED THROUGH infra/ops/unit-cat.sh
#      — the same mask that door applies, so the token on the ExecStart
#      line leaves this host as `--token <masked by unit-cat>` and the
#      packet holds what the unit was without holding the credential.
#      The drop-in directory's contents are listed by name.
#   5. THE PLAN. Exactly `cloudflared.service`: present when its file is
#      under /etc/systemd/system or systemd lists it loaded. Absent on
#      both counts is `not on this host` and an OK with nothing to do —
#      the second run of this verb — never a refusal.
#
# `--dry-run` runs every bound, prints the snapshot and the plan
# (`would stop+disable+remove cloudflared.service`, plus each drop-in
# directory it would remove) and calls nothing that acts. `--for-real`
# then: `systemctl disable --now` (stops it, drops its symlinks), the
# unit file removed, `stopped+disabled+removed cloudflared.service`
# printed as it lands, the drop-in directory removed whole (the token
# lives in the unit file; a drop-in is the same author's), ONE
# daemon-reload, reset-failed, a second snapshot — and success is
# refused (exit 1, the unit named) while systemd still lists the unit
# or its file is still on disk.
#
# WHAT IT DOES NOT DO. It never widens: the unit name is fixed here and
# nothing else on the host — boss-ops-runner (the door), boss-gcp-
# converge, WireGuard, caddy, postgres, a `cloudflared-update.timer`
# should one exist — is reachable, by construction. It does NOT touch
# the tunnel in Cloudflare: deleting the old tunnel is the broker's
# revoke phase (rotation packet e202c7c3), which completes on its own
# once that tunnel shows zero connections — which is what stopping this
# unit produces. It captures nothing: the unit is a token nobody should
# hold and a routing that lives in the tree.
#
# USAGE
#   retire-cloudflared.sh --dry-run | --for-real
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the bound; nothing stopped
#   1  failed part-way — the record states what was done and what was
#      not; or a tool this needs could not answer
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_JOBS_URL      the system of record (the runner's unit pins it;
#                      unset is a refusal, never a default)
#   BOSS_OPS_ACTOR     who the converge read signs as (the runner's own
#                      variable; default automation:ops-runner)
#   INSTALL_ETC        where the unit lives (default /etc/systemd/system —
#                      the installer's own seam, same name, same default)

set -uo pipefail

ME="retire-cloudflared"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
ETC="${INSTALL_ETC:-/etc/systemd/system}"
NODE_ID="boss-gcp"
UNIT="cloudflared.service"
UNIT_CAT="$REPO/infra/ops/unit-cat.sh"
INSTANCES="$REPO/infra/cluster/instances.toml"
CONVERGE_KIND="maintenance-cluster-converge"
# Two hours: a converge runs on every train and the timer ticks every
# ten minutes, so an observation older than this is one the estate has
# had several chances to refresh and did not.
MAX_AGE_S=7200

# --- bound 1: the mode ------------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real"
    say "  --dry-run   every bound, the masked snapshot and the plan; stops nothing"
    say "  --for-real  stop+disable $UNIT, remove its file and drop-ins, daemon-reload, verify"
    exit 2
}
[ "$#" -eq 1 ] || usage
DRY=""
case "$1" in
    --dry-run) DRY=1 ;;
    --for-real) DRY=0 ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# --- bound 2: the hostnames, from the tree ----------------------------------
[ -f "$INSTANCES" ] || { say "CANNOT ANSWER — $INSTANCES is missing, and it is the only declaration of the hostnames the tunnel serves"; exit 1; }
HOSTNAMES=()
while IFS= read -r h; do
    [ -n "$h" ] && HOSTNAMES+=("$h")
done < <(awk '
    /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
    $1 == "hostname" { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); print }
' "$INSTANCES")
[ "${#HOSTNAMES[@]}" -gt 0 ] \
    || refuse "${INSTANCES#"$REPO"/} declares no instance hostname — there is nothing the in-cluster connector could be shown to serve"
say "hostnames the in-cluster connector must serve (${INSTANCES#"$REPO"/}): ${HOSTNAMES[*]}"

# --- bound 3: the hand-over, through the system of record -------------------
[ -n "${BOSS_JOBS_URL:-}" ] \
    || refuse "BOSS_JOBS_URL is not set, so the hand-over cannot be read off the system of record and this bound cannot be evaluated. There is no safe default (the runner's unit pins it). Nothing was stopped."
ACTOR="${BOSS_OPS_ACTOR:-automation:ops-runner}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"
CONVERGE_URL="$BOSS_JOBS_URL/api/jobs?kind=$CONVERGE_KIND&limit=40"
if ! curl -fsS --max-time 15 -H "x-boss-user: $BOSS_USER" "$CONVERGE_URL" > "$TMP/converges.json" 2> "$TMP/curl.err"; then
    say "REFUSED — the system of record did not answer the converge read ($CONVERGE_URL):"
    sed 's/^/    /' "$TMP/curl.err" >&2
    say "  the hand-over cannot be evaluated, and a bound that cannot be evaluated is not passed. Nothing was stopped."
    exit 2
fi
# The newest converge that OBSERVED the connector: the run step carries
# a `cloudflared` field only on a converge that deployed (a no-op tick
# records `unchanged` and reads nothing). Newest by the run step's own
# completed_at, not by the API's order.
OBS=$(jq -c '
    (if type == "object" and has("data") then .data else . end)
    | map(. as $j
          | ((.steps // []) | map(select(.spec_slug == "run")) | .[0]) as $run
          | select($run != null and ($run.metadata.cloudflared // "") != "")
          | {id: $j.id, at: ($run.completed_at // ""),
             cloudflared: $run.metadata.cloudflared,
             ingress: ($run.metadata.tunnel_ingress // ""),
             render: ($run.metadata.tunnel_ingress_render // "")})
    | sort_by(.at) | last // empty' "$TMP/converges.json" 2>/dev/null)
if [ -z "$OBS" ]; then
    n=$(jq -r '(if type == "object" and has("data") then .data else . end) | length' "$TMP/converges.json" 2>/dev/null || echo "?")
    refuse "no $CONVERGE_KIND packet in the newest $n reports a \`cloudflared\` field — no converge has observed the in-cluster connector, so the hand-over cannot be shown. (A read that answers 0 packets is a denied scope as often as an empty world: the read signed as $ACTOR.) Nothing was stopped."
fi
OBS_ID=$(printf '%s' "$OBS" | jq -r .id)
OBS_AT=$(printf '%s' "$OBS" | jq -r .at)
OBS_CF=$(printf '%s' "$OBS" | jq -r .cloudflared)
OBS_INGRESS=$(printf '%s' "$OBS" | jq -r .ingress)
OBS_RENDER=$(printf '%s' "$OBS" | jq -r .render)
say "hand-over: converge $OBS_ID (run completed $OBS_AT) recorded cloudflared: $OBS_CF; tunnel_ingress: ${OBS_INGRESS:-<none>}${OBS_RENDER:+; render $OBS_RENDER}"
# Age. GNU date parses the API's RFC 3339 with fractional seconds; a
# timestamp it cannot parse is a bound it cannot evaluate.
obs_epoch=$(date -u -d "$OBS_AT" +%s 2>/dev/null) || obs_epoch=""
case "${obs_epoch:-empty}" in
    empty|*[!0-9]*) refuse "converge $OBS_ID's run step carries a completed_at this host cannot read (\`$OBS_AT\`), so its age cannot be evaluated. Nothing was stopped." ;;
esac
age=$(( $(date -u +%s) - obs_epoch ))
if [ "$age" -gt "$MAX_AGE_S" ]; then
    refuse "the newest converge that observed the connector, $OBS_ID, completed $OBS_AT — $((age / 60)) min ago, older than the $((MAX_AGE_S / 60)) min ceiling. What it saw is not evidence about now; converge again (a train, or the timer) and ask again. Nothing was stopped."
fi
if [ "$OBS_CF" != "connected" ]; then
    refuse "converge $OBS_ID ($OBS_AT) recorded cloudflared: $OBS_CF — the in-cluster connector was not connected when last observed, so the hand-over is not real. Nothing was stopped."
fi
missing=()
for h in "${HOSTNAMES[@]}"; do
    case "$OBS_INGRESS" in
        *"$h →"*|*"$h ->"*) ;;
        *) missing+=("$h") ;;
    esac
done
if [ "${#missing[@]}" -gt 0 ]; then
    refuse "converge $OBS_ID's tunnel_ingress (\`${OBS_INGRESS:-<none>}\`) does not route ${missing[*]} — every hostname ${INSTANCES#"$REPO"/} declares must be served by the in-cluster connector before the VM one goes. Nothing was stopped."
fi
say "hand-over: real — connected $((age / 60)) min ago, ingress routes ${HOSTNAMES[*]}"

# --- bound 4: the snapshot, masked ------------------------------------------
[ -x "$UNIT_CAT" ] || { say "CANNOT ANSWER — $UNIT_CAT is missing or not executable, and it is the mask this verb prints the unit through"; exit 1; }
snapshot() { # <label>
    echo "--- cloudflared* units on $NODE_ID ($1, $(date -u +%Y-%m-%dT%H:%M:%SZ)) ---"
    systemctl list-units --all --no-pager --plain --no-legend -- 'cloudflared*' || return 1
    echo "--- $UNIT as systemd holds it ($1, masked by unit-cat) ---"
    # exit 4 is unit-cat's honest "no such unit" — after the removal
    # that is the answer wanted; before it, it means the plan is empty.
    "$UNIT_CAT" "$UNIT" 2>&1
    rc=$?
    case "$rc" in 0|4) ;; *) return 1 ;; esac
    if [ -d "$ETC/$UNIT.d" ]; then
        echo "--- $ETC/$UNIT.d/ ($1) ---"
        ls -A "$ETC/$UNIT.d"
    fi
    echo "--- end of snapshot ---"
}
if ! snapshot before > "$TMP/before" 2> "$TMP/before.err"; then
    say "CANNOT ANSWER — systemctl could not list or print this host's $UNIT:"
    sed 's/^/    /' "$TMP/before.err" >&2
    say "  Nothing was stopped."
    exit 1
fi
cat "$TMP/before"
loaded_state() { # <file> -> load state of $UNIT or empty
    awk -v u="$UNIT" '$1 == u { print $2; exit }' "$1"
}

# --- bound 5: the plan --------------------------------------------------------
PLAN=()
load="$(loaded_state "$TMP/before")"
if [ -f "$ETC/$UNIT" ] || { [ -n "$load" ] && [ "$load" != "not-found" ]; }; then
    PLAN+=("$UNIT")
else
    echo "not on this host $UNIT"
fi
DROPINS=()
[ -d "$ETC/$UNIT.d" ] && DROPINS+=("$ETC/$UNIT.d")
say "plan: ${#PLAN[@]} unit(s) and ${#DROPINS[@]} drop-in dir(s) present on $NODE_ID"
if [ "$DRY" = 1 ]; then
    for u in "${PLAN[@]+"${PLAN[@]}"}"; do echo "would stop+disable+remove $u"; done
    for d in "${DROPINS[@]+"${DROPINS[@]}"}"; do echo "would remove $d/"; done
    say "DRY RUN — would stop+disable+remove ${#PLAN[@]} unit(s) and ${#DROPINS[@]} drop-in dir(s), then daemon-reload. The tunnel in Cloudflare is not touched either way: the broker's revoke phase (rotation e202c7c3) deletes the old tunnel on its own once it shows zero connections. Every bound passed; nothing was stopped."
    exit 0
fi
if { [ "${#PLAN[@]}" -gt 0 ] || [ "${#DROPINS[@]}" -gt 0 ]; } && [ ! -w "$ETC" ]; then
    refuse "$ETC is not writable by $(id -un), so no unit file could be removed"
fi

# --- the retirement -----------------------------------------------------------
DONE=()
for u in "${PLAN[@]+"${PLAN[@]}"}"; do
    # The internal guard: no path through this loop touches a unit other
    # than the one named at the top — even if a future edit widens PLAN,
    # this line refuses.
    if [ "$u" != "$UNIT" ]; then
        say "REFUSED — \`$u\` is not $UNIT; the loop was handed a unit it must not touch."
        exit 2
    fi
    if ! systemctl disable --now -- "$u" > "$TMP/disable.out" 2>&1; then
        cat "$TMP/disable.out" >&2
        say "FAILED at \`$u\` — systemctl disable --now exited non-zero; its file is still at $ETC/$u and no daemon-reload was run. Fix the unit and ask again."
        exit 1
    fi
    if [ -f "$ETC/$u" ]; then
        if ! rm -f -- "$ETC/$u" 2> "$TMP/rm.err"; then
            cat "$TMP/rm.err" >&2
            say "FAILED at \`$u\` — stopped+disabled, but its file could not be removed from $ETC; the token is still on disk there. No daemon-reload was run."
            exit 1
        fi
        echo "stopped+disabled+removed $u"
    else
        echo "stopped+disabled+removed $u (no file under $ETC)"
    fi
    DONE+=("$u")
done
for d in "${DROPINS[@]+"${DROPINS[@]}"}"; do
    if ! rm -rf -- "$d" 2> "$TMP/rm.err"; then
        cat "$TMP/rm.err" >&2
        say "FAILED — $d/ could not be removed; the unit file is gone, no daemon-reload was run."
        exit 1
    fi
    echo "removed $d/"
done

if [ "${#DONE[@]}" -gt 0 ] || [ "${#DROPINS[@]}" -gt 0 ]; then
    if ! systemctl daemon-reload > "$TMP/reload.out" 2>&1; then
        cat "$TMP/reload.out" >&2
        say "FAILED — removed the unit but daemon-reload exited non-zero; systemd may still hold it until it is run."
        exit 1
    fi
    echo "daemon-reload"
    for u in "${DONE[@]+"${DONE[@]}"}"; do systemctl reset-failed -- "$u" >/dev/null 2>&1 || true; done
fi

# --- verified gone, because exit 0 and gone are different claims ------------
if ! snapshot after > "$TMP/after" 2>/dev/null; then
    say "FAILED — removed $UNIT but systemctl could not list it afterwards to verify"
    exit 1
fi
cat "$TMP/after"
still=()
[ -e "$ETC/$UNIT" ] && still+=("$UNIT (file)")
load="$(loaded_state "$TMP/after")"
if [ -n "$load" ] && [ "$load" != "not-found" ]; then still+=("$UNIT (loaded: $load)"); fi
[ -d "$ETC/$UNIT.d" ] && still+=("$ETC/$UNIT.d/")
if [ "${#still[@]}" -gt 0 ]; then
    say "FAILED — the removal returned success and $UNIT is STILL present: ${still[*]}"
    say "  something put it back or systemd still holds it; this verb will not try again."
    exit 1
fi
say "OK — stopped+disabled+removed ${#DONE[@]} unit(s) (${DONE[*]:-none}) and ${#DROPINS[@]} drop-in dir(s) on $NODE_ID; hand-over evidenced by converge $OBS_ID. No token remains in $ETC. The tunnel in Cloudflare is untouched: the broker's revoke phase (rotation e202c7c3) deletes the old tunnel on its own once it shows zero connections."
exit 0
