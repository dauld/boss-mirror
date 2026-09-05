#!/usr/bin/env bash
# disk-report.sh — what is consuming the forge host's disk, READ-ONLY.
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

hr "system docker (CI jobs run here; sudo -n docker)"
if sudo -n docker system df 2>/dev/null; then
    say "-- largest images --"
    sudo -n docker images --format '{{.Size}}\t{{.Repository}}:{{.Tag}}\t{{.CreatedSince}}' 2>/dev/null | sort -h -r | head -25
    say "-- volumes --"
    sudo -n docker system df -v 2>/dev/null | sed -n '/^Local Volumes space usage/,/^$/p' | head -40
    say "-- containers (all) --"
    sudo -n docker ps -a --format '{{.Status}}\t{{.Size}}\t{{.Names}}' 2>/dev/null | head -25
else
    say "system docker: not readable (sudo -n docker refused or no daemon)"
fi

hr "rootless docker (converge builds; the daemon disk-floor-sweep prunes)"
export DOCKER_HOST="${DOCKER_HOST:-unix:///run/user/1000/docker.sock}"
docker system df 2>/dev/null || say "rootless docker: not reachable at $DOCKER_HOST"

hr "directories, largest first (du -xsh; sudo -n where refused it says so)"
# Fixed list — the places a forge host grows: both docker roots,
# Forgejo's data (repos, packages/registry, actions logs+artifacts),
# the runner's workspaces, journals, and home.
for d in /var/lib/docker /var/lib/containerd /var/lib/forgejo /var/lib/gitea \
         /opt /srv /var/log /var/log/journal /var/cache /var/tmp /tmp \
         /home /home/david/.local/share/docker /home/david/boss /home/david/.cache \
         /root /snap; do
    [ -e "$d" ] || continue
    if out=$(sudo -n du -xsh "$d" 2>/dev/null); then
        say "$out"
    elif out=$(du -xsh "$d" 2>/dev/null); then
        say "$out	(unprivileged read; may undercount)"
    else
        say "?	$d	(not readable)"
    fi
done | sort -h -r

hr "forgejo data, one level down (if readable)"
for base in /var/lib/forgejo /var/lib/gitea /opt/forgejo /srv/forgejo; do
    [ -d "$base" ] || continue
    sudo -n du -xsh "$base"/* 2>/dev/null | sort -h -r | head -15 \
        || du -xsh "$base"/* 2>/dev/null | sort -h -r | head -15
done

hr "top-level /var/lib, largest first (sudo -n)"
sudo -n du -xsh /var/lib/* 2>/dev/null | sort -h -r | head -15 || say "/var/lib: not readable without sudo"
