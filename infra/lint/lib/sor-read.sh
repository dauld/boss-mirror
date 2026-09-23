# sor-read.sh — how a lint reads a registry off the system of record:
# one GET, the body to a file, the HTTP code on stdout, and a rollout
# waited out rather than refused.
#
# Source it, then read:
#
#   . infra/lint/lib/sor-read.sh || exit 3
#   code=$(lint_sor_read "$NAME" "the jobs API" "$URL" "$body")
#   [ "$code" = "200" ] || skip "$URL answered HTTP $code"
#
# The code is curl's `%{http_code}`, and `000` when nothing answered —
# the word each lint's skip already read as "never got an answer", so a
# refusal reads exactly as it did before this helper existed.
#
# WHY (backlog 834ddb7c, measured 2026-09-23 ~09:05Z). Gate-run 44da8e5a
# was refused before any check ran: the-live-protocols-are-the-authored-
# protocols curled the jobs API during a stack rollout (train #576
# converging) and got HTTP 000, and the relaunch a minute later went
# green on the same diff. The door had learned to wait out that dark
# minute that morning (034002b3); the three lints that curl a registry
# themselves each carried their own bare `curl -sS -m 10` line and had
# not. The refusal was the right exit — 3, an infrastructure refusal,
# never a red (a26f92c4) — but it cost a relaunch the machine could have
# spent waiting. The wait is NOT defined here: it is the door's own
# `curl_through_a_roll`, sourced from infra/lib, so the lints and the
# door re-send exactly the same exits (CLAUDE.md §9a).
#
# THE WINDOW: 60 seconds, against the door's 120. A roll takes the API
# dark for about a minute (034002b3), and a lint that meets it has
# already lost however much of that minute passed before it launched,
# so 60s from the first refused connect covers a roll met anywhere in
# it. The door waits longer because an operator is waiting on a write;
# a lint's wait is a whole gate sitting on a dark registry, and the
# pre-flight runs three such lints in sequence, so a registry that is
# genuinely down — not rolling — costs at most three minutes before the
# gate refuses, the answer it would have given anyway. BOSS_SOR_WAIT_SECONDS
# overrides it (0 = not at all), the door's own knob: a box that cannot
# reach the cluster at all, a workstation off the LAN, sets it to 0.
#
# curl's own stderr is kept: the reason a connect failed ("No route to
# host", "Connection refused", "Could not resolve host") is the first
# thing the next reader of a refusal asks, and until this helper every
# one of these reads sent it to /dev/null.

# shellcheck source=infra/lib/curl-through-a-roll.sh
. "$(dirname "${BASH_SOURCE[0]}")/../../lib/curl-through-a-roll.sh" || exit 3

LINT_SOR_WAIT_SECONDS=60

# lint_sor_read WHO SERVICE URL BODY_FILE — prints the HTTP code.
lint_sor_read() {
    local who=$1 service=$2 url=$3 body=$4 code
    local window=${BOSS_SOR_WAIT_SECONDS:-$LINT_SOR_WAIT_SECONDS}
    case "$window" in
        *[!0-9]*)
            echo "$who: BOSS_SOR_WAIT_SECONDS='$window' is not a whole number of seconds — refusing to read $url rather than reading it as no wait or as forever" >&2
            printf '000'
            return 0
            ;;
    esac
    code=$(curl_through_a_roll "$window" "$who" "$service" "GET $url" \
        -sS -m 10 -o "$body" -w '%{http_code}' "$url") || true
    printf '%s' "${code:-000}"
}
