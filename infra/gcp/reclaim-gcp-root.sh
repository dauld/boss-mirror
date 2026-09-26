#!/usr/bin/env bash
#
# reclaim-gcp-root — free boss-gcp's 48 GB root by removing exactly two
# kinds of thing, and nothing else, ever: the obsolete binary backup
# directories /opt/boss-binbak-* and /opt/boss-dev-bak, and the systemd
# journal beyond a fixed 1G — through a rendered plan a passkey signs.
#
#   reclaim-gcp-root.sh --dry-run          render the plan; removes nothing
#   reclaim-gcp-root.sh <plan-sha256>      remove what the SIGNED plan names
#
# WHY IT EXISTS (backlog d3c7eada car 2, 2026-09-26)
# ---------------------------------------------------------------------
# boss-gcp's root sat at 11-13 GB free for nine days against the
# estate's 17 GB floor for a 47 GB disk, and estate.alarm filed it as
# `disk_tight:boss-gcp`. Car 1 let disk-report measure the host; its
# reading (ops-request b21ddeb3, 2026-09-26) found four hand-made
# pre-deploy binary backups from July 2026 under /opt —
# boss-binbak-pre-pr73-0702-1801 (600M), boss-binbak-pre-latest-194355
# (586M), boss-binbak-pre-pr5-20260701-032322 (558M) and boss-dev-bak
# (536M), ~2.3 GB that no commit in the tree names — and a 4.1 GB
# journal. Everything else the reading named is DATA and David's call
# (retention policy), so it is out of this verb's reach by construction:
# /var/backups/* (the cluster-pg dumps and the second-stack capture),
# every home directory, /usr/local, and the live /opt/boss and
# /opt/boss-cli.
#
# THE LASTING GAIN IS THE ~2.3 GB OF BACKUPS. The journal has no
# SystemMaxUse drop-in on this host, so journald grows it back toward
# its default ceiling (~4 GB) after the vacuum; the vacuum buys time,
# not space. A SystemMaxUse bound is the converge's business, not this
# verb's (adversarial review of car 2, S7).
#
# THE APPROVAL (design 17835005's shape; reap-terminated-pods is the
# worked example). `plan-a-gcp-root-reclaim` runs `--dry-run`, which
# prints the plan on stdout and `plan-sha256:` on stderr; the ops
# runner writes it onto the request's approve step, David's passkey
# signs those bytes, and only then does the runner hand this script the
# signed hash. The write re-renders the plan and removes nothing unless
# today's plan hashes to it — so a backup that changed, appeared or went
# since the signature voids the approval, and a second run of an
# applied plan (its backups gone) is a refusal: at most once.
#
# THE BOUNDS, in the order they are applied — each refuses loudly and
# names the path or the bound; a refusal removes NOTHING, and a bound
# that cannot be EVALUATED (a read that fails) is a refusal, never a
# pass:
#
#   1. THE ARGUMENT. Exactly one: `--dry-run`, or a 64-hex plan hash.
#      Anything else — a path included — is refused before anything is
#      looked at.
#   2. THE NAMES. Candidates are what two globs match under /opt —
#      `boss-binbak-*` and the one name `boss-dev-bak` — and each must
#      be a REAL directory whose realpath is /opt/<its own name>. A
#      symlink is refused (this verb never removes what a link points
#      at), and so is any name outside ^boss-binbak-[A-Za-z0-9._-]+$.
#      The paths are the script's, never a param: no packet can name one.
#   3. NOT A CHECKOUT. A candidate holding a `.git` anywhere is refused:
#      boss-dev-bak may be a dev checkout with unpushed commits, and
#      that is work, not a backup.
#   4. THE AGE, BY CTIME. Nothing inside a candidate may have changed
#      (ctime) in the last 30 days. Not mtime: `cp -a` preserves mtimes,
#      so a rollback backup made TODAY from an old tree reads as July by
#      mtime (measured in review). ctime cannot be copied.
#   5. NOT LIVE. A candidate is refused when it is, contains or sits
#      inside what /opt/boss or /opt/boss-cli resolves to; when any mount
#      in /proc/self/mountinfo is at or under it (a same-filesystem bind
#      mount would defeat --one-file-system); when a symlink in /opt,
#      /usr/local/bin, /usr/bin, /usr/sbin or /etc/systemd/system
#      resolves into it; when any running process's executable lives in
#      it (/proc/*/exe); when any loaded service unit's
#      ExecStart/ExecStartPre/WorkingDirectory/FragmentPath resolves into
#      it; or when any unit FILE on disk (/etc/systemd/system,
#      /lib/systemd/system, /usr/lib/systemd/system, drop-ins included)
#      names it — the retired second stack's units are stopped and
#      disabled, not loaded, and kept for a restore.
#   6. THE JOURNAL. `journalctl --vacuum-size=1G` — a fixed bound, not a
#      param, so no request can ask for 0 and throw away the host's
#      diagnosis. The newest 1G stays.
#
# THE PLAN'S BYTES ARE DETERMINISTIC: each candidate's path, its size
# in MiB and its top-level entries, sorted, then the fixed journal line.
# No clock, no free space, no journal usage — those move on their own
# and ride stderr, where the hash does not reach.
#
# WHAT IT DOES NOT SEE. It reads units, unit files, mounts and
# processes, not every script on the host: a cron line or a shell script
# naming a backup directory by path would not stop it.
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the path or the bound; nothing removed
#   1  failed part-way — the record states what was already removed and
#      what was not touched
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_RECLAIM_OPT_DIR     where the candidates and live trees live (/opt)
#   BOSS_RECLAIM_PROC_DIR    the process table (/proc)
#   BOSS_RECLAIM_MOUNTINFO   the mount table (/proc/self/mountinfo)
#   BOSS_RECLAIM_LINK_DIRS   directories scanned for symlinks into a candidate
#   BOSS_RECLAIM_UNIT_DIRS   directories of unit files grepped for a candidate
#   BOSS_RECLAIM_NOW         the clock, epoch seconds (default: now)

set -uo pipefail

ME="reclaim-gcp-root"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; say "  nothing was removed."; exit 2; }

OPT="${BOSS_RECLAIM_OPT_DIR:-/opt}"
PROC="${BOSS_RECLAIM_PROC_DIR:-/proc}"
MOUNTINFO="${BOSS_RECLAIM_MOUNTINFO:-/proc/self/mountinfo}"
UNIT_DIRS="${BOSS_RECLAIM_UNIT_DIRS:-/etc/systemd/system /lib/systemd/system /usr/lib/systemd/system}"
JOURNAL_KEEP="1G"
MIN_AGE_DAYS=30
NAME_RE='^boss-binbak-[A-Za-z0-9._-]+$'

# --- bound 1: the argument -------------------------------------------------
usage() {
    say "usage: $ME --dry-run | <plan-sha256>"
    say "  --dry-run       every bound, then the plan on stdout and plan-sha256 on stderr; removes nothing"
    say "  <plan-sha256>   remove what the signed plan names, if today's plan still hashes to it"
    say "  this verb takes no path: what it may remove is fixed in the script"
    exit 2
}
[ "$#" -eq 1 ] || usage
APPROVED=""
case "$1" in
    --dry-run) DRY=1 ;;
    *)
        [[ "$1" =~ ^[0-9a-f]{64}$ ]] || { say "the argument is --dry-run or a 64-hex plan hash, not \`$1\`"; usage; }
        DRY=0; APPROVED="$1" ;;
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

# --- bound 2, 3 + 4: the names, not a checkout, the age --------------------
CANDS=()
now="${BOSS_RECLAIM_NOW:-$(date +%s)}"
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
    git=$(find "$real" -xdev -name .git -print -quit 2> "$TMP/find.err") \
        || refuse "could not search \`$real\` for a .git ($(head -c 300 "$TMP/find.err")) — a bound that cannot be evaluated is not passed"
    [ -z "$git" ] \
        || refuse "\`$real\` holds \`$git\` — a checkout may carry unpushed commits; that is work, not a backup"
    young=$(find "$real" -xdev -newerct "@$cutoff" -print -quit 2> "$TMP/find.err") \
        || refuse "could not read the ages under \`$real\` ($(head -c 300 "$TMP/find.err")) — a bound that cannot be evaluated is not passed"
    [ -z "$young" ] \
        || refuse "\`$real\` holds \`$young\`, changed (ctime) in the last $MIN_AGE_DAYS days — a fresh backup, even one copied with its old mtimes, may be somebody's rollback target, and this verb removes only old ones"
    CANDS+=("$real")
done

# --- bound 5: not live -----------------------------------------------------
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
if [ "${#CANDS[@]}" -gt 0 ]; then
    # (b) mounts at or under a candidate. Field 5 is the mount point,
    # with space, tab, newline and backslash octal-escaped.
    [ -r "$MOUNTINFO" ] || refuse "$MOUNTINFO cannot be read, so whether a backup has a mount inside it cannot be judged"
    while read -r _ _ _ _ mp _; do
        mp=$(printf '%b' "$mp")
        for c in "${CANDS[@]}"; do
            under "$mp" "$c" && refuse "$mp is a mount point at or under \`$c\` — rm would reach through a bind mount into another tree"
        done
    done < "$MOUNTINFO"
    # (c) symlinks elsewhere that resolve into a candidate.
    for ld in $LINK_DIRS; do
        [ -d "$ld" ] || continue
        find "$ld" -maxdepth 2 -type l -print0 > "$TMP/links" 2> "$TMP/find.err" \
            || refuse "could not list the symlinks under $ld ($(head -c 300 "$TMP/find.err")) — a bound that cannot be evaluated is not passed"
        while IFS= read -r -d '' link; do
            skip=0
            for c in "${CANDS[@]}"; do under "$link" "$c" && skip=1; done
            [ "$skip" = 1 ] && continue
            t=$(realpath -m -- "$link" 2>/dev/null) || continue
            for c in "${CANDS[@]}"; do
                under "$t" "$c" && refuse "the symlink $link resolves to $t, inside \`$c\` — that backup is still linked to"
            done
        done < "$TMP/links"
    done
    # (d) running processes.
    for exe in "$PROC"/[0-9]*/exe; do
        t=$(readlink -- "$exe" 2>/dev/null) || continue
        t="${t% (deleted)}"
        for c in "${CANDS[@]}"; do
            if under "$t" "$c"; then
                pid="${exe%/exe}"; pid="${pid##*/}"
                refuse "process $pid ($(cat "$PROC/$pid/comm" 2>/dev/null || echo '?')) runs $t, inside \`$c\` — that backup is LIVE"
            fi
        done
    done
    # (e) loaded service units.
    if ! systemctl list-units --type=service --all --no-pager --plain --no-legend > "$TMP/units" 2> "$TMP/units.err"; then
        refuse "systemctl could not list this host's service units ($(head -c 300 "$TMP/units.err")), so whether a backup is a unit's binary cannot be judged — a bound that cannot be evaluated is not passed"
    fi
    while read -r unit _; do
        [ -n "$unit" ] || continue
        props=$(systemctl show -p ExecStart -p ExecStartPre -p WorkingDirectory -p FragmentPath --value -- "$unit" 2> "$TMP/show.err") \
            || refuse "systemctl show $unit failed ($(head -c 300 "$TMP/show.err")), so whether it runs from a backup cannot be judged"
        set -f
        for p in $(printf '%s\n' "$props" | grep -oE '/[^ ;]+'); do
            t=$(realpath -m -- "$p" 2>/dev/null) || continue
            for c in "${CANDS[@]}"; do
                under "$t" "$c" && refuse "the unit $unit names $p (resolving to $t), inside \`$c\` — that backup is LIVE"
            done
        done
        set +f
    done < "$TMP/units"
    # (f) unit FILES on disk, loaded or not, drop-ins included.
    for ud in $UNIT_DIRS; do
        [ -d "$ud" ] || continue
        for c in "${CANDS[@]}"; do
            for form in "$c" "$OPT/${c##*/}"; do
                grep -rlF -- "$form" "$ud" > "$TMP/grep" 2> "$TMP/grep.err"
                case $? in
                    0) refuse "the unit file $(head -n 1 "$TMP/grep") names \`$form\` — a unit kept on disk (stopped, disabled, kept for a restore) still runs from that backup" ;;
                    1) ;;
                    *) refuse "could not search the unit files under $ud ($(head -c 300 "$TMP/grep.err")) — a bound that cannot be evaluated is not passed" ;;
                esac
            done
        done
    done
fi

# --- the plan: deterministic bytes ---------------------------------------
: > "$TMP/cands"
for c in "${CANDS[@]+"${CANDS[@]}"}"; do
    mib=$(du -sxm -- "$c" 2> "$TMP/du.err" | awk '{ print $1 }')
    [ -n "$mib" ] || refuse "du could not size \`$c\` ($(head -c 300 "$TMP/du.err"))"
    echo "$c $mib" >> "$TMP/cands"
done
render() {
    local c mib
    while read -r c mib; do
        echo "would remove $c ($mib MiB), holding:"
        find "$c" -mindepth 1 -maxdepth 1 -printf '  %f\n' | LC_ALL=C sort
    done < "$TMP/cands"
    echo "would vacuum the journal to $JOURNAL_KEEP (journalctl --vacuum-size=$JOURNAL_KEEP)"
}
render > "$TMP/plan"
HASH=$(sha256sum "$TMP/plan" | cut -c1-64)
TOTAL=$(awk '{ s += $2 } END { print s + 0 }' "$TMP/cands")
if [ "${#CANDS[@]}" -eq 0 ]; then
    say "no $OPT/boss-binbak-* or $OPT/boss-dev-bak on this host — no backup directory to remove"
fi
command -v journalctl >/dev/null 2>&1 \
    || refuse "journalctl is not on PATH, so the journal bound cannot be applied"
say "journal: $(journalctl --disk-usage 2>&1)"

if [ "$DRY" = 1 ]; then
    cat "$TMP/plan"
    say "DRY RUN — every bound passed: ${#CANDS[@]} backup directories (${TOTAL} MiB) and the journal beyond $JOURNAL_KEEP would go. Nothing was removed."
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write: only the plan that was signed ------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$TMP/plan" >&2
    refuse "today's plan (above) hashes to $HASH, not the approved $APPROVED — a backup appeared, went or changed since the plan was signed (or this plan was already applied). Render and approve it again"
fi
# The capture before the removal: the approved plan, on stdout, first.
cat "$TMP/plan"
echo "$ME: plan $APPROVED still holds"

DONE=()
while read -r c mib; do
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
done < "$TMP/cands"

if ! journalctl --vacuum-size="$JOURNAL_KEEP" > "$TMP/vacuum" 2>&1; then
    sed 's/^/    /' "$TMP/vacuum" >&2
    say "FAILED — journalctl --vacuum-size=$JOURNAL_KEEP exited non-zero; the ${#DONE[@]} backup directories above are removed."
    exit 1
fi
sed 's/^/  /' "$TMP/vacuum"
echo "vacuumed the journal to $JOURNAL_KEEP: $(journalctl --disk-usage 2>&1)"
FREE_AFTER=$(free_mib)
say "OK — removed ${#DONE[@]} backup directories (${TOTAL} MiB) and vacuumed the journal to $JOURNAL_KEEP; root free ${FREE_BEFORE:-unknown} → ${FREE_AFTER:-unknown} MiB. /var/backups, homes, /usr/local, $OPT/boss and $OPT/boss-cli are untouched. The journal grows back without a SystemMaxUse bound; the lasting gain is the backups."
exit 0
