#!/usr/bin/env bash
#
# retire-second-stack — stop and disable the second, older BOSS stack on
# boss-gcp: exactly the units infra/gcp/second-stack-units.txt names,
# after capturing the database they share, and nothing else, ever.
#
# WHY IT EXISTS (design 9e3e093f, decided by David 2026-09-11; backlog
# d5941ef3 car 2)
# ---------------------------------------------------------------------
# boss-gcp is the WireGuard bastion, the off-cluster observer and the ML
# batch host. It ALSO still runs a complete older BOSS stack — 32
# services against a database at 127.0.0.1 that is not the system of
# record, one of them (boss-docs-api) serving a crate deleted from the
# tree — plus the ten `legacy-stack` chores that maintain that stack's
# data, six of them red for three weeks. A unit installed and never
# serving is worse than absent: it reports failed forever and trains
# the operator to discount the alarm. David: "retire it quickly ...
# this is really part of our tech debt payoff."
#
# Retiring it by hand is 54 systemctl invocations on a console, with
# nothing recording which, in what order, or what the database held the
# moment before. So it is an ops verb through the audited door
# (infra/ops/verbs/retire-second-stack.json), and this script is its
# whole mechanism.
#
# THE BOUNDS, in the order they are applied — each refuses loudly and
# names the unit or the bound:
#
#   1. THE MODE. Exactly one argument, `--dry-run` or `--for-real`. The
#      allowlist admits only those two literals; the script re-checks
#      rather than relying on one layer (the delete-orphan-object
#      convention).
#   2. THE LIST. infra/gcp/second-stack-units.txt, read from the checkout
#      this script lives in. Every line is `boss-<name>.timer` or
#      `boss-<name>.service` — any other shape is refused BY NAME, which
#      puts the tunnel, the journal gateway, caddy and postgres out of
#      reach by construction. A duplicate is refused.
#   3. THE KEEP SET. The host's own loops (below) are never stopped, and
#      a list that names one is refused by name — a list that asks this
#      verb to stop the door it arrives through is a list nobody
#      reviewed.
#   4. THE SNAPSHOT. `systemctl list-units --all 'boss-*'` is printed in
#      full, so the packet holds what was running before anything moved.
#      The plan is the list ∩ the units present, in the list's order;
#      a listed unit the host does not have is reported and skipped.
#   5. THE ROLE. The real run refuses while the estate registry still
#      says boss-gcp declares `legacy-stack`: boss-gcp-converge installs
#      and enables that role's timers on every tick, so a stop now would
#      be reverted within half an hour and the record would lie. Read
#      through infra/estate/node-roles.sh, the same reader the converge
#      uses. A registry that cannot be read is a bound that cannot be
#      evaluated, which is a refusal — never a pass.
#   6. CAPTURE BEFORE STOP. `pg_dump` of the stack's database — found by
#      reading BOSS_POSTGRES_URL off the stack's own unit definitions
#      (`systemctl show -p Environment`), never hardcoded here — to a
#      dated file under /var/backups/boss/second-stack/. The credential
#      is passed to pg_dump and REDACTED everywhere it is printed. If
#      the capture fails, nothing is stopped: a stopped stack whose
#      final state was not recorded is one nobody can restore.
#
# `--dry-run` runs every bound, prints the plan (`would stop+disable
# <unit>`, one per line) and rehearses the capture as a schema-only dump
# to /dev/null — it proves the credential reaches the database and
# writes nothing. `--for-real` dumps, then `systemctl disable --now`
# each planned unit one at a time, printing `stopped+disabled <unit>` as
# it goes so a run killed at the runner's timeout still leaves an exact
# record, resets the unit's failed state, and finally lists the units
# again and refuses to claim success while any planned unit is still
# active.
#
# WHAT IT DOES NOT DO. It deletes no unit file, no binary, no config,
# no database: the dump is the capture, the stop is the retirement, and
# uninstalling is the converge's business once the role is gone (car
# 3). It never expands the list (no dependents, no globs). It does not
# touch postgres itself.
#
# USAGE
#   retire-second-stack.sh --dry-run | --for-real
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the unit or the bound; nothing stopped
#   1  failed part-way — the record states what was already stopped and
#      what was not touched; or a tool this needs could not answer
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_RETIRE_UNITS_FILE   the list (default: second-stack-units.txt
#                            beside this script)
#   BOSS_RETIRE_BACKUP_DIR   where the dump lands
#                            (default: /var/backups/boss/second-stack)
#   BOSS_ESTATE_NODES_URL    see infra/estate/node-roles.sh
#   BOSS_NODE_ROLES          a pre-read role list wins (node-roles.sh)

set -uo pipefail

ME="retire-second-stack"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
LIST="${BOSS_RETIRE_UNITS_FILE:-$SELF_DIR/second-stack-units.txt}"
BACKUP_DIR="${BOSS_RETIRE_BACKUP_DIR:-/var/backups/boss/second-stack}"
NODE_ID="boss-gcp"

# THE KEEP SET — the host's own loops. The list must never name one and
# this script will never stop one, whatever the list says. Stated here
# as the enforcement and in the list's header as prose; the boss-testing
# harness pins the list against infra/estate/roles.toml as well, so the
# roles the host declares and this set cannot drift apart unnoticed.
#
#   boss-ops-runner          the door this verb arrives through
#   boss-gcp-converge        the loop that installs the host's units
#   boss-estate-observe-units  [always] in roles.toml
#   boss-estate-observe-host   role off-cluster-observer
#   boss-codebase-metrics      role off-cluster-observer (failing in the
#                              inventory; failing is not redundant)
#   boss-protocol-drift        role off-cluster-observer (19dec171)
#   boss-ml-inference-batch    role ml-batch-host — deliberate here
KEEP_STEMS=(
    boss-ops-runner
    boss-gcp-converge
    boss-estate-observe-units
    boss-estate-observe-host
    boss-codebase-metrics
    boss-protocol-drift
    boss-ml-inference-batch
)
is_kept() { # <unit>
    local stem="${1%.service}"; stem="${stem%.timer}"
    local k
    for k in "${KEEP_STEMS[@]}"; do [ "$k" = "$stem" ] && return 0; done
    return 1
}

# --- bound 1: the mode ------------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real"
    say "  --dry-run   every bound, the plan, a schema-only rehearsal of the capture; stops nothing"
    say "  --for-real  capture the database, then stop+disable exactly the listed units, in order"
    exit 2
}
[ "$#" -eq 1 ] || usage
DRY=""
case "$1" in
    --dry-run) DRY=1 ;;
    --for-real) DRY=0 ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac

# --- bound 2 + 3: the list, and the keep set ------------------------------
[ -f "$LIST" ] || refuse "the unit list $LIST is missing — there is nothing this verb is allowed to stop"
LISTED=()
lineno=0
while IFS= read -r raw || [ -n "$raw" ]; do
    lineno=$((lineno + 1))
    line="${raw%%#*}"
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%"${line##*[![:space:]]}"}"
    [ -z "$line" ] && continue
    if ! [[ "$line" =~ ^boss-[a-z0-9-]+\.(service|timer)$ ]]; then
        refuse "$LIST line $lineno names \`$line\`, which is not boss-<name>.service or boss-<name>.timer — this verb touches nothing of any other shape"
    fi
    if is_kept "$line"; then
        refuse "$LIST line $lineno names \`$line\`, which is in this verb's KEEP set (${KEEP_STEMS[*]}) — the host's own loops are never stopped by it"
    fi
    for u in "${LISTED[@]+"${LISTED[@]}"}"; do
        [ "$u" = "$line" ] && refuse "$LIST names \`$line\` twice (line $lineno) — one list, one entry each"
    done
    LISTED+=("$line")
done < "$LIST"
[ "${#LISTED[@]}" -gt 0 ] || refuse "$LIST names no units — nothing to retire, and an empty list is not a plan"
is_listed() { # <unit>
    local u
    for u in "${LISTED[@]}"; do [ "$u" = "$1" ] && return 0; done
    return 1
}
say "list: ${#LISTED[@]} units from ${LIST#"$REPO"/}; keep set: ${KEEP_STEMS[*]}"
for k in "${KEEP_STEMS[@]}"; do
    echo "keep $k.timer"
    echo "keep $k.service"
done

# --- bound 4: the snapshot, and the plan ----------------------------------
TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

snapshot() { # -> stdout: unit load active sub description
    systemctl list-units --all --no-pager --plain --no-legend -- 'boss-*'
}
if ! snapshot > "$TMP/before" 2> "$TMP/before.err"; then
    say "CANNOT ANSWER — systemctl could not list this host's boss-* units:"
    sed 's/^/    /' "$TMP/before.err" >&2
    say "  Nothing was stopped."
    exit 1
fi
echo "--- boss-* units on $NODE_ID before ($(date -u +%Y-%m-%dT%H:%M:%SZ)) ---"
cat "$TMP/before"
echo "--- end of snapshot ---"

# Present = load state is anything but not-found. `--all` lists a unit
# some other unit references but that does not exist as `not-found`.
PLAN=()
for u in "${LISTED[@]}"; do
    load=$(awk -v u="$u" '$1 == u { print $2; exit }' "$TMP/before")
    if [ -z "$load" ] || [ "$load" = "not-found" ]; then
        echo "not on this host $u"
        continue
    fi
    PLAN+=("$u")
done
say "plan: ${#PLAN[@]} of ${#LISTED[@]} listed units are present"
if [ "$DRY" = 1 ]; then
    for u in "${PLAN[@]+"${PLAN[@]}"}"; do echo "would stop+disable $u"; done
fi

# --- bound 5: the role -----------------------------------------------------
# shellcheck source=infra/estate/node-roles.sh
. "$REPO/infra/estate/node-roles.sh"
# The shared reader narrates for a converge ("installing every row");
# its note goes to the record verbatim, but the verdict here is this
# script's own, because the converge's fallback is not this verb's.
BOSS_CONVERGE_NAME="$ME" read_node_roles "$NODE_ID" > "$TMP/roles.note" 2>&1
sed 's/^/  /' "$TMP/roles.note"
# The reader's fallback for a dark registry (its last cached read, else
# the sentinel `registry-unread`) keeps a CONVERGE converging; it is not
# a reading this verb may act on. Only a live answer evaluates the
# bound — anything else is a refusal, never a pass. The source is asked
# explicitly because the sentinel is a non-empty string: an emptiness
# check alone read it as a declaration and stopped the stack under a
# dark registry, on the train gate of 2026-09-14 16:41.
if [ "${BOSS_NODE_ROLES_SOURCE:-registry}" != "registry" ]; then
    refuse "$NODE_ID's roles did not come from a live read of the estate registry (source: ${BOSS_NODE_ROLES_SOURCE}, roles: ${BOSS_NODE_ROLES:-}), so the legacy-stack bound cannot be evaluated. A bound that cannot be evaluated is not passed. Nothing was stopped."
fi
if [ -z "${BOSS_NODE_ROLES:-}" ]; then
    refuse "$NODE_ID's roles could not be read from the estate registry (or it declares none), so the legacy-stack bound cannot be evaluated. A bound that cannot be evaluated is not passed. Nothing was stopped."
fi
if has_role legacy-stack; then
    refuse "$NODE_ID still declares the role \`legacy-stack\` (roles: $BOSS_NODE_ROLES). boss-gcp-converge installs and enables that role's timers on every tick, so stopping them now would be reverted within the half hour and this record would lie. Drop the role first (d5941ef3 car 3), then ask again. Nothing was stopped."
fi
say "role: $NODE_ID declares $BOSS_NODE_ROLES — legacy-stack is not among them"

# --- bound 6: the capture --------------------------------------------------
# The database is whichever the stack's own units point at, read off
# their definitions: deploy-services.sh writes BOSS_POSTGRES_URL as an
# Environment= on the clap+env binaries (dispatcher, clock, search,
# views, brewery-sim). The first listed service that carries it wins.
PG_URL=""
PG_FROM=""
for u in "${LISTED[@]}"; do
    case "$u" in *.service) ;; *) continue ;; esac
    envline=$(systemctl show -p Environment --value "$u" 2>/dev/null) || continue
    for kv in $envline; do
        case "$kv" in
            BOSS_POSTGRES_URL=*) PG_URL="${kv#BOSS_POSTGRES_URL=}"; PG_FROM="$u" ;;
        esac
        [ -n "$PG_URL" ] && break
    done
    [ -n "$PG_URL" ] && break
done
redact() { sed -E 's#(://[^:/@]+):[^@]*@#\1:***@#'; }
if [ -z "$PG_URL" ]; then
    refuse "no listed service carries BOSS_POSTGRES_URL in its Environment (systemctl show), so the stack's database cannot be named and cannot be captured. Nothing was stopped."
fi
PG_SHOWN="$(printf '%s' "$PG_URL" | redact)"
say "database: $PG_SHOWN (from $PG_FROM's unit definition)"
command -v pg_dump >/dev/null 2>&1 \
    || refuse "pg_dump is not on PATH on this host, so the database cannot be captured. Nothing was stopped."

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DUMP="$BACKUP_DIR/second-stack-$STAMP.sql"
if [ "$DRY" = 1 ]; then
    if [ -d "$BACKUP_DIR" ]; then
        [ -w "$BACKUP_DIR" ] || refuse "$BACKUP_DIR exists but is not writable by $(id -un), so the capture could not land there"
    else
        parent="$(dirname "$BACKUP_DIR")"
        while [ ! -d "$parent" ] && [ "$parent" != "/" ]; do parent="$(dirname "$parent")"; done
        [ -w "$parent" ] || refuse "$BACKUP_DIR does not exist and $parent is not writable by $(id -un), so the capture could not land there"
    fi
    say "rehearsing the capture: pg_dump --schema-only to /dev/null (writes nothing)"
    if ! pg_dump --no-password --schema-only --dbname="$PG_URL" --file=/dev/null 2> "$TMP/pg.err"; then
        say "REFUSED — the capture rehearsal failed against $PG_SHOWN; the real run would refuse here too:"
        redact < "$TMP/pg.err" | sed 's/^/    /' >&2
        exit 2
    fi
    say "DRY RUN — would capture $PG_SHOWN to $DUMP, then stop+disable ${#PLAN[@]} units in the order above. Every bound passed; nothing was stopped."
    exit 0
fi

if ! mkdir -p "$BACKUP_DIR" 2> "$TMP/mk.err"; then
    say "REFUSED — cannot create $BACKUP_DIR for the capture:"
    sed 's/^/    /' "$TMP/mk.err" >&2
    say "  nothing was stopped."
    exit 2
fi
say "capturing $PG_SHOWN to $DUMP"
if ! pg_dump --no-password --dbname="$PG_URL" --file="$DUMP" 2> "$TMP/pg.err"; then
    say "REFUSED — the capture failed against $PG_SHOWN, so nothing was stopped:"
    redact < "$TMP/pg.err" | sed 's/^/    /' >&2
    rm -f "$DUMP"
    say "  a stopped stack whose final state was not recorded is one nobody can restore."
    exit 2
fi
if [ ! -s "$DUMP" ]; then
    rm -f "$DUMP"
    refuse "the capture produced an empty file at $DUMP, so nothing was stopped"
fi
echo "captured $DUMP ($(wc -c < "$DUMP") bytes, sha256 $(sha256sum "$DUMP" | cut -c1-64))"

# --- the retirement, one unit at a time, in order --------------------------
DONE=()
for u in "${PLAN[@]+"${PLAN[@]}"}"; do
    # The internal guard: no path through this loop stops a unit the list
    # does not name or the keep set does — even if a future edit widens
    # PLAN, this line refuses.
    if ! is_listed "$u" || is_kept "$u"; then
        say "REFUSED — \`$u\` is not in the list or is kept; the loop was handed a unit it must not stop."
        say "  already stopped+disabled: ${DONE[*]:-none}"
        exit 2
    fi
    if ! systemctl disable --now -- "$u" > "$TMP/stop.out" 2>&1; then
        cat "$TMP/stop.out" >&2
        say "FAILED at \`$u\` — systemctl disable --now exited non-zero."
        say "  already stopped+disabled (${#DONE[@]}): ${DONE[*]:-none}"
        remaining=()
        after=0
        for r in "${PLAN[@]}"; do
            [ "$after" = 1 ] && remaining+=("$r")
            [ "$r" = "$u" ] && after=1
        done
        say "  not touched (${#remaining[@]}): ${remaining[*]:-none}"
        say "  the capture at $DUMP stands. Fix the unit and ask again; the units already stopped stay stopped."
        exit 1
    fi
    systemctl reset-failed -- "$u" >/dev/null 2>&1 || true
    DONE+=("$u")
    echo "stopped+disabled $u"
done

# --- verified stopped, because exit 0 and stopped are different claims ----
if ! snapshot > "$TMP/after" 2>/dev/null; then
    say "FAILED — stopped+disabled ${#DONE[@]} units but systemctl could not list them afterwards to verify"
    exit 1
fi
echo "--- boss-* units on $NODE_ID after ($(date -u +%Y-%m-%dT%H:%M:%SZ)) ---"
cat "$TMP/after"
echo "--- end of snapshot ---"
still=()
for u in "${DONE[@]+"${DONE[@]}"}"; do
    active=$(awk -v u="$u" '$1 == u { print $3; exit }' "$TMP/after")
    case "$active" in
        active|activating|reloading) still+=("$u") ;;
    esac
done
if [ "${#still[@]}" -gt 0 ]; then
    say "FAILED — disable --now returned success and ${#still[@]} unit(s) are STILL active: ${still[*]}"
    say "  something is starting them again (a timer not in the list? the converge?); this verb will not try again."
    exit 1
fi
say "OK — stopped+disabled ${#DONE[@]} units of the second stack on $NODE_ID, in the order above; captured first at $DUMP. Unit files, binaries and the database are untouched."
exit 0
