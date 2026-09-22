# The plan document `commission-a-disk --plan` prints, and the exact
# bytes a passkey signs over (design 17835005, q1).
#
# IT LIVES IN ITS OWN FILE so there is ONE definition of the shape: the
# script runs this file, and the test runs this file. An inline program
# would have to be extracted to be tested, and an extracted copy is a
# copy (§9a).
#
# NOTHING HERE MAY VARY BETWEEN TWO RENDERS OF THE SAME TRUE STATE.
# No timestamp, no hostname, no run id. The plan is hashed over its own
# bytes, so anything that moves on its own breaks an approval that is
# still valid; and conversely, if any OBSERVED fact moves, the bytes
# move with it, which is the drift that q4 says must void an approval.
# The render time belongs on the packet, outside what is signed.
{
  plan_version: 1,
  verb: "commission-a-disk",
  target_by_id: $by_id,
  resolves_to: $dev,
  size_bytes: ($size | tonumber),
  mount_path: $mount,
  argv: ["infra/forge/commission-a-disk.sh", $by_id, $mount],
  observed: {
    partition_count: ($parts | tonumber),
    mounted_filesystems: ($mounted | tonumber),
    backs_root: false
  },
  effect: ("a GPT label, one ext4 partition labelled boss-data spanning the disk, "
           + "an /etc/fstab entry BY UUID, and the filesystem mounted at " + $mount)
}
