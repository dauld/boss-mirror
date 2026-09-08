#!/usr/bin/env bash
# Daily inference-batch trigger for boss-ml's active heuristic-formula
# and declarative-rule models. Step 7 of the Phase 2 cutover hooks
# this into systemd via boss-ml-inference-batch.timer (see
# boss-ml-inference-batch.service alongside).
#
# Usage:
#   BOSS_ML_API_URL=http://127.0.0.1:7070 ./run-inference-batch.sh
#
# Iterates the model id list and POSTs infer-batch on each one. The
# response body (BatchInferReport) is logged but not parsed.
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

BASE="${BOSS_ML_API_URL:-http://127.0.0.1:7070}"
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

# Run one model's infer-batch. Returns non-zero on failure without
# aborting the caller — `set -e` does not fire on the command tested
# by an `if`.
run_model() {
  local id="$1"
  echo "==> infer-batch ${id}"
  if "${CURL}" -sSf -X POST "${BASE}/api/ml/models/${id}/infer-batch"; then
    echo
    return 0
  fi
  echo
  echo "inference-batch: infer-batch FAILED for ${id}" >&2
  return 1
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

if [ "${#failed[@]}" -gt 0 ] || [ "${#skipped[@]}" -gt 0 ]; then
  if [ "${#failed[@]}" -gt 0 ]; then
    echo "inference-batch: ${#failed[@]} model(s) failed: ${failed[*]}" >&2
  fi
  if [ "${#skipped[@]}" -gt 0 ]; then
    echo "inference-batch: ${#skipped[@]} model(s) skipped (stale dependency): ${skipped[*]}" >&2
  fi
  exit 1
fi

echo "inference-batch: all ${#MODELS[@]} models succeeded"
exit 0
