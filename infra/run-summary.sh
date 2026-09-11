# run-summary.sh — what a unit's run leaves for its own packet.
#
# WHY THIS EXISTS, measured 2026-09-11. `boss-gcp-converge` runs
# `deploy-services.sh units` every half hour and its packet carried
# exactly one field: `result=ok`. Car e09e30bd landed an ops-runner
# install block at 15:40 UTC; the 15:52 and 16:22 converges both closed
# `result=ok`, and answering "did it install?" took filing an
# ops-request, pulling 200 journal lines off the host and reading them by
# hand — while the estate unit observer, whose roster is the TIMERS array
# that this unit deliberately is not in, was read as saying the unit was
# absent. It was not absent. Both converges had installed it.
#
# Nothing there was broken except the RECORD. A reader off the host could
# not tell "installed 14 pairs, skipped none" from "installed 12 and
# skipped 2", so the wrong conclusion was available and got drawn, and
# the evidence lived only in a journal (CLAUDE.md §Diagnosis: a record
# that says THAT it ran and not WHAT it did is the defect class that
# costs a day, and the cost is paid by whoever is next in front of it).
#
# THE SHAPE. systemd hands the verdict to a DIFFERENT process from the
# one that did the work: `ExecStart=` runs the converge, `ExecStopPost=`
# runs infra/boss-step.sh, which closes the packet. The only thing that
# can carry facts between them is a file. So:
#
#   1. the unit declares ONE path, `Environment=BOSS_RUN_SUMMARY_FILE=…`,
#      which both Exec lines inherit — the path lives once per unit, not
#      once per process (CLAUDE.md §9a);
#   2. every script that KNOWS a fact records it here itself, as soon as
#      it knows it — no caller parses another script's prose, and a run
#      that dies keeps whatever it had already recorded;
#   3. boss-step.sh merges the file onto the step and DELETES it.
#
# (3) plus `run_summary_reset` at the top of a run is what stops a stale
# summary being read as this run's: the remove happens before the run can
# refuse anything, so a file present at ExecStopPost time was written by
# the run that just ended. A previous run's success stamped on this run's
# packet would be the same defect one layer in — a wrong answer instead
# of an error.
#
# UNSET IS A NO-OP, deliberately. A human running `deploy-services.sh
# units` by hand is not inside a packet and writes nothing; so is every
# lint that drives these installers without asking for a summary.
#
# VISIBILITY IS NEVER A PRECONDITION. Every function here returns 0
# whatever happens — a missing jq or an unwritable /run may not abort the
# converge that keeps a host's units current (the 2026-09-05 shape: an
# arm that needs the patient is not an arm). A failure warns on stderr,
# which lands in the journal beside the work.

# One key, one value, merged in immediately. Strings only: these land in
# step metadata, where every field is authored as a string.
run_summary_field() { # <key> <value>
    [ -n "${BOSS_RUN_SUMMARY_FILE:-}" ] || return 0
    _run_summary_apply --arg k "$1" --arg v "${2-}" '. + {($k): $v}'
}

# An anomaly, VERBATIM, appended to `anomalies` — a SKIP, a warning, a
# sub-installer's refusal. Capped, and the cap is STATED on the record
# (`anomalies_dropped`) rather than trimming quietly: a packet is not a
# log store, but a digest that drops the one line naming the problem is
# the reduction this whole file argues against. The full text is always
# in the host's journal.
run_summary_note() { # <line>
    [ -n "${BOSS_RUN_SUMMARY_FILE:-}" ] || return 0
    _run_summary_apply --arg line "${1-}" --argjson cap "${BOSS_RUN_SUMMARY_CAP:-4000}" '
        (.anomalies // "") as $have
        | if ($have | length) >= $cap
          then . + {anomalies_dropped: ((((.anomalies_dropped // "0") | tonumber) + 1) | tostring)}
          else . + {anomalies: ($have + (if $have == "" then "" else "\n" end) + $line)}
          end'
}

# Forget anything an earlier run left. Called once, at the top of a run,
# BEFORE it can refuse or die — see (3) above.
run_summary_reset() {
    [ -n "${BOSS_RUN_SUMMARY_FILE:-}" ] || return 0
    rm -f "$BOSS_RUN_SUMMARY_FILE" 2>/dev/null || true
    return 0
}

# <jq args...> <filter> applied to the summary object (`{}` when there is
# no file yet), written back atomically.
_run_summary_apply() {
    if ! command -v jq >/dev/null 2>&1; then
        echo "run-summary: jq is not installed — this run's packet will say nothing about" >&2
        echo "    what it did. Install jq; the work itself is unaffected." >&2
        return 0
    fi
    _rs_cur='{}'
    if [ -s "$BOSS_RUN_SUMMARY_FILE" ]; then
        _rs_cur="$(cat "$BOSS_RUN_SUMMARY_FILE")"
    fi
    if ! _rs_tmp="$(mktemp "${BOSS_RUN_SUMMARY_FILE}.XXXXXX" 2>/dev/null)"; then
        echo "run-summary: cannot write beside $BOSS_RUN_SUMMARY_FILE — the packet will not" >&2
        echo "    carry this run's summary." >&2
        unset _rs_cur
        return 0
    fi
    if printf '%s' "$_rs_cur" | jq "$@" >"$_rs_tmp" 2>/dev/null; then
        mv -f "$_rs_tmp" "$BOSS_RUN_SUMMARY_FILE" 2>/dev/null || rm -f "$_rs_tmp" 2>/dev/null
    else
        rm -f "$_rs_tmp" 2>/dev/null
        echo "run-summary: could not record a field in $BOSS_RUN_SUMMARY_FILE" >&2
    fi
    unset _rs_cur _rs_tmp
    return 0
}
