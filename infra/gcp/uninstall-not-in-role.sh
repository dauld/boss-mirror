#!/usr/bin/env bash
#
# uninstall-not-in-role — remove from boss-gcp the unit pairs its
# declared roles do not name: exactly the set the installer itself
# reports as NOT IN ROLE, derived at call time, and nothing else, ever.
#
# WHY IT EXISTS (design 9e3e093f, decided by David 2026-09-11; backlog
# d5941ef3 car 4)
# ---------------------------------------------------------------------
# A host declares its roles in the estate registry and derives its unit
# roster from them; `install-units.sh units` installs the rows the
# roles name and REPORTS every other row as NOT IN ROLE — reported,
# never removed, because removing is destructive and belongs behind the
# audited door. Car 2 stopped the second stack (ops-request 7912c9ae,
# 2026-09-15 21:11Z: 52 units stopped+disabled, unit FILES untouched)
# and car 3 dropped `legacy-stack` from the host's roles, so since then
# boss-gcp-converge has said, every half hour, "installed 6 of 16 timer
# unit pair(s), not in role 10" with the same ten names under
# anomalies. The files are still on disk. A unit installed and never
# serving is worse than absent: it reports failed forever and trains
# the operator to discount the alarm. The item's own proposal: "an
# uninstall path for units the role does not name … through the
# ops-runner door as a bounded verb" (infra/ops/verbs/uninstall-not-in-
# role.json). This script is its whole mechanism.
#
# THE BOUNDS, in the order they are applied — each refuses loudly and
# names the unit or the bound:
#
#   1. THE MODE. Exactly one argument, `--dry-run` or `--for-real`. The
#      allowlist admits only those two literals; the script re-checks
#      rather than relying on one layer.
#   2. THE SNAPSHOT. `systemctl list-unit-files 'boss-*'` and
#      `list-units --all 'boss-*'` are printed in full, so the packet
#      holds what was on the host before anything moved.
#   3. THE ROLES, read LIVE. infra/estate/node-roles.sh — the same
#      reader the converge uses — must answer from the registry NOW.
#      Its fallbacks (a cached declaration, the `registry-unread`
#      sentinel) keep a converge converging and are refused here: a
#      bound that cannot be evaluated is not passed. A host declaring
#      NO roles is refused too — the installer reads "no roles" as
#      "every row", and every row is not a set with an outside.
#   4. THE SET, derived — never listed. `install-units.sh roster`,
#      run from the checkout this script lives in with the roles just
#      read, prints one line per roles.toml row: `in-role <stem>` or
#      `not-in-role <stem>`. That is the installer's OWN roster_for_roles
#      — the one that decides what `units` installs and enables — so
#      what this verb may remove is the complement of what the converge
#      keeps, by construction, and a role added or dropped in the
#      registry moves both at once (CLAUDE.md §9a). Every in-role stem
#      is printed as `keep`. A not-in-role stem outside the shape
#      `boss-[a-z0-9-]+` is refused by name; an EMPTY set is refused:
#      "nothing to uninstall" is a verdict, not an OK.
#   5. THE PLAN. For each not-in-role stem, its `.timer` then its
#      `.service` (a service removed while its timer still waits is
#      started once more at the next elapse). A unit is PRESENT when
#      its file is under /etc/systemd/system — the retire's after-
#      snapshot showed the disabled units GONE from `list-units --all`
#      (systemd forgets an inactive unit nobody references) with their
#      files still on disk, so the file is the fact, the listing is
#      corroboration. A unit that is neither is reported `not on this
#      host` and skipped: the second run of this verb is that for every
#      unit, and a converged host is the goal, not a refusal.
#
# `--dry-run` runs every bound and prints the plan (`would disable+remove
# <unit>`, one per line) — no systemctl call that acts, no file touched.
# `--for-real` then, per planned unit in order: `systemctl disable --now`
# (stops it, drops its symlinks), removes the unit file, and prints
# `disabled+removed <unit>` as it lands so a run killed at the runner's
# timeout still leaves an exact record; per stem it also removes the
# installer's own drop-in (`<stem>.service.d/jobs-url.conf`, and the
# directory when that leaves it empty — a drop-in somebody else wrote
# stays, and is named). Then ONE daemon-reload, reset-failed on each
# removed unit, a second snapshot, and success is refused while any
# planned unit still has a file or is still loaded.
#
# WHAT IT DOES NOT DO. It never widens the set: no globs, no dependents,
# nothing outside roles.toml − roster — so `boss-ops-runner` (installed by
# infra/ops/install-ops-runner.sh, not a roles.toml row), the retired
# daemons of the second stack, WireGuard, caddy and postgres
# are unreachable by construction. It stops nothing that is in role,
# whatever the file system says. It captures nothing: these are unit
# FILES the tree still carries, reinstalled by one converge should a
# role come back; the database the retire verb captured is untouched.
#
# USAGE
#   uninstall-not-in-role.sh --dry-run | --for-real
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the unit or the bound; nothing removed
#   1  failed part-way — the record states what was already removed and
#      what was not touched; or a tool this needs could not answer
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   INSTALL_ETC              where the installer put the units (default
#                            /etc/systemd/system — the installer's own
#                            seam, same name, same default)
#   BOSS_REPO_ROOT           the checkout install-units.sh derives from
#                            (default: the one this script lives in)
#   BOSS_ESTATE_NODES_URL    see infra/estate/node-roles.sh
#   BOSS_NODE_ROLES          a pre-read role list wins (node-roles.sh) —
#                            and is refused here, because it is not live

set -uo pipefail

ME="uninstall-not-in-role"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
ETC="${INSTALL_ETC:-/etc/systemd/system}"
NODE_ID="boss-gcp"
INSTALLER="$REPO/infra/gcp/install-units.sh"

# --- bound 1: the mode ------------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real"
    say "  --dry-run   every bound and the plan; removes nothing"
    say "  --for-real  disable, then remove the unit files of exactly the not-in-role set, in order"
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

# --- bound 2: the snapshot --------------------------------------------------
snapshot() { # -> stdout: the unit files, then the loaded units
    echo "--- boss-* unit files on $NODE_ID ($1, $(date -u +%Y-%m-%dT%H:%M:%SZ)) ---"
    systemctl list-unit-files --no-pager --plain --no-legend -- 'boss-*' || return 1
    echo "--- boss-* units on $NODE_ID ($1) ---"
    systemctl list-units --all --no-pager --plain --no-legend -- 'boss-*' || return 1
    echo "--- end of snapshot ---"
}
if ! snapshot before > "$TMP/before" 2> "$TMP/before.err"; then
    say "CANNOT ANSWER — systemctl could not list this host's boss-* units:"
    sed 's/^/    /' "$TMP/before.err" >&2
    say "  Nothing was removed."
    exit 1
fi
cat "$TMP/before"
# Loaded units, by name -> load state, for the plan and the check.
loaded_state() { # <unit> -> load state or empty
    awk -v u="$1" 'seen && $1 == u { print $2; exit } /^--- boss-\* units/ { seen = 1 }' "$TMP/before"
}

# --- bound 3: the roles, live -----------------------------------------------
# shellcheck source=infra/estate/node-roles.sh
. "$REPO/infra/estate/node-roles.sh"
BOSS_CONVERGE_NAME="$ME" read_node_roles "$NODE_ID" > "$TMP/roles.note" 2>&1
sed 's/^/  /' "$TMP/roles.note"
# The reader's fallbacks for a dark registry — a cached read, else the
# non-empty sentinel `registry-unread` — keep a CONVERGE converging;
# neither is a reading this verb may act on (the retire verb learned
# this on the train gate of 2026-09-14 16:41, under a dark registry).
if [ "${BOSS_NODE_ROLES_SOURCE:-registry}" != "registry" ]; then
    refuse "$NODE_ID's roles did not come from a live read of the estate registry (source: ${BOSS_NODE_ROLES_SOURCE}, roles: ${BOSS_NODE_ROLES:-}), so the set this verb may remove cannot be derived. A bound that cannot be evaluated is not passed. Nothing was removed."
fi
if [ -z "${BOSS_NODE_ROLES:-}" ]; then
    refuse "$NODE_ID declares no roles in the estate registry (or it could not be read). The installer reads no roles as EVERY row, which leaves nothing outside the roster to remove — and a host that says nothing about what it is for is not one to uninstall from. Nothing was removed."
fi
say "roles: $NODE_ID declares $BOSS_NODE_ROLES (live)"

# --- bound 4: the set, derived by the installer ------------------------------
[ -f "$INSTALLER" ] || { say "CANNOT ANSWER — $INSTALLER is missing, and it is the only derivation of this host's roster"; exit 1; }
if ! BOSS_REPO_ROOT="${BOSS_REPO_ROOT:-$REPO}" BOSS_NODE_ROLES="$BOSS_NODE_ROLES" \
        bash "$INSTALLER" roster > "$TMP/roster" 2> "$TMP/roster.err"; then
    say "CANNOT ANSWER — install-units.sh roster exited non-zero, so the set cannot be derived:"
    sed 's/^/    /' "$TMP/roster.err" >&2
    say "  Nothing was removed."
    exit 1
fi
KEEP=()
NOT_IN_ROLE=()
while IFS= read -r line; do
    case "$line" in
        "in-role "*) KEEP+=("${line#in-role }") ;;
        "not-in-role "*)
            stem="${line#not-in-role }"
            if ! [[ "$stem" =~ ^boss-[a-z0-9-]+$ ]]; then
                refuse "the roster names \`$stem\` as not in role, which is not a boss-<name> stem — this verb touches nothing of any other shape"
            fi
            NOT_IN_ROLE+=("$stem")
            ;;
        # Anything else the installer prints (a warning, a refusal)
        # is not a roster line.
        *) ;;
    esac
done < "$TMP/roster"
if [ "${#KEEP[@]}" -eq 0 ] && [ "${#NOT_IN_ROLE[@]}" -eq 0 ]; then
    say "CANNOT ANSWER — install-units.sh roster printed no in-role/not-in-role line:"
    sed 's/^/    /' "$TMP/roster" >&2
    sed 's/^/    /' "$TMP/roster.err" >&2
    exit 1
fi
for k in "${KEEP[@]+"${KEEP[@]}"}"; do echo "keep $k"; done
[ "${#NOT_IN_ROLE[@]}" -gt 0 ] \
    || refuse "every roles.toml row is in role on $NODE_ID (roles: $BOSS_NODE_ROLES; keep: ${KEEP[*]}) — there is nothing to uninstall, and an empty set is a verdict, not an OK. Nothing was removed."
say "set: ${#NOT_IN_ROLE[@]} stem(s) not in role (${NOT_IN_ROLE[*]}); keep: ${#KEEP[@]} (${KEEP[*]})"
is_kept() { # <unit>
    local stem="${1%.service}"; stem="${stem%.timer}"
    local k
    for k in "${KEEP[@]+"${KEEP[@]}"}"; do [ "$k" = "$stem" ] && return 0; done
    # The door and the converge are never in this set, roles.toml row or not.
    case "$stem" in boss-ops-runner|boss-gcp-converge) return 0 ;; esac
    return 1
}
in_set() { # <unit>
    local stem="${1%.service}"; stem="${stem%.timer}"
    local s
    for s in "${NOT_IN_ROLE[@]}"; do [ "$s" = "$stem" ] && return 0; done
    return 1
}

# --- bound 5: the plan ------------------------------------------------------
PLAN=()
for stem in "${NOT_IN_ROLE[@]}"; do
    for u in "$stem.timer" "$stem.service"; do
        load="$(loaded_state "$u")"
        if [ -f "$ETC/$u" ] || { [ -n "$load" ] && [ "$load" != "not-found" ]; }; then
            PLAN+=("$u")
        else
            echo "not on this host $u"
        fi
    done
done
say "plan: ${#PLAN[@]} unit(s) of ${#NOT_IN_ROLE[@]} not-in-role stem(s) present on $NODE_ID"
if [ "$DRY" = 1 ]; then
    for u in "${PLAN[@]+"${PLAN[@]}"}"; do echo "would disable+remove $u"; done
    say "DRY RUN — would disable+remove ${#PLAN[@]} unit(s) in the order above, then daemon-reload. Every bound passed; nothing was removed."
    exit 0
fi
if [ "${#PLAN[@]}" -gt 0 ] && [ ! -w "$ETC" ]; then
    refuse "$ETC is not writable by $(id -un), so no unit file could be removed"
fi

# --- the removal, one unit at a time, in order -------------------------------
DONE=()
fail_at() { # <unit> <why>
    say "FAILED at \`$1\` — $2"
    say "  already disabled+removed (${#DONE[@]}): ${DONE[*]:-none}"
    local remaining=() after=0 r
    for r in "${PLAN[@]}"; do
        [ "$after" = 1 ] && remaining+=("$r")
        [ "$r" = "$1" ] && after=1
    done
    say "  not touched (${#remaining[@]}): ${remaining[*]:-none}"
    say "  no daemon-reload was run. Fix the unit and ask again; the units already removed stay removed."
    exit 1
}
for u in "${PLAN[@]+"${PLAN[@]}"}"; do
    # The internal guard: no path through this loop removes a unit the
    # derivation did not name or the roles keep — even if a future edit
    # widens PLAN, this line refuses.
    if ! in_set "$u" || is_kept "$u"; then
        say "REFUSED — \`$u\` is not in the not-in-role set or is kept; the loop was handed a unit it must not remove."
        say "  already disabled+removed: ${DONE[*]:-none}"
        exit 2
    fi
    if [ -f "$ETC/$u" ]; then
        if ! systemctl disable --now -- "$u" > "$TMP/disable.out" 2>&1; then
            cat "$TMP/disable.out" >&2
            fail_at "$u" "systemctl disable --now exited non-zero; its file is still at $ETC/$u."
        fi
        if ! rm -f -- "$ETC/$u" 2> "$TMP/rm.err"; then
            cat "$TMP/rm.err" >&2
            fail_at "$u" "disabled, but its file could not be removed from $ETC."
        fi
        echo "disabled+removed $u"
    else
        # Loaded with no file under $ETC: a leftover systemd still holds
        # from elsewhere. disable --now drops it; there is no file of
        # ours to remove, and the after-snapshot must show it gone.
        if ! systemctl disable --now -- "$u" > "$TMP/disable.out" 2>&1; then
            cat "$TMP/disable.out" >&2
            fail_at "$u" "systemctl disable --now exited non-zero (no file under $ETC)."
        fi
        echo "disabled+removed $u (no file under $ETC)"
    fi
    DONE+=("$u")
    case "$u" in
        *.service)
            d="$ETC/$u.d"
            if [ -f "$d/jobs-url.conf" ]; then
                rm -f -- "$d/jobs-url.conf" && echo "removed $d/jobs-url.conf"
            fi
            if [ -d "$d" ]; then
                if rmdir -- "$d" 2>/dev/null; then
                    echo "removed $d/"
                else
                    echo "left $d/ — it holds a drop-in this verb did not write: $(ls -A "$d" | tr '\n' ' ')"
                fi
            fi
            ;;
    esac
done

if [ "${#DONE[@]}" -gt 0 ]; then
    if ! systemctl daemon-reload > "$TMP/reload.out" 2>&1; then
        cat "$TMP/reload.out" >&2
        say "FAILED — removed ${#DONE[@]} unit file(s) but daemon-reload exited non-zero; systemd may still hold them until it is run."
        exit 1
    fi
    echo "daemon-reload"
    for u in "${DONE[@]}"; do systemctl reset-failed -- "$u" >/dev/null 2>&1 || true; done
fi

# --- verified gone, because exit 0 and gone are different claims ----------
if ! snapshot after > "$TMP/after" 2>/dev/null; then
    say "FAILED — removed ${#DONE[@]} unit(s) but systemctl could not list them afterwards to verify"
    exit 1
fi
cat "$TMP/after"
still=()
for u in "${DONE[@]+"${DONE[@]}"}"; do
    [ -e "$ETC/$u" ] && still+=("$u (file)")
    load=$(awk -v u="$u" 'seen && $1 == u { print $2; exit } /^--- boss-\* units/ { seen = 1 }' "$TMP/after")
    if [ -n "$load" ] && [ "$load" != "not-found" ]; then still+=("$u (loaded: $load)"); fi
done
if [ "${#still[@]}" -gt 0 ]; then
    say "FAILED — the removal returned success and ${#still[@]} unit(s) are STILL present: ${still[*]}"
    say "  something put them back or systemd still holds them; this verb will not try again."
    exit 1
fi
say "OK — disabled+removed ${#DONE[@]} unit(s) not in role on $NODE_ID, in the order above (roles: $BOSS_NODE_ROLES; kept: ${KEEP[*]}). The tree still carries every unit file; a role declared again reinstalls them on the next converge."
exit 0
