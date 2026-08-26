#!/usr/bin/env bash
# apply-script-configmap.sh — make the gate the tree says it is.
#
# WHY THIS EXISTS (2026-08-26). The gate Job cannot run run.sh out of
# the clone, because run.sh IS what performs the clone — so the script
# has to reach the pod some other way, and that way is ConfigMap
# `gate-runner-script`. gate-runner.yaml carried the regeneration
# command in a COMMENT and asked whoever applied it to remember.
#
# Nobody remembered, and nothing noticed. On 2026-08-26 the live
# ConfigMap held the run.sh from `perf/the-gate-uses-the-cpu-it-was-
# given` — a branch that was measured 16% slower and deliberately never
# parked (db127cb4) — while main carried a different script entirely.
# Ten gates ran that day and every receipt they issued was produced by
# a script that is not in the repository. The verdicts were sound (the
# manifest pins CARGO_BUILD_JOBS, so the rejected width never
# activated), but a receipt's claim is "the gate defined in this
# repository passed on this tree", and that first half was not true.
#
# CLAUDE.md 9a: a comment asking the next person to keep two copies in
# sync is not a mechanism. This is the mechanism. Run it whenever
# run.sh changes, and before a gate you intend to trust.
#
#   ./infra/gate-runner/apply-script-configmap.sh [namespace]
#
# --check exits non-zero if the live ConfigMap has drifted from the
# tree, without changing anything. That is the form to reach for when
# you are asking "is the thing that ran the thing I have?".
set -euo pipefail
cd "$(dirname "$0")/../.."

SCRIPT=infra/gate-runner/run.sh
NAME=gate-runner-script

CHECK=0
if [ "${1:-}" = "--check" ]; then
    CHECK=1
    shift
fi
NAMESPACE="${1:-boss-dev}"

[ -f "$SCRIPT" ] || { echo "apply-script-configmap: $SCRIPT not found" >&2; exit 1; }

rendered=$(kubectl -n "$NAMESPACE" create configmap "$NAME" \
    --from-file="run.sh=$SCRIPT" \
    --dry-run=client -o yaml)

if [ "$CHECK" = "1" ]; then
    # Compare the SCRIPT BODY, not the rendered YAML: the rendered form
    # carries metadata (creationTimestamp, resourceVersion) that differs
    # every time and would report drift on an identical script.
    live=$(kubectl -n "$NAMESPACE" get configmap "$NAME" -o jsonpath='{.data.run\.sh}' 2>/dev/null || true)
    if [ -z "$live" ]; then
        echo "apply-script-configmap: ConfigMap $NAME does not exist in $NAMESPACE." >&2
        echo "    A gate launched now would fail to start rather than run the wrong" >&2
        echo "    script, which is the safe direction — but it still cannot gate." >&2
        exit 1
    fi
    if [ "$live" = "$(cat "$SCRIPT")" ]; then
        echo "apply-script-configmap: $NAME matches $SCRIPT"
        exit 0
    fi
    echo "apply-script-configmap: DRIFT — $NAME does not match $SCRIPT." >&2
    echo "    The gate that issues receipts is not the gate in this tree." >&2
    echo "    Diff (live vs tree):" >&2
    diff <(printf '%s' "$live") "$SCRIPT" >&2 || true
    echo "    Fix with: $0 $NAMESPACE" >&2
    exit 1
fi

printf '%s\n' "$rendered" | kubectl apply -f -
echo "apply-script-configmap: $NAME in $NAMESPACE now matches $SCRIPT"
