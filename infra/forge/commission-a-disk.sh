#!/usr/bin/env bash
# commission-a-disk — turn a raw block device into mounted capacity.
#
# MUTATING, AND THE MOST DESTRUCTIVE THING IN THIS TREE: it writes a
# partition table and a filesystem. Everything below exists to make the
# WRONG TARGET UNREPRESENTABLE rather than merely disapproved, which is
# the claim design 17835005 turns on — an approval that authorises
# "format the disk you meant" is weaker than a command that cannot name
# the disk you did not.
#
# WHY THAT IS NOT PARANOIA. On 2026-09-21 a second NVMe went into the
# forge and the kernel RENUMBERED the devices: the new blank drive came
# up as nvme0n1 and the live root filesystem moved to nvme1n1p2. The
# disk that looked like "the new one" by number was the one carrying the
# system. The host booted only because fstab resolves by UUID. Any
# operator or agent reaching for a kernel name that day would have been
# aiming at the wrong device while believing otherwise.
#
# SO THE TARGET IS A STABLE NAME, and three preconditions are checked
# IMMEDIATELY BEFORE the write, never once at plan time:
#
#   1. the target resolves under /dev/disk/by-id/ (a kernel name is
#      refused outright — it is not a stable identity);
#   2. it carries NO partition table (a disk with partitions is
#      somebody's disk);
#   3. no filesystem anywhere on it is mounted, and it is not the
#      device backing / — checked by resolution, not by string
#      comparison, because /dev/nvme1n1p2 and a by-id symlink to it are
#      the same disk spelled two ways.
#
# A refusal here is exit 78 (EX_CONFIG): the request was wrong, not the
# run. Exit 1 is reserved for a step that genuinely failed.
set -eu

die() { echo "commission-a-disk: $*" >&2; exit 78; }

[ $# -eq 2 ] || die "usage: commission-a-disk.sh <device-by-id> <mount-path>"
BY_ID="$1"
MOUNT="$2"

case "$BY_ID" in
  /dev/disk/by-id/*) : ;;
  *) die "target must be a /dev/disk/by-id/ path — '$BY_ID' is not a stable identity, and kernel names move when a disk is added (measured on the forge, 2026-09-21)" ;;
esac
case "$MOUNT" in
  /*) : ;;
  *) die "mount path must be absolute, got '$MOUNT'" ;;
esac

[ -e "$BY_ID" ] || die "no such device: $BY_ID"
DEV=$(readlink -f "$BY_ID") || die "cannot resolve $BY_ID"
[ -b "$DEV" ] || die "$BY_ID resolves to $DEV, which is not a block device"

# PRECONDITION 2 — no partition table. `lsblk` lists the device plus
# any children; more than one line means it has partitions.
kids=$(lsblk -nro NAME "$DEV" | wc -l)
[ "$kids" -eq 1 ] || die "$DEV already carries $((kids - 1)) partition(s) — a disk with partitions is somebody's disk; this verb only commissions a raw one"

# PRECONDITION 3 — nothing on it is mounted, and it does not back /.
# Resolution, not string comparison: a by-id symlink and a kernel name
# are the same disk spelled two ways.
root_src=$(findmnt -nro SOURCE / || true)
if [ -n "$root_src" ]; then
    root_dev=$(readlink -f "$root_src" 2>/dev/null || echo "$root_src")
    case "$root_dev" in
      "$DEV"*) die "$DEV backs the root filesystem ($root_src) — refusing" ;;
    esac
fi
mounted=$(lsblk -nro MOUNTPOINTS "$DEV" | tr -d ' ' | grep -c . || true)
[ "${mounted:-0}" -eq 0 ] || die "$DEV has $mounted mounted filesystem(s) — refusing"

echo "commission-a-disk: preconditions hold for $BY_ID -> $DEV (raw, unmounted, not root)"
echo "commission-a-disk: partitioning"
parted -s "$DEV" mklabel gpt mkpart boss-data ext4 0% 100%
udevadm settle 2>/dev/null || sleep 2

PART="${BY_ID}-part1"
[ -e "$PART" ] || die "partition did not appear at $PART"
mkfs.ext4 -q -L boss-data "$(readlink -f "$PART")"

UUID=$(blkid -s UUID -o value "$(readlink -f "$PART")") || die "cannot read the new filesystem's UUID"
mkdir -p "$MOUNT"
# BY UUID, never by kernel name — the reason this verb exists.
if ! grep -q "UUID=$UUID" /etc/fstab; then
    echo "UUID=$UUID $MOUNT ext4 defaults,noatime 0 2" >> /etc/fstab
fi
systemctl daemon-reload
mount -a
findmnt -nro TARGET "$MOUNT" >/dev/null || die "mounted nothing at $MOUNT"

echo "commission-a-disk: $MOUNT is live on UUID=$UUID"
df -h "$MOUNT"
