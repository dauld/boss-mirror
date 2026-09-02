#!/usr/bin/env bash
# Forbid chrono::Utc::now() outside the allowlist of crates that
# legitimately need wall-clock access. Matches `Utc::now` on a word
# boundary, so it catches the called forms `chrono::Utc::now()` /
# bare `Utc::now()` AND the function-reference form
# `unwrap_or_else(Utc::now)` — the latter leaks identically (it gets
# invoked with no args later) and previously slipped past a
# parens-requiring regex into a projection rebuilder.
#
# Why this exists — updated for the sim-stamp retirement (David,
# 2026-08-22, packet a7a4cae5). Two rules now share this gate:
#
#   1. RECORD STAMPS ARE WALL TIME, minted centrally. `EventStamp`
#      (crates/core/boss-core/src/publisher.rs) and
#      `boss_clock_client::wall_now()` are the only sanctioned
#      sources of an `Event.timestamp` / `audit_log.timestamp`.
#      Handlers do not mint their own second wall reading — the row
#      bind and the record must share ONE instant (`stamp.timestamp`)
#      or replay-diff reproduces a different row than the live write.
#   2. BUSINESS DATES ROUTE THROUGH THE CLOCK PORT. "What day is it
#      in this company's timeline" (`happened_on`, `issued_on`,
#      `completed_on` defaults, aging buckets, `{day}` tokens) still
#      reads `state.clock.now().await.now` — sim-aware by design.
#      A stray Utc::now() in domain-date logic desyncs the demo's
#      business timeline the same way it always did (the class that
#      surfaced 116k mis-dated rows in regen 18, closed by v1.0.5
#      #68). This lint keeps that leak class from recurring.
#
# Allowlist: only these paths may call Utc::now()
#   * crates/core/boss-clock/         (the clock-api itself —
#                                      wall-mode IS Utc::now())
#   * crates/core/boss-clock-client/  (WallClockClient, the transport-
#                                      error fallback, and `wall_now()`
#                                      — THE sanctioned record-stamp
#                                      source outside EventStamp)
#   * crates/core/boss-core/src/publisher.rs
#                                     (EventStamp mints the record
#                                      stamp — wall by decision)
#   * **/tests/                       (test files; no production
#                                      runtime)
#   * #[cfg(test)] items              (inline test blocks — the
#                                      exemption is the attributed
#                                      item's BODY, brace-counted; it
#                                      used to run to end-of-file and
#                                      swallowed three production
#                                      callsites, see
#                                      `in_cfg_test_item` below)
#   * cli boundary tools (e.g. boss-cli emit) — call with the path
#     argument to the lint when extending
#
# Usage:
#   ./infra/lint/no-wallclock.sh              # scan the tree
#   ./infra/lint/no-wallclock.sh --self-test  # pin the cfg(test) rule
#
# Exit 0 = clean, 1 = violations.
#
# Wire into CI by adding to the PR-time check matrix.

set -euo pipefail

cd "$(dirname "$0")/../.."

# Path-prefix patterns that are allowed.
#
# Adding to this list: the bar is "this path's Utc::now() never
# stamps an audit_log row." Authentication tokens, internal queue
# claim timestamps, runtime dispatcher cost-window math,
# diagnostic checkpoints — none of those route through the
# Event::new + DomainPublisher pipeline that lands in audit_log,
# so wallclock there is correct.
#
# When in doubt, leave it OUT and audit the callsite. The cost
# of a false-positive flag is a 30-second look; the cost of a
# silent wallclock leak is a regen 18 (116k mis-dated rows).
ALLOWED_PREFIXES=(
  # The clock-api itself, the wall-mode path that IS Utc::now()
  "crates/core/boss-clock/"
  # WallClockClient + ReqwestClockClient's transport-error fallback +
  # wall_now(), the sanctioned record-stamp source.
  "crates/core/boss-clock-client/"
  # EventStamp mints the record stamp — wall-clock by decision
  # (sim time retired from the record, David 2026-08-22, packet
  # a7a4cae5). The one place Event timestamps are born for live
  # emits.
  "crates/core/boss-core/src/publisher.rs"
  # Auth tokens — wallclock IS the right semantic for token
  # freshness checks (jwt-style "now > exp"). Doesn't emit
  # audit_log; manages local in-process auth state only.
  "crates/core/boss-gateway/src/local_auth.rs"
  # Perf metrics — diagnostic only, never emitted as audit
  "crates/core/boss-gateway/src/perf.rs"
  # Cybernetics-layer queue + dispatcher + cost-ledger
  # operational state. claim_at, started_at, finished_at,
  # cost-window math, all internal runtime accounting that
  # never lands in audit_log.
  "crates/core/boss-events/src/queue.rs"
  "crates/core/boss-events/src/dispatcher.rs"
  "crates/core/boss-events/src/ledger.rs"
  "crates/core/boss-events/src/claude_dispatcher.rs"
  # Diagnostic CLI — checkpoint timestamps in operator output.
  "crates/core/boss-events/src/bin/boss_audit_integrity_check.rs"
  # Deprecated clock helpers, pre-Clock-as-service. The module
  # itself still exists for backward compat but new code should
  # use boss_clock_client::ClockClient.
  "crates/core/boss-core/src/clock.rs"
  # Policy rule-engine evaluator: rule expiry math is local
  # decision logic, not audit-emitting. Post-#84 the engine
  # itself lives in boss-policy-client; boss-policy is now just
  # the HTTP + Postgres adapter (also allowed because the
  # bootstrap binary uses chrono parse helpers).
  "crates/core/boss-policy/"
  "crates/core/boss-policy-client/src/engine.rs"
  "crates/core/boss-policy-client/src/in_memory.rs"
  "crates/core/boss-policy-client/src/seed_loader.rs"
  # Calendar port.rs trait-default convenience overloads.
  # The _at variants are what production code calls; the
  # bare-arg defaults exist for legacy callers.
  "crates/core/boss-calendar/src/port.rs"
  # Same pattern for the other port.rs default-method overloads.
  "crates/modules/boss-people/src/port.rs"
  "crates/modules/boss-commerce/src/port.rs"
  "crates/modules/boss-shipping/src/port.rs"
  "crates/modules/boss-inventory/src/port.rs"
  "crates/core/boss-content/src/port.rs"
  # Classes client deprecated wallclock helper.
  "crates/core/boss-classes-client/src/lib.rs"
  # Boss-docs review system: in-memory + reindex are internal
  # state for the design-doc review workflow. http.rs has tests
  # inline (covered by tests/ filter where appropriate).
  "crates/core/boss-docs/"
  # Sim-side faker + bridge generators — sim-only; the audit
  # trail comes from the LiveApiOutput emit path which IS
  # clock-routed (see v1.0.6 #72 scheduler-driven anchors).
  "crates/orchestrators/boss-sim/src/shape_driven/faker.rs"
  "crates/modules/boss-assets/src/bridge.rs"
  # Fleet system_parts uses Utc::now() for in-memory updated_at;
  # the audit-emitting handler path is separate + clock-routed.
  "crates/modules/boss-assets/src/system_parts.rs"
  # In-memory + test fixture adapters; never deployed.
  "crates/core/boss-content/src/in_memory.rs"
  "crates/modules/boss-inventory/src/in_memory.rs"
  "crates/core/boss-policy/src/in_memory.rs"
  # ML inference + generators: predictions land in
  # ml_predictions table, not audit_log. created_at /
  # updated_at on those rows is wallclock by design.
  "crates/core/boss-ml/"
  "crates/modules/boss-ml-plugins/"
  # CLI flag defaults (today = Utc::now().date_naive() unless
  # --today is passed). Operators override at runtime.
  "crates/modules/boss-ledger/src/bin/boss_ledger_recognize.rs"
  # Demo synthetic agent loop — wallclock is the intended
  # source; events feed the /ops live ticker, never audit_log.
  "crates/core/boss-observability/src/demo_agents.rs"
  # Sim output: shape_driven engine wires sim-time anchors via
  # clock-api in the LiveApiOutput path (#72). The shape_driven
  # / output.rs core uses Utc::now() in test/in-memory paths
  # only; LiveApiOutput goes through advance_clock_to_instant.
  "crates/orchestrators/boss-sim/src/output.rs"
  # boss-jobs registry + step-plugin row creation timestamps
  # (created_at on WorkflowSpec, StepPluginSpec rows). These
  # are reference-data rows in the platform registry — not
  # audit-emitting sim/operations work.
  "crates/core/boss-jobs/src/registry.rs"
  "crates/core/boss-jobs/src/step_plugins.rs"
  # Scheduling fallbacks: HTTP handlers use Utc::now() as the
  # default when ?from query param is absent (operator viewing
  # the calendar wants "now-ish"). Materialize daemon uses it
  # for the next-week computation.
  "crates/core/boss-jobs/src/scheduling/materialize.rs"
  "crates/core/boss-jobs/src/scheduling/http.rs"
  # Internal data struct default constructor — Message, etc.
  "crates/core/boss-core/src/agent.rs"
  # Rebuilders walk historic audit_log entries; the read_at
  # backfill timestamp is wallclock by design (synthetic since
  # the original event didn't carry it).
  "crates/modules/boss-messages/src/rebuild.rs"
  # Daily ledger recognition cron — operator-time tick.
  "crates/modules/boss-ledger/src/recognize.rs"
)

# Files whose entire body is exempt (e.g. CLI boundary tools
# where wallclock is the intent; the boss-cli emit subcommand
# constructs an Event for stdout inspection — never published —
# and the operator's wallclock IS the intended timestamp).
ALLOWED_FILES=(
  "crates/orchestrators/boss-cli/src/main.rs"
  # docs_flush is a CLI snapshot tool, runs at operator request
  "crates/orchestrators/boss-cli/src/docs_flush.rs"
  # The train conductor, same class as its two siblings above and
  # adjudicated on the heuristic repair that first made it visible
  # (packet 5cccfbee, 2026-08-23). It stamps `completed_at` into a
  # step's metadata payload because a step carries only a completion
  # DATE and the arrival report's timings are real durations of real
  # CI runs. Wall is the only reading that means anything there: a
  # conductor run is IT-department work in the real world, not a date
  # in the brewery's simulated calendar, and the Job it writes to is
  # non-simulated. Rule 1 is untouched — this is a payload field, and
  # the record stamp is still minted by EventStamp.
  "crates/orchestrators/boss-cli/src/train.rs"
  # The sim binary's `went_live=` marker on the regenerate-deployment
  # backfill step: when did this regen actually go live, in real time.
  # Deployment bookkeeping on a real Job, not a brewery business date
  # — the same reason this file is already allowed by the SQL pass
  # below. Surfaced by the same heuristic repair; its mid-file
  # `mod day_cursor_tests` was what hid it.
  "crates/tenants/boss-brewery-engine/src/bin/boss_brewery_sim.rs"
  # Operator-baseline seed: stamps via BOSS_EPOCH_START now
  # (v1.0.5 fix), but the binary's own clock fallback is OK
  "crates/modules/boss-people/src/bin/boss_operator_baseline_seed.rs"
  # Brewery-engine seed + bootstrap: use BOSS_EPOCH_START
  # explicitly now (#69); main.rs imports chrono just for type
  # signatures, not Utc::now() calls.
  "crates/tenants/boss-brewery-engine/src/bin/boss_brewery_data_seed.rs"
  "crates/tenants/boss-brewery-engine/src/bin/boss_brewery_bootstrap.rs"
  # File-references GC daemon: wallclock IS the right "now"
  # for deciding whether soft-deleted bytes are past their
  # retention window. Operator-time semantics.
  "crates/core/boss-content/src/bin/boss_files_gc.rs"
)

is_allowed() {
  local file="$1"
  # Test files anywhere are fine.
  if [[ "$file" == *"/tests/"* ]]; then
    return 0
  fi
  # In-memory adapters are by convention test fixtures (the prod
  # path uses the postgres adapter); the in_memory.rs naming
  # convention is consistent across the workspace. Allow these
  # crate-wide — if a non-test in-memory caller needs review,
  # demote individually.
  if [[ "$file" == *"/in_memory.rs" ]]; then
    return 0
  fi
  # Trait port.rs defaults are convenience overloads that
  # delegate to _at variants. New code should call _at directly;
  # the bare overloads exist for legacy callers + tests.
  if [[ "$file" == *"/port.rs" ]]; then
    return 0
  fi
  # Doc-comment lines that reference Utc::now() in prose (not code)
  # land in `///` blocks; we filter those out via the grep below
  # so they never reach this gate.
  for prefix in "${ALLOWED_PREFIXES[@]}"; do
    if [[ "$file" == "$prefix"* ]]; then
      return 0
    fi
  done
  for allowed in "${ALLOWED_FILES[@]}"; do
    if [[ "$file" == "$allowed" ]]; then
      return 0
    fi
  done
  return 1
}

# rg if available (much faster on large trees), grep -r as fallback.
if command -v rg >/dev/null 2>&1; then
  search() { rg -n --type rust 'Utc::now\b' crates/ "$@"; }
else
  search() {
    grep -rn --include='*.rs' -E 'Utc::now\b' crates/ "$@" || true
  }
fi

# Is FILE:LINE inside the body of a `#[cfg(test)]` item? Prints
# "test" or "prod".
#
# THE BUG THIS REPLACES (packet 5cccfbee). The old rule was "find the
# nearest preceding `#[cfg(test)]`; if there is one, the hit is test
# code." A `#[cfg(test)]` attribute applies to ONE item, but that rule
# never closed the region, so the first one in a file exempted every
# line after it — silently, with the lint still printing "clean".
# Three production callsites were swallowed that way, and none of them
# was in a test:
#
#   boss-cli/src/train.rs:465        #[cfg(test)] on a const fn
#                                    → hid line 2376, 1900 lines later
#   boss-cli/src/docs_flush.rs:790   #[cfg(test)] on a fn
#                                    → hid `today()` six lines later
#   boss_brewery_sim.rs:1226         a MID-FILE `mod day_cursor_tests`
#                                    → hid line 1563
#
# A lint whose exemption has no end is not an allowlist, it is a
# silence, and this one grew every time somebody added an inline test
# helper high in a file. All three are adjudicated below now — two as
# explicit ALLOWED_FILES entries with a reason, one already allowed —
# which is the mechanism the header promises.
#
# THE RULE NOW: the exemption is exactly the attributed item's body,
# found by counting braces from the item header to its match. Nothing
# outside a `#[cfg(test)]` item can be exempted by this pass, so a new
# leak anywhere else is loud on the run that introduces it. Bodiless
# items (`#[cfg(test)] use ...;`) end at their semicolon. Pinned by
# `--self-test`.
in_cfg_test_item() {
  awk -v target="$2" '
    function braces(s,   t, o, c) {
      t = s; o = gsub(/\{/, "", t)
      t = s; c = gsub(/\}/, "", t)
      return o - c
    }
    {
      seg = $0
      if (state == "" || state == "out") {
        if ($0 ~ /^[[:space:]]*#\[cfg\(test\)\]/) {
          state = "pending"
          # Anything trailing on the attribute line is the item start
          # (`#[cfg(test)] mod tests {` on one line).
          sub(/^[[:space:]]*#\[cfg\(test\)\]/, "", seg)
        } else {
          seg = ""
        }
      } else if (state == "pending") {
        # Stacked attributes, doc comments and blanks sit between the
        # attribute and the item it applies to.
        if ($0 ~ /^[[:space:]]*(#\[|\/\/|$)/) {
          seg = ""
        } else {
          state = "item"; depth = 0; opened = 0
        }
      }
      if (state == "pending" && seg ~ /[^[:space:]]/) {
        state = "item"; depth = 0; opened = 0
      }
      if (state == "item") {
        depth += braces(seg)
        if (seg ~ /\{/) { opened = 1 }
        if (opened && depth <= 0) { ending = 1 }
        else if (!opened && seg ~ /;[[:space:]]*$/) { ending = 1 }
        else { ending = 0 }
      }
      if (NR == target) { print (state == "item" ? "test" : "prod"); exit }
      if (state == "item" && ending) { state = "out"; ending = 0 }
    }
  ' "$1" 2>/dev/null || echo "prod"
}

# --self-test: the classifier against a fixture carrying both shapes
# that used to swallow a file. Runs without touching the tree.
if [ "${1:-}" = "--self-test" ]; then
  fixture=$(mktemp -t no-wallclock-selftest.XXXXXX)
  trap 'rm -f "$fixture"' EXIT
  cat >"$fixture" <<'FIXTURE'
use chrono::Utc;

impl Policy {
    /// A test-only constructor. FUNCTION-level `#[cfg(test)]`, not a
    /// module — the item ends at its closing brace.
    #[cfg(test)]
    pub(crate) const fn immediate() -> Self {
        Policy
    }
}

fn production_leak() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod mid_file_tests {
    use super::*;
    #[test]
    fn t() {
        let _ = Utc::now();
    }
}

fn second_production_leak() -> String {
    Utc::now().to_rfc3339()
}
FIXTURE
  # line 13 production, line 21 inside the test mod, line 26 production
  # again — the case a never-closing exemption gets wrong.
  st_fail=0
  for probe in "13:prod" "21:test" "26:prod"; do
    ln="${probe%%:*}"; want="${probe##*:}"
    got=$(in_cfg_test_item "$fixture" "$ln")
    if [ "$got" != "$want" ]; then
      echo "self-test FAIL: line $ln classified '$got', expected '$want'"
      st_fail=1
    fi
  done
  if [ "$st_fail" -eq 0 ]; then
    echo "no-wallclock --self-test: ok (cfg(test) exemption is bounded to the item)"
    exit 0
  fi
  echo "no-wallclock --self-test: FAILED — the cfg(test) exemption is leaking past the item it applies to."
  exit 1
fi

# Drop doc-comment lines (`///` or `//` with Utc::now in prose).
# Match `Utc::now` on a word boundary: catches `chrono::Utc::now()`,
# bare `Utc::now()` (after `use chrono::Utc;`), and the
# function-reference form `unwrap_or_else(Utc::now)`. The `\b`
# excludes identifiers like `now_from`. All leak the same way.
raw_hits=$(search | grep -v '^[^:]*:\s*[0-9]*:\s*//' || true)

hits=""
while IFS= read -r line; do
  [ -z "$line" ] && continue
  file="${line%%:*}"
  rest="${line#*:}"
  lineno="${rest%%:*}"
  if [ "$(in_cfg_test_item "$file" "$lineno")" = "test" ]; then
    continue
  fi
  hits+="$line"$'\n'
done <<<"$raw_hits"

violations=0
violation_lines=()
while IFS= read -r line; do
  [ -z "$line" ] && continue
  # Lines look like: path/to/file.rs:LINE:code
  file="${line%%:*}"
  if ! is_allowed "$file"; then
    violation_lines+=("$line")
    violations=$((violations + 1))
  fi
done <<<"$hits"

if [ "$violations" -eq 0 ]; then
  echo "no-wallclock (Rust): clean"
else
  echo "no-wallclock (Rust): $violations violation(s) — these production callsites stamp wallclock instead of routing through state.clock.now():"
  echo
  for line in "${violation_lines[@]}"; do
    echo "  $line"
  done
  echo
  echo "Fix: replace with \`state.clock.now().await.now\` (in async handlers)"
  echo "or pass a DateTime<Utc> argument from a clock-routed caller."
  echo "Why, and the full allowlist: the header of this script."
  echo "History: v1.0.5 commit 1a862d9a, and #68 (116k mis-stamped rows)."
fi

# ============================================================
# SQL wallclock pass (#78 — v1.0.8)
# ============================================================
#
# The Rust pass above catches Utc::now() and friends. v1.0.7
# fixed two production cases where the leak was inside a SQL
# *string* — boss-commerce's AR aging used PostgreSQL's
# CURRENT_DATE directly (wallclock at the DB layer), and the
# boss-ml declarative rules + plugins did the same. The audit
# found nine more sites of the same shape.
#
# This pass flags SQL `CURRENT_DATE` / `CURRENT_TIMESTAMP` /
# `NOW()` inside Rust string literals where it's part of
# business-domain date arithmetic. Legitimate operational
# record-keeping (updated_at, created_at, deleted_at,
# closed_at, settled_at, expires_at lifecycle columns) is
# allowlisted on the line itself — these are wall-time
# concerns by design (when did the row get touched), not
# happened-on facts.
#
# Allowlist categories on the line:
# - `<col>_at = NOW()` for the operational lifecycle columns
# - `expires_at [<>=]* NOW()` for policy/auth expiry checks
# - `COALESCE($N, NOW()|CURRENT_DATE)` — caller-supplied wins,
#   wallclock is just the fallback at the API boundary
sql_search() {
  if command -v rg >/dev/null 2>&1; then
    rg -n --type rust '(CURRENT_DATE|CURRENT_TIMESTAMP|\bNOW\s*\(\s*\))' crates/
  else
    grep -rn --include='*.rs' -E '(CURRENT_DATE|CURRENT_TIMESTAMP|\bNOW\s*\(\s*\))' crates/ || true
  fi
}

# Patterns on the same line that make a wallclock SQL call OK.
# Keep this list small and explicit. Any new column joins the
# allowlist only if the audit log doesn't read from it.
sql_allow_line() {
  local line_content="$1"
  # Operational lifecycle columns: when did the row get touched.
  # Not business-domain dates; never read into audit_log payloads.
  # delivered_at is the event-outbox relay's bookkeeping stamp — the
  # audit row's `timestamp` comes from the Event's own clock-routed
  # field; delivered_at only drives the pending-row filter.
  # first_seen_at/last_seen_at are spelled out because the `\b` above
  # means a bare `seen_at` alternative does NOT match the prefixed
  # forms (`_` is a word character, so there is no boundary before
  # `seen`). They are the design-doc indexer's bookkeeping — which
  # docs failed to parse and when it last tried — and are never read
  # into an audit_log payload.
  if echo "$line_content" | grep -qE '\b(updated_at|created_at|deleted_at|closed_at|settled_at|published|locked_at|paid_at|sent_at|read_at|expires_at|received_at|claimed_at|started_at|finished_at|completed_at|opened_at|seen_at|first_seen_at|last_seen_at|delivered_at)\s*=\s*NOW\s*\(\s*\)'; then
    return 0
  fi
  # The sim-clock epoch anchor: `wall_anchor` records the real-world
  # instant the sim epoch was primed. Wallclock is correct here by
  # definition — it's the fixed reference sim-time is measured *from*,
  # not a business date stamped onto any event.
  if echo "$line_content" | grep -qE 'wall_anchor\s*=\s*NOW\s*\(\s*\)'; then
    return 0
  fi
  # The sim-clock pause anchor: `paused_at` records the real-world
  # instant the current pause started — the wall reference boss-clock
  # uses to freeze sim-time (same class as wall_anchor). Matches both
  # the direct `= NOW()` (restart-epoch pause-claim) and the
  # `= CASE WHEN $1 THEN NOW() ELSE NULL END` pause/resume form. Never
  # a business date stamped onto any event.
  if echo "$line_content" | grep -qE 'paused_at\s*=\s*(NOW\s*\(\s*\)|CASE)'; then
    return 0
  fi
  # Expiry checks against wallclock (policy rules, tokens).
  if echo "$line_content" | grep -qE 'expires_at\s*[<>!=]+\s*NOW\s*\(\s*\)'; then
    return 0
  fi
  # COALESCE($N, NOW()|CURRENT_DATE) — caller supplies, fallback
  # is wallclock at the API boundary. The inner $N is the
  # clock-aware path.
  if echo "$line_content" | grep -qE 'COALESCE\s*\(\s*\$[0-9]+,\s*(NOW\s*\(\s*\)|CURRENT_DATE)\s*\)'; then
    return 0
  fi
  # Sequence + UUID generation columns appearing alongside NOW()
  # on the same line (e.g. `gen_random_uuid(), NOW(), ...` in
  # tests — covered by tests/ path filter above, but tolerate
  # in case the same pattern surfaces in dev tools).
  if echo "$line_content" | grep -qE 'gen_random_uuid|nextval'; then
    return 0
  fi
  # INSERT/UPSERT VALUES tuple where NOW() is a positional column
  # value (the column declaration is on the previous line).
  # Pattern: `VALUES (...positional args..., NOW())`. The column
  # name is in the INSERT column list above — these are always
  # operational `_at` columns when NOW() appears in this position.
  if echo "$line_content" | grep -qE 'VALUES\s*\([^)]*NOW\s*\(\s*\)\s*\)'; then
    return 0
  fi
  return 1
}

# File-level allowlist for SQL wallclock — files whose entire
# SQL surface is operational/record-keeping or pre-Clock-as-service
# scaffolding the rest of the codebase doesn't depend on yet.
sql_is_file_allowed() {
  local file="$1"
  # Same blanket allow as Rust pass for clock crates, in_memory,
  # port.rs, tests, ml (predictions land in own table not audit_log).
  if [[ "$file" == *"/tests/"* ]]; then return 0; fi
  if [[ "$file" == *"/in_memory.rs" ]]; then return 0; fi
  if [[ "$file" == *"/port.rs" ]]; then return 0; fi
  case "$file" in
    crates/core/boss-clock/*) return 0 ;;
    crates/core/boss-clock-client/*) return 0 ;;
    crates/core/boss-ml/*) return 0 ;;
    # boss-ml-plugins removed (#86) — ClockClient is threaded
    # through InferenceDispatcher's ctx.now; plugins source today
    # from ctx.now and bind it as a SQL param. Any new wallclock
    # here is a leak.
    # boss-policy SQL is operational + rule-expiry; the audit
    # confirmed nothing here writes to audit_log.
    crates/core/boss-policy/*) return 0 ;;
    # Post-#84 (P9 hexagonal split), the policy engine lives in
    # boss-policy-client. Its `check` method needs wallclock to
    # test whether user overrides are currently active per their
    # `expires_at` field — operator-controlled expiry, not
    # sim-time. Not audit-emitting.
    crates/core/boss-policy-client/src/engine.rs) return 0 ;;
    crates/core/boss-policy-client/src/in_memory.rs) return 0 ;;
    crates/core/boss-policy-client/src/predicates.rs) return 0 ;;
    crates/core/boss-policy-client/src/seed_loader.rs) return 0 ;;
    crates/core/boss-policy-client/src/defaults.rs) return 0 ;;
    # scheduling/postgres.rs + scheduling/rebuild.rs left this list
    # 2026-09-02 (packet d7b8158e): assignment status + calendar-token
    # writes now bind stamp.timestamp / ev.ts, so replay reproduces
    # the rows byte-identically. Any new NOW() there is a leak.
    # Registry row-creation timestamps (reference data, not audit).
    crates/core/boss-jobs/src/registry.rs) return 0 ;;
    # Messages-events purge daemon — operates on the mutable
    # messages_events log, not audit_log.
    crates/modules/boss-messages/src/bin/boss_messages_events_purge.rs) return 0 ;;
    # Sim binary writes sim_clock metadata; the bus-side audit
    # comes from elsewhere.
    crates/tenants/boss-brewery-engine/src/bin/boss_brewery_sim.rs) return 0 ;;
  esac
  return 1
}

raw_sql=$(sql_search | grep -v '^[^:]*:\s*[0-9]*:\s*//' || true)
sql_violations=0
sql_violation_lines=()
while IFS= read -r line; do
  [ -z "$line" ] && continue
  file="${line%%:*}"
  if sql_is_file_allowed "$file"; then
    continue
  fi
  # Strip the file:line: prefix to get the code content.
  code="${line#*:}"
  code="${code#*:}"
  if sql_allow_line "$code"; then
    continue
  fi
  sql_violation_lines+=("$line")
  sql_violations=$((sql_violations + 1))
done <<<"$raw_sql"

if [ "$sql_violations" -eq 0 ]; then
  echo "no-wallclock (SQL): clean"
else
  echo "no-wallclock (SQL): $sql_violations violation(s) — business-domain SQL reads wallclock instead of binding from ClockClient:"
  echo
  for line in "${sql_violation_lines[@]}"; do
    echo "  $line"
  done
  echo
  echo "Fix: bind \`today\` as a SQL parameter (\`\$1::date\`) sourced from"
  echo "\`state.clock.now().await.now.date_naive()\`. Same pattern as"
  echo "v1.0.7 commit c70455e5 (boss-commerce AR aging) and a39bec22 (boss-ml)."
fi

total=$((violations + sql_violations))
if [ "$total" -gt 0 ]; then
  exit 1
fi
exit 0
