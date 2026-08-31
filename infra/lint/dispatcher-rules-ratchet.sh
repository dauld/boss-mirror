#!/usr/bin/env bash
#
# dispatcher-rules-ratchet — the shrink-only guard on reactive rules
# (protocol-policy-publish.md, the rule census).
#
# THE PRINCIPLE
# -------------
# Under the 3P admission edge, a dispatcher rule is a reaction the
# protocol definition could not express. The census classed the 38
# rules of 2026-08-12: seven jobs-internal consequences that move
# into WorkflowSpec `on` blocks whole, ~22 domain effects that become
# admission-staged obligations, and nine external-glue reactions that
# stay. Every migration deletes its rule; nothing should quietly add
# one. So the roster is a ratchet: the count may fall, and any rise
# fails CI with this explanation in the output.
#
# THE CHECKED PROPERTY
# --------------------
# The number of `[[rule]]` entries in infra/dispatcher/rules.toml is
# <= the baseline recorded below. When a migration lands, lower the
# baseline in the same car — the same one-definition discipline the
# outbox ratchet used, and like it, this line is the entire state.
#
# A genuinely NEW reaction is still possible — timers, external
# ingress, cross-protocol reactors are legitimate residents — but it
# costs raising the baseline here, in a diff a reviewer sees, with a
# sentence in rules.toml saying why it cannot be a protocol
# consequence.
#
# Usage:  infra/lint/dispatcher-rules-ratchet.sh

set -euo pipefail

# 38 -> 40 (2026-08-13). Train #20 landed the two feedback-obligation
# reactors — `complete-feedback-branch-on-car-merged` and
# `notify-filer-on-feedback-terminal` — without raising this line,
# which nothing caught because forge CI did not yet run the gate. Both
# qualify under the cross-protocol-reactor exemption above and both
# carry their "why" in rules.toml: one advances a `user-feedback` job
# from a `ship-a-change` close, the other notifies the filer on ANY
# terminal. Neither can be declared as a consequence inside a single
# Workflow definition, because each spans two protocols by
# construction. Raised here rather than on that train because the
# violation only became visible when the gate was wired in.
#
# 42 -> 45 (2026-08-14, migration 122). Three more maintenance areas:
# `maintenance-sweep-build-caches-daily`,
# `maintenance-sweep-image-freshness-daily`,
# `maintenance-sweep-converge-lag-daily`.
#
# All three are CLOCK rules, which is the first of the dispatcher's
# three sanctioned roles under its narrowed charter (David, 2026-08-14:
# the dispatcher "is essentially the queue watcher for us now" — clock,
# threshold, matchmaking, and nothing else). They qualify under the
# timer exemption above for the reason that exemption exists: there is
# no Workflow whose definition could declare them, because nothing has
# happened yet. A sweep's whole point is to run when NO event fired.
#
# Note this ratchet counts in the right direction for once. It exists to
# stop routing leaking into the dispatcher, and these rules add none:
# every one of them only admits a packet, and what happens next is
# `maintenance-sweep`'s own protocol row. The number to watch is not
# this total but the 22 `step.done.*` rules underneath it, which ARE
# routing and are owed back to the protocol.
#
# 45 -> 46 (2026-08-14, migration 128). `expire-signals-on-job-closed`.
# A CROSS-PROTOCOL REACTOR, which is the exemption above and not the
# routing this ratchet exists to stop: no single Workflow definition
# can express "when a job of ANY kind closes, retire the inbox
# messages about it", because those messages are not part of the
# job's protocol — they belong to a different domain that merely
# observed it. Declaring it inside every Workflow would be the
# duplication, not the discipline.
# 46 -> 47 (2026-08-15, migration 129). `publish-to-github-daily`.
# A TIMER, the exemption above: "publish a batch to the public mirror
# once a day" is triggered by the clock, and there is no Workflow whose
# definition could declare it because no packet causes it. Note the
# rule was ALREADY seeded and running — this change only adds it to
# rules.toml, where it should have been from the start. The count rose
# because the file caught up with the registry, not because a new
# reaction was introduced, and the drift guard
# (`dispatcher_rules_seed_matches_toml`) is what forced the catch-up
# after it reddened the 13-car train 20260815-0621.
# 47 -> 48 (2026-08-16, migration 141). `design-review-level-sweep`.
# A TIMER, the exemption above, and the clearest case of it yet: the
# condition it watches is a standing STATE of the corpus — "docs with
# open questions and nobody reviewing them" — which no Workflow
# definition can declare because no packet causes it. That is not a
# technicality; asking a level question only on an edge IS the defect
# (ae8a14f7), and roughly twenty-three questions were unreachable
# because of it. It adds no routing: the sweep only admits packets the
# existing `design-review-spawn` rule would have admitted, and hands
# them to the same `jobs.spawn` args.
# 48 -> 49 (2026-08-16). `maintenance-sweep-cluster-conformance-daily`.
# A TIMER under the exemption above, and the same shape as the four
# maintenance sweeps already here: no packet causes "a day passed", so
# no Workflow definition can declare it. It adds no routing — it hands
# `jobs.spawn` the same maintenance-sweep args the other four use, with
# a different target.
#
# What it buys: infra/cluster/manifests/ describes what should be
# running and nothing checked that any of it was. boss-dev.yaml was
# merged, applied by hand, and a day later a design doc asserted in
# review that the pod had never run while it had 25 hours of uptime.
# The check that answers this existed as a script; a script nobody runs
# is not a mechanism. David, 2026-08-16: "let's try and get as much
# maintenance and management into job protocols rather than floating
# around scripts or system timers elsewhere."
# 49 -> 50 (2026-08-17). `maintenance-sweep-doc-status-daily`.
# A TIMER under the exemption above, same shape as the five sweeps
# already here: no packet causes "a day passed". It adds no routing —
# `jobs.spawn` with the same maintenance-sweep args, a different
# target. Buys a caller for /api/design/stale-statuses, which has
# existed and reported to nobody (feedback 0b8ae875).
# 50 -> 51 (2026-08-20, migration 152). `network-census-daily`.
# A TIMER under the exemption above, and the purest case: it spawns
# nothing and routes nothing — one firing writes one
# `jobs.network.census` event, the packet-loss census's measured
# series (packet-loss.md, decided 9fb9904f). The condition it watches
# is a standing state of the network — what is NOT moving — which no
# Workflow definition can declare, because no packet causes it; a
# census's whole point is to run when no event fired.
# 51 -> 52 (2026-08-30, migration 202608302200).
# `estate-compare-on-observation`. EXTERNAL INGRESS under the
# exemption above: `jobs.estate.observed` is posted by a CronJob
# looking at the cluster — no packet causes an observation, so no
# Workflow definition can declare the reaction to one. It adds no
# routing: one firing reads the estate registry and writes one
# `jobs.estate.compared` event, the compare half of the
# declared/observed split (59ef456a).
#
# 52 -> 53 (2026-08-31): maintenance-sweep-empty-decisions-daily — a
# TIMER, the ratchet's own first carve-out. No protocol can declare
# "notice that a completed decision recorded nothing" because the
# packets it watches are pinned to OLD versions whose contracts are
# frozen; a clock has to look. One firing spawns one maintenance-sweep
# packet, no routing (cdfe2e1a).
BASELINE=53
RULES_FILE="infra/dispatcher/rules.toml"

count=$(grep -c '^\[\[rule\]\]' "$RULES_FILE")

if (( count > BASELINE )); then
    echo "dispatcher-rules-ratchet: $RULES_FILE has $count rules; baseline is $BASELINE" >&2
    echo "" >&2
    echo "  A new dispatcher rule is a reaction the protocol definition could" >&2
    echo "  not express (protocol-policy-publish.md). If this reaction truly" >&2
    echo "  cannot be a protocol consequence (timer / external ingress /" >&2
    echo "  cross-protocol reactor), raise BASELINE in this script in the same" >&2
    echo "  change and say why next to the rule. Otherwise: declare it in the" >&2
    echo "  Workflow definition instead." >&2
    exit 1
fi

if (( count < BASELINE )); then
    echo "dispatcher-rules-ratchet: $count rules (baseline $BASELINE) — a migration landed; lower BASELINE to $count in the same car"
    # Advisory, not fatal: the migration car that forgets to tighten
    # the ratchet gets told, loudly, without blocking the migration.
fi

echo "dispatcher-rules-ratchet: OK ($count rules <= baseline $BASELINE)"
