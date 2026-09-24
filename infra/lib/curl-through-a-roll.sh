# curl-through-a-roll.sh — curl, re-sent only while the request never
# left: the ONE definition of waiting out a rollout, for every shell
# reader of the system of record.
#
# Source it, then call:
#
#   curl_through_a_roll WINDOW WHO SERVICE WHAT <curl args...>
#
#   WINDOW   whole seconds a refused connect is waited out (0 = not at all)
#   WHO      the word every line it prints starts with (`boss-api`, a
#              lint's name)
#   SERVICE  what is dark, as a phrase (`the jobs API`, `the dispatcher`)
#   WHAT     the request, as a phrase (`GET /api/jobs`)
#
# It prints curl's stdout and returns curl's own exit code, so a caller
# sees exactly what a bare curl would have shown it — except that exit
# 6 and 7 are re-sent until WINDOW has passed, with one line on stderr
# per wait, and past the window nothing is printed on stdout and one
# line names how long it waited.
#
# WHY ONLY 6 AND 7 (backlog 034002b3, twice on 2026-09-23). The stack
# rolls with strategy Recreate, so every train that converges takes the
# jobs API dark for about a minute. curl exit 6 (could not resolve
# host) and 7 (could not connect — refused, no route, network
# unreachable) are the two that mean the request never left. Any other
# exit may be a request the server received — 28 is one code for a
# connect timeout AND a transfer that timed out after the body went, 52
# and 56 are a reply lost after sending — so it is surfaced on the
# first attempt, and an HTTP status is an answer whatever it says. A
# write re-sent on a 28 could land twice; a write re-sent on a 7 cannot
# have landed once.
#
# WHY IT LIVES HERE (backlog 834ddb7c, 2026-09-23). It was written into
# infra/dev/boss-api, the door, that morning. That afternoon a gate was
# refused before any check ran because a pre-flight lint curled the
# jobs API itself during a roll and had no such wait; the relaunch a
# minute later went green on the same diff. The door and the lints now
# source this one file — the door from beside its REAL location (it is
# reached through a symlink), the lints through infra/lint/lib/sor-read.sh
# — and a_lint_that_reads_the_api_waits_out_a_roll.rs refuses a second
# definition anywhere under infra/ (CLAUDE.md §9a). The CLI's twin is
# `train::waiting_out_a_roll` (crates/orchestrators/boss-cli), the same
# rule over reqwest's own connect classification. infra/boss-api-curl.sh,
# the cron chores' helper, sources it too since backlog c51967b5
# (2026-09-23) — it had carried an older rule that re-sent 28, 52, 55
# and 56 as well — and the estate observer, whose image carries none of
# this repo, inlines the same two exits, pinned equal to the case arm
# below by a_chore_write_is_sent_once.rs.
#
# Bash: it reads $SECONDS and uses `local`.

curl_through_a_roll() {
  local window=$1 who=$2 service=$3 what=$4
  shift 4
  case "$window" in
    ''|*[!0-9]*)
      echo "$who: $what: a roll window of '$window' is not a whole number of seconds — refusing rather than reading it as no wait or as forever" >&2
      return 2
      ;;
  esac
  local started=$SECONDS attempt=1 pause=2 rc answer elapsed
  while :; do
    rc=0; answer=$(curl "$@") || rc=$?
    case "$rc" in
      6|7) ;;
      *) printf '%s' "$answer"; return "$rc" ;;
    esac
    elapsed=$((SECONDS - started))
    if [ "$elapsed" -ge "$window" ]; then
      echo "$who: $what: $service refused every connection for ${elapsed}s ($attempt attempts, curl exit $rc; waited out for up to ${window}s in case it was a rollout) — nothing was sent, so nothing landed and relaunching is safe" >&2
      return "$rc"
    fi
    [ "$pause" -le $((window - elapsed)) ] || pause=$((window - elapsed))
    echo "$who: $service is not answering (a rollout?) — retrying $what in ${pause}s (${elapsed}s of ${window}s, curl exit $rc)" >&2
    sleep "$pause"
    attempt=$((attempt + 1))
    pause=$((pause * 2)); [ "$pause" -le 15 ] || pause=15
  done
}
