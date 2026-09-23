#!/usr/bin/env bash
# Daily inference-batch trigger for boss-ml's active heuristic-formula
# and declarative-rule models. Step 7 of the Phase 2 cutover hooks
# this into systemd via boss-ml-inference-batch.timer (see
# boss-ml-inference-batch.service alongside).
#
# Usage:
#   BOSS_ML_API_URL=http://<instance>:7070 ./run-inference-batch.sh
#
# Iterates the model id list and POSTs infer-batch on each. Each
# response is a BatchInferReport (crates/core/boss-ml/src/inference.rs:
# model_id, written, skipped, errors) and is logged whole AND read for
# its `written` count.
#
# WHERE, AND HOW MANY (backlog 9599babc, 2026-09-23). This script read
# `${BOSS_ML_API_URL:-http://127.0.0.1:7070}` and its unit pinned the
# same loopback address. On boss-gcp that is the retired second stack's
# ML API over the retired stack's database (ops-request 7912c9ae stopped
# it on 2026-09-15; the unit's own `Requires=boss-ml-api.service` started
# it again every night at 02:30). Every POST answered 2xx, so seven
# nightly packets closed `result=ok` while the system of record's
# risk-scores answered `total_scored: 0` against a directory holding an
# account. `ok` was true of the HTTP calls and said nothing about where
# the predictions went or how many there were. So now:
#   * NO DEFAULT. Without BOSS_ML_API_URL the script refuses, exit 78
#     (EX_CONFIG), the way boss-step.sh and the wrap refuse without
#     BOSS_JOBS_URL. The unit reads it from /etc/boss/sor.env, rendered
#     from infra/estate/estate.toml as the record's host on boss-ports'
#     `ml` port (infra/estate/render-sor-env.sh).
#   * A 2xx WITHOUT A COUNT IS A FAILED MODEL. Whatever answered is not
#     the ML API this batch means — a wrong target answers instead of
#     erroring (CLAUDE.md §Doors).
#   * THE PACKET CARRIES THE COUNT. The run writes predictions_written,
#     predictions_by_model, ml_api_url, the failed and skipped models and
#     its own exit_status to $BOSS_RUN_SUMMARY_FILE (infra/run-summary.sh),
#     which the unit's ExecStopPost merges onto the `run` step.
#   * A ZERO IS LOUD. A run in which every model answered and none wrote
#     a single prediction exits 1: that is the shape of a batch scoring a
#     database nobody reads, and it lands the packet on `failed`.
#
# BEST-EFFORT PER MODEL. The models are independent: one model's
# infer-batch failing must not skip every model after it and leave
# their predictions stale. So each model runs, its outcome is
# captured, and the loop continues on failure (logging to stderr
# which model failed). The ONE exception is the real ordering
# dependency below. At the end the script exits non-zero if ANY model
# failed or was skipped, so systemd (Type=oneshot) and the
# maintenance packet still record a degraded run.
#
# `curl` is overridable via BOSS_ML_CURL purely so the co-located test
# (crates/core/boss-testing/tests/run_inference_batch_sh.rs) can drive
# the loop without a live ML API. Production leaves it unset.

set -euo pipefail

# shellcheck source=infra/run-summary.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/run-summary.sh"

# Forget anything an earlier run left, before this one can refuse, and
# record this run's exit however it ends.
run_summary_reset
trap 'run_summary_field exit_status "$?"' EXIT

BASE="${BOSS_ML_API_URL:-}"
if [ -z "$BASE" ]; then
  echo "inference-batch: BOSS_ML_API_URL is not set, and there is no safe default." >&2
  echo "    A 127.0.0.1:7070 default is how this batch spent a week scoring the retired" >&2
  echo "    second stack's database while every packet said ok (backlog 9599babc)." >&2
  echo "    The unit reads it from /etc/boss/sor.env (infra/estate/render-sor-env.sh)." >&2
  exit 78
fi
run_summary_field ml_api_url "$BASE"
CURL="${BOSS_ML_CURL:-curl}"

MODELS=(
  # Order matters: the churn-risk plugin must populate
  # ml_predictions BEFORE next-action-high-churn-risk reads them.
  mdl-account-churn-risk-v1
  mdl-next-action-contract-expiring-v1
  mdl-next-action-past-due-invoice-v1
  mdl-next-action-missing-primary-contact-v1
  mdl-next-action-high-churn-risk-v1
  mdl-next-action-stalled-service-ticket-v1
  # Renamed from mdl-next-action-pm-visit-due-v1 in v1.0.6 ML
  # plugin refresh; registry now exposes the verbose name.
  mdl-next-action-preventive-maintenance-due-v1
)

# The one real ordering dependency, kept explicit: high-churn-risk
# reads the predictions account-churn-risk writes. If churn-risk did
# not succeed this run, running high-churn-risk would stamp fresh
# timestamps onto stale input, so it is skipped instead.
CHURN_MODEL="mdl-account-churn-risk-v1"
DEPENDENT_MODEL="mdl-next-action-high-churn-risk-v1"

churn_ok=0
failed=()
skipped=()
total_written=0
total_errors=0
by_model='{}'

# Run one model's infer-batch and read its count. Returns non-zero on
# failure without aborting the caller — `set -e` does not fire on the
# command tested by an `if`.
run_model() {
  local id="$1" body written errors
  echo "==> infer-batch ${id}"
  if ! body="$("${CURL}" -sSf -X POST "${BASE}/api/ml/models/${id}/infer-batch")"; then
    echo
    echo "inference-batch: infer-batch FAILED for ${id}" >&2
    return 1
  fi
  printf '%s\n' "$body"
  written="$(printf '%s' "$body" | jq -r 'select(type == "object") | .written | select(type == "number")' 2>/dev/null || true)"
  case ${written:-empty} in
    empty|*[!0-9]*)
      echo "inference-batch: ${id} answered 2xx at ${BASE} but not a BatchInferReport (no numeric written) — not the ML API this batch means" >&2
      return 1
      ;;
  esac
  errors="$(printf '%s' "$body" | jq -r '(.errors // []) | length' 2>/dev/null || true)"
  case ${errors:-empty} in empty|*[!0-9]*) errors=0 ;; esac
  total_written=$((total_written + written))
  total_errors=$((total_errors + errors))
  by_model="$(printf '%s' "$by_model" | jq -c --arg k "$id" --argjson v "$written" '. + {($k): $v}')"
  echo "inference-batch: ${id} wrote ${written} prediction(s), ${errors} error(s)"
  return 0
}

for id in "${MODELS[@]}"; do
  # Dependency guard: skip the dependent model if its upstream did not
  # succeed this run. Running it on stale input would emit
  # fresh-stamped stale predictions.
  if [ "${id}" = "${DEPENDENT_MODEL}" ] && [ "${churn_ok}" -ne 1 ]; then
    echo "inference-batch: SKIP ${DEPENDENT_MODEL} — ${CHURN_MODEL} did not \
succeed this run; running it on stale input would emit fresh-stamped stale \
predictions" >&2
    skipped+=("${id}")
    continue
  fi

  if run_model "${id}"; then
    if [ "${id}" = "${CHURN_MODEL}" ]; then
      churn_ok=1
    fi
  else
    failed+=("${id}")
  fi
done

run_summary_field predictions_written "$total_written"
run_summary_field prediction_errors "$total_errors"
run_summary_json predictions_by_model "$by_model"
[ "${#failed[@]}" -eq 0 ] || run_summary_field models_failed "${failed[*]}"
[ "${#skipped[@]}" -eq 0 ] || run_summary_field models_skipped "${skipped[*]}"

if [ "${#failed[@]}" -gt 0 ] || [ "${#skipped[@]}" -gt 0 ]; then
  if [ "${#failed[@]}" -gt 0 ]; then
    echo "inference-batch: ${#failed[@]} model(s) failed: ${failed[*]}" >&2
  fi
  if [ "${#skipped[@]}" -gt 0 ]; then
    echo "inference-batch: ${#skipped[@]} model(s) skipped (stale dependency): ${skipped[*]}" >&2
  fi
  echo "inference-batch: ${total_written} predictions written at ${BASE}" >&2
  exit 1
fi

if [ "$total_written" -eq 0 ]; then
  echo "inference-batch: 0 predictions written across ${#MODELS[@]} models at ${BASE} — every model answered and none scored anything; a batch that writes nothing is not a success (backlog 9599babc: the wrong instance answers exactly like this)" >&2
  exit 1
fi

echo "inference-batch: all ${#MODELS[@]} models succeeded, ${total_written} predictions written at ${BASE}"
exit 0
