#!/usr/bin/env bash
# the-launcher-checks-its-own-image.sh — `boss-launch --check` refuses an
# image that lacks what the launcher sources, and passes one that has it.
#
# The converge runs this check on the built image before rolling the
# cluster (cluster-deploy-lib.sh image_boots). Here it runs on a copy of
# the launcher in a scratch directory: alone, it must fail naming the
# missing tenant-launch.sh; with the library copied beside it, it must
# pass — and it must do so WITHOUT the service binaries, which only
# the image has. 2026-09-05: the image lacked the library and the pod
# crash-looped for hours; this is that failure made a pre-roll refusal.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src="$here/../oss-quickstart"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
cp "$src/services-launcher.sh" "$tmp/boss-launch"; chmod +x "$tmp/boss-launch"
out="$(bash "$tmp/boss-launch" --check 2>&1)"; rc=$?
[[ $rc -ne 0 ]] || { echo "FAIL: --check passed with tenant-launch.sh missing:"; echo "$out"; exit 1; } >&2
grep -q "tenant-launch.sh is missing" <<<"$out" || { echo "FAIL: --check did not name the missing file:"; echo "$out"; exit 1; } >&2
cp "$src/tenant-launch.sh" "$tmp/tenant-launch.sh"
out="$(bash "$tmp/boss-launch" --check 2>&1)"; rc=$?
[[ $rc -eq 0 ]] || { echo "FAIL: --check failed with the library beside the launcher:"; echo "$out"; exit 1; } >&2
grep -q "launcher check: ok" <<<"$out" || { echo "FAIL: no ok line:"; echo "$out"; exit 1; } >&2
[[ ! -f "$tmp/etc" ]] || true
echo "the-launcher-checks-its-own-image: self-test ok — --check refuses a launcher without tenant-launch.sh beside it, names the file, and passes with it (no binaries needed)"
exit 0
