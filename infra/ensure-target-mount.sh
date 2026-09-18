#!/usr/bin/env bash
# Ensure `/opt/boss/target` is a symlink into the dedicated
# build mount (`/var/lib/boss-build/target`) instead of a real
# directory on the OS root disk.
#
# Why this matters: the repo's release build produces ~10 GB of
# artifacts. On the GCP VMs that ship the playground we have a
# 48 GB root disk and a separate 98 GB build mount at
# `/var/lib/boss-build` (per `infra/oss-quickstart`). When
# `/opt/boss/target` is a real directory on `/`, those 10 GB
# pile onto root and a couple of release rebuilds will fill it
# and crash systemd. We saw exactly that during the v1.0.5
# Clock-as-service deploy: root went from 38 G → 38 G → 47 G →
# OOM on the next build.
#
# The intended setup (the pre-cluster dev VM's):
# `/opt/boss/target` is a symlink to `/var/lib/boss-build/target`.
# Fresh checkouts + `git clean -dfx` periodically break the
# symlink — running this script idempotently restores it.
#
# Usage:
#   sudo ./infra/ensure-target-mount.sh
#
# Safe to run at any time. If `target/` is already a symlink to
# the build mount → no-op. If it's a real directory → moves
# contents to the mount + replaces with a symlink. If the build
# mount itself isn't there → exits 0 with a warning (some dev
# hosts run target on root by design).

set -euo pipefail

REPO_TARGET="/opt/boss/target"
MOUNT_TARGET="/var/lib/boss-build/target"

if [[ ! -d /var/lib/boss-build ]]; then
    echo "ensure-target-mount: /var/lib/boss-build absent — leaving $REPO_TARGET as-is" >&2
    exit 0
fi

if [[ -L "$REPO_TARGET" ]]; then
    resolved=$(readlink -f "$REPO_TARGET")
    if [[ "$resolved" == "$MOUNT_TARGET" ]]; then
        echo "ensure-target-mount: ok ($REPO_TARGET → $MOUNT_TARGET)"
        exit 0
    fi
    echo "ensure-target-mount: $REPO_TARGET points at $resolved, not $MOUNT_TARGET — re-pointing" >&2
    rm "$REPO_TARGET"
fi

if [[ -d "$REPO_TARGET" && ! -L "$REPO_TARGET" ]]; then
    echo "ensure-target-mount: moving real directory $REPO_TARGET → $MOUNT_TARGET"
    if [[ -e "$MOUNT_TARGET" ]]; then
        echo "  $MOUNT_TARGET already exists — merging via rsync (this may take a minute)" >&2
        sudo -u boss rsync -a "$REPO_TARGET/" "$MOUNT_TARGET/"
        sudo -u boss rm -rf "$REPO_TARGET"
    else
        sudo -u boss mv "$REPO_TARGET" "$MOUNT_TARGET"
    fi
fi

sudo -u boss ln -s "$MOUNT_TARGET" "$REPO_TARGET"
echo "ensure-target-mount: ok ($REPO_TARGET → $MOUNT_TARGET)"
df -h / /var/lib/boss-build | tail -2
