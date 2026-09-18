#!/usr/bin/env bash
#
# idempotence-ratchet — the double-apply guard for accumulating state
# mutations.
#
# THE PRINCIPLE
# -------------
# The event bus is at-least-once. JetStream redelivers on NAK, on ack
# timeout, and on consumer restart, so every handler must be safe to
# run twice on the same event. Most writes are naturally idempotent
# because they set an absolute value — `SET status = $1` lands the
# same way however many times it runs.
#
# An ACCUMULATING mutation is not. `SET on_hand = on_hand + $2` moves
# the row further every time it executes, so one redelivered event
# silently doubles the stock movement. That is not hypothetical: it is
# the 2026-06-16 class where GL 1300/1320 decoupled from physical
# inventory because non-idempotent `on_hand` mutations double-applied
# on redelivery, and it produced no error anywhere — just numbers that
# stopped reconciling.
#
# Of the five correctness properties, idempotence was the one with
# tests but no static guard (conservation has four lints, provenance
# one, closure two; determinism is covered at runtime by the
# ledger-replay-check timer). This is that guard.
#
# THE CHECKED PROPERTY
# --------------------
# A ratchet, not a ban. Accumulating arithmetic is sometimes the right
# shape — a running total under a row lock is how stock actually
# behaves — so the rule is that every such site is REVIEWED, not that
# none exist. The known sites are listed below with why each is safe.
# A new one fails this lint until someone has looked at it and either
# made it idempotent or recorded why redelivery cannot reach it.
#
# ADDING A SITE
# -------------
# Ask first whether the write can be absolute instead. If it genuinely
# must accumulate, guard the handler (a dedup key, an event-id ledger,
# or an `ON CONFLICT DO NOTHING` on a natural key), then add the file
# here with a one-line reason.
set -euo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh

# Files permitted to carry accumulating mutations, with the reason.
#
#  - boss-inventory/src/postgres.rs — raw-material stock. `on_hand`
#    and `value_cents` accumulate under the row lock of a single
#    UPDATE; the receive path is reached from PO receipt, which
#    carries a natural key the caller dedups on.
#  - boss-products/src/postgres.rs — finished-goods stock, same shape
#    on the consume side.
#
# Both were the subject of the 2026-06-16 idempotency pass. They are
# listed because they are known and reviewed, NOT because accumulating
# writes are fine.
ALLOWED=(
    "crates/modules/boss-inventory/src/postgres.rs"
    "crates/modules/boss-products/src/postgres.rs"
)

search() {
    if command -v rg >/dev/null 2>&1; then
        rg -n --type rust '[a-z_]+ = [a-z_]+ *[+-] *\(?\$[0-9]+' crates/ || true
    else
        grep -rnE --include='*.rs' '[a-z_]+ = [a-z_]+ *[+-] *\(?\$[0-9]+' crates/ || true
    fi
}

violations=0
while IFS= read -r hit; do
    [ -z "$hit" ] && continue
    file="${hit%%:*}"
    # Tests may accumulate freely — they are not handlers.
    case "$file" in
        */tests/*) continue ;;
    esac
    allowed=0
    for a in "${ALLOWED[@]}"; do
        [ "$file" = "$a" ] && allowed=1 && break
    done
    if [ "$allowed" -eq 0 ]; then
        if [ "$violations" -eq 0 ]; then
            echo "idempotence-ratchet: accumulating state mutation in a file that has not been reviewed for redelivery:" >&2
            echo >&2
        fi
        echo "  $hit" >&2
        violations=$((violations + 1))
    fi
done < <(search)

if [ "$violations" -gt 0 ]; then
    cat >&2 <<'MSG'

An accumulating write (`col = col + $n`) applies twice if its event is
delivered twice, and the bus is at-least-once. Either:

  1. make the write absolute (`SET col = $n`) — usually possible once
     the handler computes the new total rather than the delta; or
  2. guard the handler so a redelivered event is a no-op (a dedup key,
     an event-id ledger, ON CONFLICT DO NOTHING on a natural key); or
  3. if redelivery genuinely cannot reach this path, add the file to
     ALLOWED in infra/lint/idempotence-ratchet.sh with the reason.

Silence here is the failure mode: a double-applied stock movement
raises nothing, it just stops reconciling with the GL.
MSG
    exit 1
fi

# Files READ, not sites found: the day the last accumulating write is
# made absolute, the tree is clean and the search still ran.
lint_scanned idempotence-ratchet "$(find crates -name '*.rs' -type f | wc -l | tr -d ' ')" "Rust file(s) under crates/, against ${#ALLOWED[@]} reviewed file(s)"
echo "idempotence-ratchet: clean (${#ALLOWED[@]} reviewed sites)"
