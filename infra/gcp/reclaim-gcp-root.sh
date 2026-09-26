#!/usr/bin/env bash
#
# reclaim-gcp-root — free boss-gcp's 48 GB root by removing exactly two
# kinds of thing, and nothing else, ever: the obsolete binary backup
# directories /opt/boss-binbak-* and /opt/boss-dev-bak, and the systemd
# journal beyond a fixed 1G.
#
# WHY IT EXISTS (backlog d3c7eada car 2, 2026-09-26)
# ---------------------------------------------------------------------
# boss-gcp's root sat at 11-13 GB free for nine days against the
# estate's 17 GB floor for a 47 GB disk, and estate.alarm filed it as
# `disk_tight:boss-gcp`. Car 1 let disk-report measure the host; its
# reading (ops-request b21ddeb3, 2026-09-26) found four July 2026
# pre-deploy binary backups under /opt — boss-binbak-pre-pr73-0702-1801
# (600M), boss-binbak-pre-latest-194355 (586M),
# boss-binbak-pre-pr5-20260701-032322 (558M) and boss-dev-bak (536M),
# ~2.3 GB written by the bare-metal deploy that was deleted on
# 2026-09-18 — and a 4.1 GB journal. Together about 5 GB, which is what
# the floor needs. Everything else the reading named is DATA and David's
# call (retention policy), so it is out of this verb's reach by
# construction: /var/backups/* (the cluster-pg dumps and the second-
# stack capture), every home directory, /usr/local, and the live
# /opt/boss and /opt/boss-cli.
#
# THE BOUNDS, in the order they are applied — each refuses loudly and
# names the path or the bound; a refusal removes NOTHING:
#
#   1. THE MODE. Exactly one argument, `--dry-run` or `--for-real`. The
#      allowlist admits only those two literals and no second word; the
#      script re-checks rather than relying on one layer, so a path
#      handed to it is refused before anything is looked at.
#   2. THE NAMES. Candidates are what two globs match under /opt —
#      `boss-binbak-*` and the one name `boss-dev-bak` — and each must
#      be a REAL directory whose realpath is /opt/<its own name>. A
#      symlink is refused (this verb never removes what a link points
#      at), and so is any name outside ^boss-binbak-[A-Za-z0-9._-]+$.
#      The paths are the script's, never a param: no packet can name one.
#   3. THE AGE. Nothing inside a candidate may be younger than 30 days.
#      The backups are from July; a fresh one would be somebody's
#      rollback target, and this verb does not guess whose.
#   4. NOT LIVE. A candidate is refused when it is, contains or sits
#      inside what /opt/boss or /opt/boss-cli resolves to; when a symlink
#      in /opt, /usr/local/bin, /usr/bin, /usr/sbin or
#      /etc/systemd/system resolves into it; when any running process's
#      executable lives in it (/proc/*/exe); or when any loaded service
#      unit's ExecStart/ExecStartPre/WorkingDirectory/FragmentPath
#      resolves into it. A bound that cannot be evaluated (systemctl
#      cannot list units) is a refusal, never a pass.
#   5. THE JOURNAL. `journalctl --vacuum-size=1G` — a fixed bound, not a
#      param, so no packet can ask for 0 and throw away the host's
#      diagnosis. The newest 1G stays.
#
# `--dry-run` runs every bound above, prints the plan (`would remove
# <dir> (<N> MiB)`, the journal's current usage and `would vacuum`) and
# the root's free space, and removes nothing. `--for-real` removes each
# planned directory with `rm -rf --one-file-system`, one at a time,
# printing `removed <dir>` as it goes so a run killed at the runner's
# timeout still leaves an exact record, verifies each is gone, then
# vacuums the journal and prints the root's free space before and after.
#
# WHAT IT DOES NOT SEE. It reads units and processes, not every script
# on the host: a cron line or a shell script naming a backup directory
# by path would not stop it. The deploy that wrote these directories was
# deleted on 2026-09-18 and nothing in the tree names them.
#
# USAGE
#   reclaim-gcp-root.sh --dry-run | --for-real
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the path or the bound; nothing removed
#   1  failed part-way — the record states what was already removed and
#      what was not touched; or a tool this needs could not answer
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_RECLAIM_OPT_DIR     where the candidates and the live trees live
#                            (default /opt)
#   BOSS_RECLAIM_PROC_DIR    the process table (default /proc)
#   BOSS_RECLAIM_LINK_DIRS   space-separated directories scanned for
#                            symlinks into a candidate

set -uo pipefail

ME="reclaim-gcp-root"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; say "  nothing was removed."; exit 2; }

OPT="${BOSS_RECLAIM_OPT_DIR:-/opt}"
PROC="${BOSS_RECLAIM_PROC_DIR:-/proc}"
JOURNAL_KEEP="1G"
MIN_AGE_DAYS=30
NAME_RE='^boss-binbak-[A-Za-z0-9._-]+$'

# --- bound 1: the mode ------------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real"
    say "  --dry-run   every bound, the plan and the root's free space; removes nothing"
    say "  --for-real  remove $OPT/boss-binbak-* and $OPT/boss-dev-bak, then vacuum the journal to $JOURNAL_KEEP"
    say "  this verb takes no path: what it may remove is fixed in the script"
    exit 2
}
[ "$#" -eq 1 ] || usage
case "$1" in
    --dry-run) DRY=1 ;;
    --for-real) DRY=0 ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

under() { # <path> <dir> — path is dir or inside it
    [ "$1" = "$2" ] || [[ "$1" == "$2"/* ]]
}

OPT_REAL=$(realpath -e -- "$OPT" 2>/dev/null) \
    || refuse "$OPT does not resolve on this host"
LINK_DIRS="${BOSS_RECLAIM_LINK_DIRS:-$OPT_REAL /usr/local/bin /usr/bin /usr/sbin /etc/systemd/system}"
free_mib() { df -Pm -- "$OPT_REAL" 2>/dev/null | awk 'NR == 2 { print $4 }'; }
FREE_BEFORE=$(free_mib)
say "root free before: ${FREE_BEFORE:-unknown} MiB (df $OPT_REAL)"

# --- bound 2 + 3: the names, and the age ----------------------------------
CANDS=()
now=$(date +%s)
cutoff=$((now - MIN_AGE_DAYS * 86400))
for d in "$OPT"/boss-binbak-* "$OPT"/boss-dev-bak; do
    [ -e "$d" ] || [ -L "$d" ] || continue
    name="${d##*/}"
    if [ "$name" != "boss-dev-bak" ] && ! [[ "$name" =~ $NAME_RE ]]; then
        refuse "\`$d\` matched the glob but its name is not ^boss-binbak-[A-Za-z0-9._-]+\$ — a name this verb was not reviewed to remove"
    fi
    if [ -L "$d" ]; then
        refuse "\`$d\` is a symlink (to $(readlink -- "$d")) — this verb removes only real directories under $OPT, never what a link points at"
    fi
    [ -d "$d" ] || refuse "\`$d\` is not a directory — this verb removes backup directories only"
    real=$(realpath -e -- "$d") || refuse "\`$d\` does not resolve"
    [ "$real" = "$OPT_REAL/$name" ] \
        || refuse "\`$d\` resolves to $real, not $OPT_REAL/$name — it is not the directory its name says"
    young=$(find "$real" -xdev -newermt "@$cutoff" -print -quit 2>/dev/null)
    if [ -n "$young" ]; then
        refuse "\`$real\` holds \`$young\`, modified in the last $MIN_AGE_DAYS days — a fresh backup may be somebody's rollback target, and this verb removes only old ones"
    fi
    CANDS+=("$real")
done

# --- bound 4: not live ---------------------------------------------------
# (a) the live trees themselves.
for live in "$OPT/boss" "$OPT/boss-cli"; do
    [ -e "$live" ] || continue
    lr=$(realpath -e -- "$live") || refuse "$live does not resolve, so whether it points into a backup cannot be judged"
    for c in "${CANDS[@]+"${CANDS[@]}"}"; do
        if under "$lr" "$c" || under "$c" "$lr"; then
            refuse "$live resolves to $lr, which is \`$c\` or overlaps it — that backup is LIVE"
        fi
    done
done
# (b) symlinks elsewhere that resolve into a candidate.
for ld in $LINK_DIRS; do
    [ -d "$ld" ] || continue
    while IFS= read -r -d '' link; do
        skip=0
        for c in "${CANDS[@]+"${CANDS[@]}"}"; do under "$link" "$c" && skip=1; done
        [ "$skip" = 1 ] && continue
        t=$(realpath -m -- "$link" 2>/dev/null) || continue
        for c in "${CANDS[@]+"${CANDS[@]}"}"; do
            under "$t" "$c" && refuse "the symlink $link resolves to $t, inside \`$c\` — that backup is still linked to"
        done
    done < <(find "$ld" -maxdepth 2 -type l -print0 2>/dev/null)
done
# (c) running processes.
for exe in "$PROC"/[0-9]*/exe; do
    t=$(readlink -- "$exe" 2>/dev/null) || continue
    t="${t% (deleted)}"
    for c in "${CANDS[@]+"${CANDS[@]}"}"; do
        if under "$t" "$c"; then
            pid="${exe%/exe}"; pid="${pid##*/}"
            refuse "process $pid ($(cat "$PROC/$pid/comm" 2>/dev/null || echo '?')) runs $t, inside \`$c\` — that backup is LIVE"
        fi
    done
done
# (d) loaded service units.
if [ "${#CANDS[@]}" -gt 0 ]; then
    if ! systemctl list-units --type=service --all --no-pager --plain --no-legend > "$TMP/units" 2> "$TMP/units.err"; then
        say "REFUSED — systemctl could not list this host's service units, so whether a backup is a unit's binary cannot be judged:"
        sed 's/^/    /' "$TMP/units.err" >&2
        say "  a bound that cannot be evaluated is not passed. Nothing was removed."
        exit 2
    fi
    while read -r unit _; do
        [ -n "$unit" ] || continue
        props=$(systemctl show -p ExecStart -p ExecStartPre -p WorkingDirectory -p FragmentPath --value -- "$unit" 2>/dev/null) || continue
        for p in $(printf '%s\n' "$props" | grep -oE '/[^ ;]+'); do
            t=$(realpath -m -- "$p" 2>/dev/null) || continue
            for c in "${CANDS[@]}"; do
                under "$t" "$c" && refuse "the unit $unit names $p (resolving to $t), inside \`$c\` — that backup is LIVE"
            done
        done
    done < "$TMP/units"
fi

# --- the plan --------------------------------------------------------------
TOTAL=0
for c in "${CANDS[@]+"${CANDS[@]}"}"; do
    mib=$(du -sxm -- "$c" 2>/dev/null | awk '{ print $1 }')
    TOTAL=$((TOTAL + ${mib:-0}))
    echo "$c ${mib:-?} MiB"
done > "$TMP/plan"
if [ "${#CANDS[@]}" -eq 0 ]; then
    say "no $OPT/boss-binbak-* or $OPT/boss-dev-bak on this host — no backup directory to remove"
fi
command -v journalctl >/dev/null 2>&1 \
    || refuse "journalctl is not on PATH, so the journal bound cannot be applied"
JOURNAL_USAGE=$(journalctl --disk-usage 2>&1)
say "journal: $JOURNAL_USAGE"

if [ "$DRY" = 1 ]; then
    while read -r c mib _; do echo "would remove $c ($mib MiB)"; done < "$TMP/plan"
    echo "would vacuum the journal to $JOURNAL_KEEP (journalctl --vacuum-size=$JOURNAL_KEEP)"
    say "DRY RUN — every bound passed: ${#CANDS[@]} backup directories (${TOTAL} MiB) and the journal beyond $JOURNAL_KEEP would go. Nothing was removed."
    exit 0
fi

# --- the removal, one directory at a time ---------------------------------
DONE=()
while read -r c mib _; do
    # The internal guard: no path through this loop removes anything but
    # a real /opt/boss-binbak-* or /opt/boss-dev-bak directory — even if
    # a future edit widens the plan, this refuses.
    name="${c##*/}"
    if [ "${c%/*}" != "$OPT_REAL" ] || [ -L "$c" ] || ! [ -d "$c" ] \
        || { [ "$name" != "boss-dev-bak" ] && ! [[ "$name" =~ $NAME_RE ]]; }; then
        say "REFUSED — \`$c\` is not a backup directory this verb may remove; the loop was handed a path it must not touch."
        say "  already removed: ${DONE[*]:-none}"
        exit 2
    fi
    if ! rm -rf --one-file-system -- "$c" 2> "$TMP/rm.err" || [ -e "$c" ]; then
        sed 's/^/    /' "$TMP/rm.err" >&2
        say "FAILED at \`$c\` — rm did not remove it."
        say "  already removed (${#DONE[@]}): ${DONE[*]:-none}"
        say "  the journal was not vacuumed."
        exit 1
    fi
    DONE+=("$c")
    echo "removed $c ($mib MiB)"
done < "$TMP/plan"

if ! journalctl --vacuum-size="$JOURNAL_KEEP" > "$TMP/vacuum" 2>&1; then
    sed 's/^/    /' "$TMP/vacuum" >&2
    say "FAILED — journalctl --vacuum-size=$JOURNAL_KEEP exited non-zero; the ${#DONE[@]} backup directories above are removed."
    exit 1
fi
sed 's/^/  /' "$TMP/vacuum"
echo "vacuumed the journal to $JOURNAL_KEEP: $(journalctl --disk-usage 2>&1)"
FREE_AFTER=$(free_mib)
say "OK — removed ${#DONE[@]} backup directories (${TOTAL} MiB) and vacuumed the journal to $JOURNAL_KEEP; root free ${FREE_BEFORE:-unknown} → ${FREE_AFTER:-unknown} MiB. /var/backups, homes, /usr/local, $OPT/boss and $OPT/boss-cli are untouched."
exit 0
