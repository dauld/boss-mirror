#!/usr/bin/env bash
# surface-usage.sh — which surfaces each operator opened, rolled up
# daily and filed as a packet.
#
# Backlog 628f182b. David, 2026-09-16, running the company as one human
# plus agents: "I am mostly concerned about my personal HCI … the UI
# and good transparency into underlying state has been really important
# for driving reliability and trust." Then: "let's measure which
# surfaces I open for a week." Measured that day: nothing recorded a
# route open per actor. The SPA now posts every client-side navigation
# to `POST /api/surface-opens` (apps/web/src/shell/surface-opens.ts) —
# the route PATTERN and the time, the actor signed by the gateway from
# the session — and the jobs API holds the rows for thirty days
# (boss_jobs::surface_opens). This script is the daily reading of them.
#
# THE SHAPE IS infra/codebase-metrics.sh's, deliberately: same host,
# same unit/timer/role registration, same PATCH of the row onto the
# packet the unit's ExecStartPre opened, same body-as-a-file transport,
# same journal headline. It re-derives nothing: the count per actor per
# route is ONE GROUP BY on the jobs API (`GET /api/surface-opens/
# rollup`), the same read the Codebase page's Surfaces section makes,
# so the packet and the page cannot disagree by summing differently
# (CLAUDE.md §9a). What this adds is the comparison the server cannot
# make — which catalogued surfaces NOBODY opened — because the roster
# of surfaces is the nav catalog in the tree, and this is the one host
# with a tree and a route to the system of record.
#
# THE CHORE CONTRACT (design 14c135f5): the row carries `measured` (the
# numbers) and `coverage` (what population was examined and whether
# that was all of it). A run that cannot state its coverage — the
# roll-up unreadable, the catalog unreadable — FAILS with exit 3 and
# files nothing: a row from no read would say "nobody opened anything",
# which is the confident wrong answer, not a quiet day.
#
# WHAT THE ROW CARRIES
# --------------------
#   measured   at; opens (the total); actors (how many); distinct_routes;
#              per_actor — for each actor: opens, distinct routes, and
#              every route with its count, most-opened first;
#              top_routes — the ten most-opened across all actors;
#              never_opened — the catalog paths no actor opened in the
#              window (the deletion candidates) and their count;
#              retention — what the sweep deleted, or why it could not;
#              method.
#   coverage   the window (since, until, hours); the roll-up read (the
#              URL, how many rows); the catalog read (the file, its
#              head, how many paths); complete: true — the roll-up is
#              the whole table over the window, not a page of it.
#
# THE ROUTE IS A PATTERN. `/ux/jobs/:jobId`, never a uuid; so
# `never_opened` compares catalog PATHS against opened PATTERNS, and a
# catalog path with a detail page behind it (`/ux/jobs`) is opened when
# its list is opened, not when a detail is. The nav catalog's `path`
# values are the roster; tab strips (ItTabs.svelte) and detail pages
# are surfaces too but are not catalogued, and are not in the
# never-opened list — they show up in `per_actor` when opened.
#
# RETENTION. Rows older than thirty days are swept on the same run
# (`POST /api/surface-opens/sweep`), and the sweep's count and cutoff
# ride the row under `measured.retention`. The number is a constant in
# boss_jobs::surface_opens::RETENTION_DAYS until the retention registry
# lands (backlog 16115a17); then it is a retention row and this script
# reads what the sweep applied, which it already does. `row` does not
# sweep — an operator asking the question by hand deletes nothing.
#
# EXIT STATUS — the vocabulary `infra/lint/lib/git-answer.sh` defines:
#   0  the roll-up was read and here is (or here was filed) the reading
#   1  the roll-up was read and something else failed (filing, or the
#      sweep — the measurement is on the packet either way)
#   3  NO reading: the roll-up or the catalog could not be read. An
#      infrastructure refusal, not a day with no opens.
#
# USAGE
#   surface-usage.sh row   [--repo DIR] [--hours N]
#   surface-usage.sh file  [--repo DIR] [--hours N]
#
# `row` prints the row and touches nothing. `file` computes it, sweeps
# retention, and PATCHes it onto the open `maintenance-surface-usage`
# packet — the daily cadence's whole body of work. `--repo` is the
# checkout whose nav catalog is the roster (default: the one this
# script is in); `--hours` is the window (default 24). BOSS_JOBS_URL
# names the system of record, for the roll-up AND the filing — one URL,
# so the row is filed where it was measured.

set -uo pipefail

export TZ=UTC

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lint/lib/git-answer.sh
. "$SELF_DIR/lint/lib/git-answer.sh"

NAME="surface-usage"
KIND="maintenance-surface-usage"
CATALOG="apps/web/src/shell/nav-catalog.ts"
TOP=10

usage() { sed -n '/^# USAGE/,/^$/p' "$0" | sed 's/^# \{0,1\}//' >&2; }

CMD="${1:-}"
[ -n "$CMD" ] && shift
REPO="$SELF_DIR/.."
HOURS=24
while [ $# -gt 0 ]; do
    case "$1" in
        --repo) REPO="${2:?--repo needs a directory}"; shift 2 ;;
        --hours) HOURS="${2:?--hours needs a number}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "$NAME: unknown argument '$1'" >&2; usage; exit 2 ;;
    esac
done
case "$CMD" in
    row|file) ;;
    *) echo "$NAME: expected one of row|file" >&2; usage; exit 2 ;;
esac
case "$HOURS" in
    ''|*[!0-9]*|0) echo "$NAME: --hours must be a positive integer, got '$HOURS'" >&2; exit 2 ;;
esac

for tool in jq curl date; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "$NAME: $LINT_CANNOT_ANSWER_MARKER — no $tool on PATH, so nothing was read." >&2
        exit "$LINT_CANNOT_ANSWER"
    }
done

if ! cd "$REPO" 2>/dev/null; then
    {
        printf '%s: %s — --repo %s is not a directory this process can enter,\n' \
            "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$REPO"
        printf '  so the nav catalog was not read. An INFRASTRUCTURE refusal (exit %s), not a\n' \
            "$LINT_CANNOT_ANSWER"
        printf '  catalog with no surfaces in it.\n'
    } >&2
    exit "$LINT_CANNOT_ANSWER"
fi
REPO="$(pwd)"

if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$NAME: BOSS_JOBS_URL is not set, and there is no safe default — a reading taken from one instance and filed on another is a reading of nothing (infra/boss-step.sh carries the argument)." >&2
    exit 78   # EX_CONFIG
fi

# ---------------------------------------------------------------------
# THE TRANSPORT — the same shape codebase-metrics.sh and
# protocol-drift.sh use, so the deploy-roll retry posture and the
# body-as-a-file cap live in one place each.
# ---------------------------------------------------------------------
API_CURL="$SELF_DIR/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh
BOSS_USER='{"id":"automation:surface-usage","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

api() { # <method> <path> [body]
    local method="$1" path="$2" body="${3:-}"
    if [ -n "$body" ]; then
        local bodyfile
        bodyfile=$(mktemp -t surface-usage-body.XXXXXX) || return 1
        printf '%s' "$body" > "$bodyfile"
        "$API_CURL" -fsS -X "$method" -H "x-boss-user: $BOSS_USER" \
            -H "content-type: application/json" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            --data-binary "@$bodyfile" "$BOSS_JOBS_URL$path"
        local rc=$?
        rm -f "$bodyfile"
        return $rc
    else
        "$API_CURL" -fsS -X "$method" -H "x-boss-user: $BOSS_USER" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            "$BOSS_JOBS_URL$path"
    fi
}

# ---------------------------------------------------------------------
# THE ROSTER — every `path:` the nav catalog declares, once each. Read
# off the source with a regex rather than by importing it: this host
# has no bun, and the catalog's own test pins that each entry carries
# exactly this `path: '…'` shape. Two entries share `/it/registry/rules`,
# hence the dedup. A catalog that yields no path is unreadable, not
# empty — the file has never had fewer than twenty.
# ---------------------------------------------------------------------
catalog_paths() { # prints one path per line, catalog order, deduped
    grep -oE "path: '[^']+'" "$CATALOG" | sed -E "s/^path: '([^']+)'$/\1/" | awk '!seen[$0]++'
}

# The checkout's commit, as context for the catalog read: null with the
# reason when git cannot answer. The roll-up still happened, so this is
# never a refusal (the same stance protocol-drift.sh takes).
head_json() {
    local err sha
    err=$(mktemp) || { jq -n '{head: null, head_why: "no writable temp dir to keep git stderr"}'; return; }
    if sha=$(git rev-parse HEAD 2>"$err"); then
        jq -n --arg sha "$sha" '{head: $sha, head_why: null}'
    else
        jq -n --arg why "git could not read $REPO: $(tr '\n' ' ' <"$err")" \
            '{head: null, head_why: $why}'
    fi
    rm -f "$err"
}

method_json() {
    jq -n '{
      rollup: "GET /api/surface-opens/rollup?since&until — one GROUP BY (actor_id, route) over surface_opens on the jobs API, ordered actor then opens descending. The same read the Codebase page makes over 7 days; this row is 24 hours of it. Nothing here re-sums raw rows.",
      route: "A route is a PATTERN minted by the SPA off its parsed Route — /ux/jobs/:jobId, never the id (apps/web/src/shell/surface-opens.ts). One POST per client-side navigation, debounced against the same pattern, so a detail page opened for ten ids in a row is one open.",
      actor: "The session actor the gateway signed onto the request; the body never names one, and a machine-shaped id (an automation, an agent login) is refused at the door. Agents do not run the SPA, so nothing of theirs is counted.",
      never_opened: "The nav catalog paths (apps/web/src/shell/nav-catalog.ts, every entry once) that no actor opened in the window. Tab strips and detail pages are surfaces too but are not catalogued, so they appear in per_actor when opened and never in this list. Seven days of this list is the deletion-candidate reading David asked for; one day of it is one day.",
      retention: "POST /api/surface-opens/sweep deletes rows past RETENTION_DAYS (a constant in boss_jobs::surface_opens until backlog 16115a17 makes it a retention row); deleted and before are what the sweep reported. Only `file` sweeps."
    }'
}

# The reading: the roll-up JSON on stdin, the catalog paths as a JSON
# array, the window and the head — folded into `measured` + `coverage`.
row_json() {
    local since until rollup paths npaths head method
    until=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    since=$(date -u -d "$until - $HOURS hours" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null) || {
        echo "$NAME: $LINT_CANNOT_ANSWER_MARKER — date(1) could not compute the window start, so no window was read." >&2
        return "$LINT_CANNOT_ANSWER"
    }

    paths=$(catalog_paths | jq -R . | jq -s .) || paths='[]'
    npaths=$(printf '%s' "$paths" | jq 'length')
    if [ "${npaths:-0}" = "0" ]; then
        {
            printf '%s: %s — %s/%s yielded no nav paths, so there is no roster to\n' \
                "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$REPO" "$CATALOG"
            printf '  compare the opens against. A row with an empty never-opened list would\n'
            printf '  read as "every surface was opened" — an infrastructure refusal (exit %s).\n' \
                "$LINT_CANNOT_ANSWER"
        } >&2
        return "$LINT_CANNOT_ANSWER"
    fi

    local err
    err=$(mktemp -t surface-usage-err.XXXXXX) || return 1
    rollup=$(api GET "/api/surface-opens/rollup?since=$since&until=$until" 2>"$err")
    local rc=$?
    if [ "$rc" != "0" ] || ! printf '%s' "$rollup" | jq -e '.rows | type == "array"' >/dev/null 2>&1; then
        {
            printf '%s: %s — GET %s/api/surface-opens/rollup did not answer with rows (curl exit %s),\n' \
                "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$BOSS_JOBS_URL" "$rc"
            printf '  so nothing was read and nothing is filed. A row from no read would say\n'
            printf '  "nobody opened anything". It said:\n'
            sed 's/^/    /' "$err"
            # A here-string, not `printf | head`: under pipefail a reader
            # that exits early SIGPIPEs the writer (28af807c).
            head -c 2000 <<< "$rollup" | sed 's/^/    /'
        } >&2
        rm -f "$err"
        return "$LINT_CANNOT_ANSWER"
    fi
    rm -f "$err"

    head=$(head_json) || return 1
    method=$(method_json) || return 1
    # THE ROLL-UP GOES IN ON STDIN, NOT AS AN ARGUMENT — the cap that
    # took codebase-metrics for two days is the same cap.
    printf '%s' "$rollup" | jq \
        --argjson paths "$paths" --argjson head "$head" --argjson method "$method" \
        --arg since "$since" --arg until "$until" --argjson hours "$HOURS" \
        --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        --arg source "$BOSS_JOBS_URL/api/surface-opens/rollup" --arg catalog "$CATALOG" \
        --argjson top "$TOP" '
        .rows as $rows
        | ($rows | map(.route) | unique) as $opened
        | ($rows | group_by(.actor_id) | map({
              actor_id: .[0].actor_id,
              opens: (map(.opens) | add),
              distinct_routes: (map(.route) | unique | length),
              routes: (sort_by(-.opens, .route) | map({route, opens, last_at}))
            }) | sort_by(-.opens, .actor_id)) as $per_actor
        | ($rows | group_by(.route) | map({route: .[0].route, opens: (map(.opens) | add)})
            | sort_by(-.opens, .route) | .[:$top]) as $top_routes
        | ($paths - $opened) as $never
        | {measured: {
             at: $at,
             opens: ($rows | map(.opens) | add // 0),
             actors: ($per_actor | length),
             distinct_routes: ($opened | length),
             per_actor: $per_actor,
             top_routes: $top_routes,
             never_opened: $never,
             never_opened_count: ($never | length),
             catalog_routes: ($paths | length),
             retention: null,
             method: $method},
           coverage: {
             window: {since: $since, until: $until, hours: $hours},
             rollup: {source: $source, rows: ($rows | length)},
             catalog: {file: $catalog, head: $head.head, head_why: $head.head_why, routes: ($paths | length)},
             complete: true}}'
}

# The sweep, as a JSON object for `measured.retention`: what the door
# reported, or `null` beside the reason it could not.
sweep_json() {
    local err out rc
    err=$(mktemp -t surface-usage-sweep.XXXXXX) || { jq -n '{swept: null, error: "no writable temp dir"}'; return 1; }
    out=$(api POST "/api/surface-opens/sweep" 2>"$err"); rc=$?
    if [ "$rc" = "0" ] && printf '%s' "$out" | jq -e '.deleted | type == "number"' >/dev/null 2>&1; then
        printf '%s' "$out" | jq '{swept: {deleted, before, retention_days}, error: null}'
        rm -f "$err"
        return 0
    fi
    jq -n --arg why "POST /api/surface-opens/sweep failed (curl exit $rc): $(tr '\n' ' ' <"$err") ${out:0:500}" \
        '{swept: null, error: $why}'
    rm -f "$err"
    return 1
}

# ---------------------------------------------------------------------
# FILING — the packet is the record. The open packet was opened by this
# unit's ExecStartPre (boss-maintenance-wrap.sh); this writes the
# reading onto it with PATCH /api/jobs/{id}/metadata, which MERGES
# top-level keys server-side. Never the full job PUT.
# ---------------------------------------------------------------------
file_row() {
    local row
    row=$(row_json) || exit $?

    local retention sweep_rc=0
    retention=$(sweep_json) || sweep_rc=1
    row=$(printf '%s' "$row" | jq --argjson r "$retention" '.measured.retention = $r')

    local open open_count job_id
    open=$(api GET "/api/jobs?kind=$KIND&status=open&limit=2") || {
        echo "$NAME: could not read the open $KIND packet; the reading below was computed and NOT filed." >&2
        printf '%s\n' "$row" >&2
        exit 1
    }
    open_count=$(printf '%s' "$open" | jq '(.data // []) | length')
    if [ "$open_count" = "0" ]; then
        echo "$NAME: no open $KIND packet to file onto — ExecStartPre could not open one (the jobs API was unreachable then, most likely). The reading is below; this run recorded nothing." >&2
        printf '%s\n' "$row" >&2
        exit 1
    fi
    if [ "$open_count" != "1" ]; then
        echo "$NAME: $open_count open $KIND packets — refusing to guess which one today's reading belongs on." >&2
        exit 1
    fi
    job_id=$(printf '%s' "$open" | jq -r '.data[0].id')

    # Two top-level keys, merged server-side: `measured` is the reading,
    # `coverage` what it examined — the chore contract's two required
    # fields.
    local body
    body=$(printf '%s' "$row" | jq '{measured: .measured, coverage: .coverage}')
    api PATCH "/api/jobs/$job_id/metadata" "$body" >/dev/null || {
        echo "$NAME: filing the reading onto ${job_id:0:8} failed. It is below rather than lost." >&2
        printf '%s\n' "$row" >&2
        exit 1
    }

    # THE JOURNAL GETS THE HEADLINE, not a digest that hides the counts:
    # the full row is on the packet; the totals and each actor's line
    # are here, where `systemctl status` shows them.
    printf '%s' "$row" | jq -r --arg id "${job_id:0:8}" '
        .measured as $m | .coverage as $c
        | "surface-usage: \($m.opens) open(s) by \($m.actors) actor(s) across \($m.distinct_routes) route(s) in the last \($c.window.hours)h (\($c.window.since) to \($c.window.until)); " +
          "\($m.never_opened_count) of \($m.catalog_routes) catalogued surfaces never opened",
          ($m.per_actor[] | "  \(.actor_id): \(.opens) open(s), \(.distinct_routes) route(s) — top " + (.routes[:3] | map("\(.route) ×\(.opens)") | join(", "))),
          (if $m.retention.swept != null then "  retention: swept \($m.retention.swept.deleted) row(s) older than \($m.retention.swept.before)"
           else "  retention: NOT swept — \($m.retention.error)" end),
          "  filed on \($id)"'
    # A sweep that failed is loud AFTER the reading is on the packet:
    # the measurement is never lost to the housekeeping around it.
    exit "$sweep_rc"
}

case "$CMD" in
    row) row_json ;;
    file) file_row ;;
esac
