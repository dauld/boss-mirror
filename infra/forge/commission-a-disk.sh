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
#
# --plan RENDERS AND DOES NOT ACT (design 17835005, answered by David
# 2026-09-21). It evaluates exactly the preconditions the write path
# evaluates, observes the same facts, and prints a PLAN DOCUMENT: the
# resolved target, what was observed, the argv that would run and the
# effect it would have. Nothing is written. It is the "plan" of
# plan-then-approve, and it is what a passkey signs over — q1 settled
# that the signature binds a rendered plan rather than a verb call,
# because a verb call authorises an intent whose target can still
# resolve differently at execution time, which is exactly how the
# device renumbering would have gone wrong.
#
# THE PLAN IS HASHED OVER ITS OWN BYTES, so there is one definition of
# what was approved and no canonicalisation to drift (§9a). Whoever
# verifies re-hashes the bytes it was handed; nobody re-renders and
# compares. That is why this document carries NO timestamp and nothing
# else that varies between two renders of the same true state — the
# render time belongs on the packet, outside what is signed. Two plans
# of the same disk in the same state are byte-identical; if any
# OBSERVED fact moves, the bytes move with it, which is the drift q4
# says must void an approval.
set -eu

die() { echo "commission-a-disk: $*" >&2; exit 78; }

PLAN=0
if [ "${1-}" = "--plan" ]; then PLAN=1; shift; fi

[ $# -eq 2 ] || die "usage: commission-a-disk.sh [--plan] <device-by-id> <mount-path>"
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

# ---- the plan, when that is all that was asked for ------------------
# Reached only with every precondition holding: a plan for a target that
# cannot be commissioned is not a plan, it is a refusal, and the `die`
# calls above have already made it one. So a plan on stdout means "this
# would run", and exit 78 means "it would not, and here is why" — the
# same two answers the write path gives, without the write.
if [ "$PLAN" -eq 1 ]; then
    size=$(blockdev --getsize64 "$DEV" 2>/dev/null || echo 0)
    # The template is a FILE, not an inline program, so the test runs
    # the same bytes the script runs rather than a copy of them (§9a).
    jq -n --arg by_id "$BY_ID" --arg dev "$DEV" --arg mount "$MOUNT" \
          --arg size "$size" --arg parts "$((kids - 1))" --arg mounted "${mounted:-0}" \
          -f "$(dirname "$0")/commission-a-disk.plan.jq"
    exit 0
fi

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
