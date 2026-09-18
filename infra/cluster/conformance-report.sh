#!/usr/bin/env bash
# conformance-report.sh — the cluster's orphan set, WITH ITS VERDICT.
#
# What the `conformance-report` ops verb runs (infra/ops/verbs/
# conformance-report.json), filed by the cluster-conformance sweep for
# itself the moment its Inspect step becomes ready
# (measure-cluster-conformance-sweep-on-inspect-ready). The answer is
# `infra/cluster/undeclared-objects.sh --list` — the ONE definition of
# the orphan set that the orphan lint and delete-orphan-object share —
# passed through untouched, and then ONE machine-readable line judging
# it, in the shape every sweep report ends with:
#
#   verdict: clean                       zero undeclared objects
#   verdict: <n> undeclared object(s)    the set above is the finding
#   verdict: unanswered (...)            the derivation could not answer
#
# WHY A WRAPPER (backlog 970c0c94, measured 2026-09-18). The verb ran
# the derivation directly and its answer landed on the ops-request as
# free text — exit 0 while reporting one undeclared object — so nothing
# could complete the sweep's Inspect step by rule, and it sat assigned
# to the agent for a day with the reading on another packet. The
# derivation's own contract cannot carry the verdict: in `--list` its
# STDOUT IS THE ORPHAN SET, one object per line, empty when clean
# (undeclared_objects_sh.rs pins that), and the lint reads it that way.
# So the verdict is drawn HERE, from the answer, and the derivation
# stays what it is. The dispatcher rule
# judge-cluster-conformance-sweep-on-report-answered reads the last
# `verdict:` line (maintenance.sweep.judge) and completes the sweep's
# Inspect step when it is clean.
#
# THE THRESHOLD IS ZERO, and it is not new: the orphan lint fails on
# any undeclared object, and delete-orphan-object derives its authority
# from the same set. The exit code passes through: 0 for an answer,
# finding or not (a finding is an answer, not a failure), and the
# derivation's own 4 for "cannot answer", so the verb's contract holds.
set -uo pipefail

# The derivation, overridable so a test can plant one and exercise the
# verdict against each shape of its contract without a cluster.
derivation="${BOSS_UNDECLARED_OBJECTS:-$(dirname "$0")/undeclared-objects.sh}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

"$derivation" --list > "$work/set" 2> "$work/said"
rc=$?
cat "$work/set"
cat "$work/said" >&2

if [ "$rc" -ne 0 ]; then
    # A refusal is neither clean nor a count. The derivation's reason
    # already rode stderr above; the verdict names the code so a reader
    # of the recorded output does not have to re-derive it.
    printf 'verdict: unanswered (undeclared-objects.sh --list exit %s; its reason is above)\n' "$rc"
    exit "$rc"
fi

n=$(grep -c . "$work/set" || true)
if [ "${n:-0}" -eq 0 ]; then
    printf 'verdict: clean\n'
elif [ "$n" -eq 1 ]; then
    printf 'verdict: 1 undeclared object\n'
else
    printf 'verdict: %s undeclared objects\n' "$n"
fi
exit 0
