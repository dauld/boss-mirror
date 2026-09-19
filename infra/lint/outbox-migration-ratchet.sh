#!/usr/bin/env bash
#
# outbox-migration-ratchet — the kind-classification guard for the
# transactional-audit-log migration (outbox phase 2).
#
# THE PRINCIPLE
# -------------
# A domain event is the rebuild source for state. If it publishes
# post-commit (`DomainPublisher::emit_*`), there is a crash window
# where the state commits and the event is lost — the swallowed-write
# class behind the 2026-07-13 replay-divergence incident. The fix is
# structural: writers record the event ON THE TRANSACTIONAL OUTBOX,
# inside the same transaction as the state
# (`boss_events::outbox::record_event_in_tx` via an `EventStamp`), and
# boss-event-relay delivers it to audit_log + NATS after commit.
#
# THE CHECKED PROPERTY
# --------------------
# The migration COMPLETED 2026-07-28 (the last PENDING row,
# boss-cybernetics, moved to the record-only EventRecorder path), so
# this is now a FLAT BAN: NO crate may call the post-commit publish
# APIs. Domain writes record via record_event_in_tx inside their
# transaction; row-less signals (cybernetics telemetry, the jobs
# post-materialization step.ready pass) record via the EventRecorder
# port / JobsRepository::record_events. A brand-new crate that starts
# publishing post-commit fails CI immediately.
#
# WHAT COUNTS AS AN EMIT SITE
# ---------------------------
# A method call to any of the four DomainPublisher publish APIs
# (`.emit_at(` / `.emit_with_actor_at(` / `.emit_simulated_at(` /
# `.emit_with_actor_simulated_at(`) in a crate's src/ tree.
# `publisher.rs` (the API definition) and `tests/` are excluded.
# Comment-only mentions are excluded.
#
# Usage:  infra/lint/outbox-migration-ratchet.sh
# The CI hook lives in .github/workflows/ci.yml alongside the other lints.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

EMIT_PATTERN='\.emit_at\(|\.emit_with_actor_at\(|\.emit_simulated_at\(|\.emit_with_actor_simulated_at\('

# Count emit call sites in one crate's src/ tree (0 when the crate has
# none). Excludes the publisher's own definition file, tests/, and
# comment lines.
count_sites() {
  local crate_dir="$1"
  # `|| true` swallows grep's exit-1-on-no-match under pipefail; the
  # wc output is what we want either way.
  { grep -rn --include='*.rs' -E "$EMIT_PATTERN" "$crate_dir" 2>/dev/null \
      | grep -v '/publisher\.rs:' \
      | grep -v '/tests/' \
      | grep -vE '^[^:]+:[0-9]+: *//' \
      | wc -l; } || true
}

list_sites() {
  local crate_dir="$1"
  { grep -rn --include='*.rs' -E "$EMIT_PATTERN" "$crate_dir" 2>/dev/null \
      | grep -v '/publisher\.rs:' \
      | grep -v '/tests/' \
      | grep -vE '^[^:]+:[0-9]+: *//' \
      | sed 's/^/    /'; } || true
}

fail=0
crates_scanned=0

echo "outbox-migration-ratchet: post-commit publisher emits (flat ban)"
echo

# Flat ban: every crate must have zero post-commit emit sites.
while IFS= read -r cargo_toml; do
  dir=$(dirname "$cargo_toml")
  crate=$(basename "$dir")
  [ -d "$dir/src" ] || continue
  crates_scanned=$((crates_scanned + 1))
  n=$(count_sites "$dir/src")
  if [ "$n" -ne 0 ]; then
    echo "  [FAIL] $crate has $n post-commit emit site(s):"
    list_sites "$dir/src"
    echo "         → record the event in the domain transaction instead:"
    echo "           boss_events::outbox::record_event_in_tx(&mut tx, &stamp.event(kind, payload))"
    echo "         (row-less signals go through the EventRecorder port)"
    fail=1
  fi
done < <(find crates -mindepth 3 -maxdepth 3 -name Cargo.toml)
echo "  [ok] zero post-commit emit sites across the workspace"

echo
if [ "$fail" -ne 0 ]; then
  echo "outbox-migration-ratchet: FAIL"
  exit 1
fi
lint_scanned outbox-migration-ratchet "$crates_scanned" "crate src tree(s) under crates/"
echo "outbox-migration-ratchet: OK"
