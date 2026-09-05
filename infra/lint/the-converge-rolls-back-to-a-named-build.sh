#!/usr/bin/env bash
# the-converge-rolls-back-to-a-named-build.sh — the cluster converge
# rolls a failed boot back to the LAST CONVERGED build by image name,
# pins the applied manifests to that build, and refuses an image that
# fails its own boot check.
#
# Exercises infra/forge/cluster-deploy-lib.sh with a stub kubectl and
# a stub docker that record every call. 2026-09-05: the real runner
# rolled a bricked head back to "the previous revision", which was the
# placeholder image its own apply had created seconds earlier and
# which could not boot; the cluster stayed dark for four hours.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="$here/../forge/cluster-deploy-lib.sh"
[[ -f "$lib" ]] || { echo "the-converge-rolls-back-to-a-named-build: missing $lib" >&2; exit 1; }
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export STUB_LOG="$tmp/calls" STUB_FAIL_FIRST_ROLLOUT="$tmp/fail-first"
cat >"$tmp/kubectl" <<'STUB'
#!/usr/bin/env bash
echo "kubectl $*" >>"$STUB_LOG"
case "$*" in
  *"get deploy boss"*) echo "10.20.0.15:3000/david/boss:running-before";;
  *"rollout status"*) if [[ -f "$STUB_FAIL_FIRST_ROLLOUT" ]]; then rm -f "$STUB_FAIL_FIRST_ROLLOUT"; exit 1; fi;;
esac
exit 0
STUB
chmod +x "$tmp/kubectl"
# shellcheck source=/dev/null
. "$lib"
fail() { echo "FAIL: $*" >&2; echo "--- calls:" >&2; cat "$STUB_LOG" 2>/dev/null >&2; exit 1; }

# 1. A head that boots: one patch to HEAD, no rollback, rc 0.
: >"$STUB_LOG"
roll_deployment "$tmp/kubectl" 10.20.0.15:3000/david/boss headsha lastgood "$tmp/failed" || fail "a booting head returned non-zero"
grep -q 'patch deploy boss.*david/boss:headsha' "$STUB_LOG" || fail "HEAD was not rolled"
[[ ! -f "$tmp/failed" ]] || fail "a booting head was quarantined"

# 2. A head that never goes Ready: quarantined, rolled back to REGISTRY:LAST_GOOD by name, rc 1.
: >"$STUB_LOG"; touch "$STUB_FAIL_FIRST_ROLLOUT"
roll_deployment "$tmp/kubectl" 10.20.0.15:3000/david/boss brickedsha lastgood "$tmp/failed" 2>"$tmp/err" && fail "a bricked head returned zero"
[[ "$(<"$tmp/failed")" == "brickedsha" ]] || fail "the bricked head was not quarantined"
grep -q 'patch deploy boss.*david/boss:lastgood' "$STUB_LOG" || fail "the rollback did not target the last converged build by name"
grep -q 'rolling back to 10.20.0.15:3000/david/boss:lastgood' "$tmp/err" || fail "the journal line does not name the target: $(cat "$tmp/err")"
grep -q 'rolled back — cluster serves 10.20.0.15:3000/david/boss:lastgood' "$tmp/err" || fail "no verified-rolled-back line: $(cat "$tmp/err")"

# 3. No converged build yet (first ever converge): the target is the image that was running.
: >"$STUB_LOG"; touch "$STUB_FAIL_FIRST_ROLLOUT"; rm -f "$tmp/failed"
roll_deployment "$tmp/kubectl" 10.20.0.15:3000/david/boss brickedsha none "$tmp/failed" 2>"$tmp/err" && fail "rc 0 on a bricked first converge"
grep -q 'patch deploy boss.*david/boss:running-before' "$STUB_LOG" || fail "with no converged build the rollback did not target the running image"

# 4. The applied manifests carry the converged image, never the placeholder.
mkdir -p "$tmp/src"; printf 'spec:\n  image: 10.20.0.15:3000/david/boss:b2814ef\n  other: 10.20.0.15:3000/david/boss-ci:rust1.96\n' >"$tmp/src/boss.yaml"
manifests_with_image "$tmp/src" "$tmp/dst" 10.20.0.15:3000/david/boss lastgood
grep -q 'image: 10.20.0.15:3000/david/boss:lastgood' "$tmp/dst/boss.yaml" || fail "the placeholder tag survived the apply copy: $(cat "$tmp/dst/boss.yaml")"
grep -q 'boss-ci:rust1.96' "$tmp/dst/boss.yaml" || fail "an unrelated image was rewritten"
rm -rf "$tmp/dst"; manifests_with_image "$tmp/src" "$tmp/dst" 10.20.0.15:3000/david/boss none
grep -q 'boss:b2814ef' "$tmp/dst/boss.yaml" || fail "a first converge (no stamp) rewrote the manifest"

# 5. The boot check gates the roll: image_boots runs the image's launcher check.
cat >"$tmp/docker" <<'STUB'
#!/usr/bin/env bash
echo "docker $*" >>"$STUB_LOG"; [[ "$*" == *"--check"* ]] && [[ "$*" == *"boots-image"* ]] && exit 0; exit 1
STUB
chmod +x "$tmp/docker"; : >"$STUB_LOG"
image_boots "$tmp/docker" 10.20.0.15:3000/david/boss:boots-image || fail "a booting image was refused"
image_boots "$tmp/docker" 10.20.0.15:3000/david/boss:broken-image && fail "a broken image passed the boot check"
grep -q 'entrypoint /usr/local/bin/boss-launch' "$STUB_LOG" || fail "the boot check did not run the launcher"

echo "the-converge-rolls-back-to-a-named-build: self-test ok — a bricked head is quarantined and rolled back to the last converged build by name (or the running image on a first converge), the applied manifests carry the converged image, and the boot check gates the roll"
exit 0
