#!/usr/bin/env bash
# disk-report.sh — what is consuming this host's disk, READ-ONLY. It
# serves the forge and boss-gcp (infra/ops/verbs/disk-report.json);
# every section skips what the host does not have, so one script reads
# both.
#
# BOSS-GCP (backlog d3c7eada, 2026-09-26). boss-gcp's 48 GB root sat at
# 11-13 GB free for nine days under its 17 GB floor, and the only disk
# verb serving it was `df` — so what filled the root could not be read
# through a door. The residue suspected there is the retired second
# stack (ops-request 7912c9ae left its unit files and binaries, and put
# its database capture under /var/backups/boss/second-stack/), so the
# fixed list carries /var/backups and /usr/local, and /var/backups,
# /opt, /home and /var/lib are also read one level down. That host
# mounts its builds (/var/lib/boss-build, sdc) and postgres (sdb) UNDER
# /var/lib, so a directory that is a mount point is marked as its own
# filesystem: its size says nothing about the root the alarm is about.
#
# WHY. On 2026-09-05 the locomotive refused train #204 at 65GB free
# against its 70GB floor; the bounded reclaim (disk-floor-sweep.sh)
# pruned every regenerable cache it is allowed to touch and stopped at
# exactly 70GB with "a human decides next" — and nobody could say what
# held the other 158GB of a 228GB disk, because no door from the dev
# pod reads this host's filesystem: the forge token has no package
# scope, gatewayd serves journals only, and the ops verb allowlist had
# no report verb. David: "I want to understand what is consuming disk
# first. If we can't do the investigation from here, I want to discuss
# how we add that capability as we go." This is that capability, in
# the shape the allowlist demands: one fixed script, no arguments,
# nothing mutated.
#
# WHAT IT READS. This host runs TWO docker daemons (reap-dead-ci-jobs
# header): the system daemon that Forgejo Actions jobs run in (`sudo
# docker`), and david's rootless daemon that the converge builds in.
# disk-floor-sweep prunes only the rootless one, so the system daemon's
# images, build cache and job volumes are the first suspect. Then a
# fixed list of directories, largest first. `sudo -n` is tried where
# root is needed and the line says so when it was refused, so a
# reader can tell "small" from "unreadable".
set -uo pipefail

say() { printf '%s\n' "$*"; }
hr()  { say "== $* =="; }

hr "filesystems"
df -h -x tmpfs -x devtmpfs -x overlay 2>/dev/null || df -h
# WHICH filesystem, with its mount options: whether the root volume
# reflinks (xfs with reflink=1, btrfs) decides whether a CI seed can be
# copied for free the way the gate's is on w-1, or costs the whole
# target size per job (47fc2bc1). Never assumed; read here.
say "-- root filesystem type and options (reflink: xfs reflink=1 or btrfs) --"
findmnt -no SOURCE,FSTYPE,OPTIONS / 2>/dev/null || say "findmnt unavailable"
if command -v xfs_info >/dev/null 2>&1 && [ "$(findmnt -no FSTYPE / 2>/dev/null)" = "xfs" ]; then
    sudo -n xfs_info / 2>/dev/null | grep -o 'reflink=[01]' || xfs_info / 2>/dev/null | grep -o 'reflink=[01]' || say "xfs_info: not readable without sudo"
fi

hr "CI runner volume policy (act_runner container.valid_volumes)"
# Whether a CI job may mount a host path at all. The runner's config is
# not in the tree; its policy decides whether a seed volume is even
# possible. Read, never guessed — a missing key means the default
# (no host volumes).
for cfg in /etc/forgejo-runner/config.yaml /etc/act_runner/config.yaml /home/david/.config/forgejo-runner/config.yaml /var/lib/forgejo-runner/config.yaml; do
    if [ -r "$cfg" ] || sudo -n test -r "$cfg" 2>/dev/null; then
        say "-- $cfg --"
        (sudo -n cat "$cfg" 2>/dev/null || cat "$cfg") | grep -nE 'valid_volumes|privileged|options:|docker_host|workdir_parent|^\s*-\s' | grep -vE 'token|secret' | sed -n '1,20p'
        say "(only the volume/privilege lines are shown; tokens never)"
        break
    fi
done

hr "system docker (CI jobs run here; sudo -n docker)"
if sudo -n docker system df 2>/dev/null; then
    say "-- largest images --"
    sudo -n docker images --format '{{.Size}}\t{{.Repository}}:{{.Tag}}\t{{.CreatedSince}}' 2>/dev/null | sort -h -r | sed -n '1,25p'
    say "-- volumes --"
    sudo -n docker system df -v 2>/dev/null | sed -n '/^Local Volumes space usage/,/^$/p' | sed -n '1,40p'
    say "-- containers (all) --"
    sudo -n docker ps -a --format '{{.Status}}\t{{.Size}}\t{{.Names}}' 2>/dev/null | sed -n '1,25p'
else
    say "system docker: not readable (sudo -n docker refused or no daemon)"
fi

hr "rootless docker (converge builds; the daemon disk-floor-sweep prunes)"
export DOCKER_HOST="${DOCKER_HOST:-unix:///run/user/1000/docker.sock}"
docker system df 2>/dev/null || say "rootless docker: not reachable at $DOCKER_HOST"

# One directory's size, one line: sudo -n first, then an unprivileged
# read that says it may undercount, then "not readable" — so a reader
# tells "small" from "unreadable". A mount point is marked as its own
# filesystem (d3c7eada: boss-gcp mounts sdb and sdc under /var/lib).
measure() {
    local d="$1" out note=""
    if mountpoint -q "$d" 2>/dev/null; then
        note="	(its own filesystem, not the root: $(findmnt -no SOURCE "$d" 2>/dev/null | sed -n '1p'))"
    fi
    if out=$(sudo -n du -xsh "$d" 2>/dev/null); then
        say "$out$note"
    elif out=$(du -xsh "$d" 2>/dev/null); then
        say "$out	(unprivileged read; may undercount)$note"
    else
        say "?	$d	(not readable)$note"
    fi
}

hr "directories, largest first (du -xsh; sudo -n where refused it says so)"
# Fixed list — the places a forge host grows: both docker roots,
# Forgejo's data (repos, packages/registry, actions logs+artifacts),
# the runner's workspaces, journals, and home; and the places a
# retired stack leaves residue on boss-gcp: backups and /usr/local; and
# /var/lib whole, which `du -x` counts on the ROOT only — the mounts
# under it are left out, so this line is /var/lib's share of the root.
for d in /var/lib/docker /var/lib/containerd /var/lib/forgejo /var/lib/gitea \
         /opt /srv /var/log /var/log/journal /var/cache /var/tmp /tmp \
         /var/backups /usr/local /var/lib \
         /home /home/david/.local/share/docker /home/david/boss /home/david/.cache \
         /root /snap; do
    [ -e "$d" ] || continue
    measure "$d"
done | sort -h -r

hr "forgejo data, one level down (if readable)"
for base in /var/lib/forgejo /var/lib/gitea /opt/forgejo /srv/forgejo; do
    [ -d "$base" ] || continue
    sudo -n du -xsh "$base"/* 2>/dev/null | sort -h -r | sed -n '1,15p' \
        || du -xsh "$base"/* 2>/dev/null | sort -h -r | sed -n '1,15p'
done

# One level down, bounded to the 15 largest entries each — the level a
# reclaim is decided at (d3c7eada). /var/lib's entries are measured one
# by one so a mount point under it is marked, not counted as root.
for base in /var/backups /opt /home /var/lib; do
    [ -d "$base" ] || continue
    set -- "$base"/*
    [ -e "$1" ] || continue
    hr "$base, one level down, largest first (15 at most)"
    for d in "$@"; do
        measure "$d"
    done | sort -h -r | sed -n '1,15p'
done

hr "verdict"
# THE READING CARRIES ITS VERDICT (backlog 970c0c94, measured
# 2026-09-18). The disk-headroom sweep files this report for itself the
# moment its Inspect step becomes ready (measure-disk-headroom-sweep-on-
# inspect-ready) and the answer landed as free text: exit 0 at 68% root
# exactly as it would at 99%, so nothing could complete the sweep's
# Inspect step by rule and four of them sat assigned to the agent for a
# day with a clean number on another packet. The last line is now ONE
# machine-readable verdict — `verdict: clean` or `verdict: <finding>` —
# that the dispatcher rule judge-disk-headroom-sweep-on-report-answered
# reads (maintenance.sweep.judge). The exit code stays 0 either way: a
# finding is an answer, not a failure.
#
# THE FLOOR IS THE ESTATE'S, NOT A NEW NUMBER. It is the same rule the
# estate comparator applies to every observed host —
# max(DISK_TIGHT_FLOOR_GB, min(DISK_TIGHT_FLOOR_PCT of capacity,
# DISK_TIGHT_HEADROOM_CEILING_GB)) — read from the same `df -k /` the
# host observer takes (infra/estate/observe-host.sh, nearest GiB). A
# shell script cannot read a Rust `const`, so the three numbers are
# PINNED to crates/orchestrators/boss-dispatcher-handlers/src/handlers/
# estate_compare.rs by `the_disk_report_judges_by_the_comparators_floor`,
# which names whichever moved (CLAUDE.md §9a: a pin is what you write
# when you cannot collapse today). If the floor ever lands in one
# registry both readers should read it and the pin should go.
DISK_TIGHT_FLOOR_GB=16
DISK_TIGHT_FLOOR_PCT=35
DISK_TIGHT_HEADROOM_CEILING_GB=200
disk_kb=$(df -k / 2>/dev/null | awk 'NR==2 {print $2}')
free_kb=$(df -k / 2>/dev/null | awk 'NR==2 {print $4}')
case "${disk_kb:-empty}${free_kb:-empty}" in
    *empty*|*[!0-9]*)
        # An unmeasured disk is not a clean one (estate_compare.rs
        # records it apart from a finding for the same reason).
        say "verdict: disk_unmeasured (df -k / answered '${disk_kb:-}' '${free_kb:-}')"
        ;;
    *)
        disk_gb=$(( (disk_kb + 524288) / 1048576 ))
        free_gb=$(( (free_kb + 524288) / 1048576 ))
        # The percentage floor, rounded UP so that `free < pct_floor`
        # is exactly the comparator's `free * 100 < total * PCT`.
        pct_floor=$(( (disk_gb * DISK_TIGHT_FLOOR_PCT + 99) / 100 ))
        floor=$pct_floor
        [ "$floor" -gt "$DISK_TIGHT_HEADROOM_CEILING_GB" ] && floor=$DISK_TIGHT_HEADROOM_CEILING_GB
        [ "$floor" -lt "$DISK_TIGHT_FLOOR_GB" ] && floor=$DISK_TIGHT_FLOOR_GB
        if [ "$disk_gb" -gt 0 ] && [ "$free_gb" -ge "$floor" ]; then
            say "verdict: clean"
        else
            say "verdict: disk_tight free=${free_gb}g floor=${floor}g"
        fi
        ;;
esac
