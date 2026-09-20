#!/usr/bin/env bash
#
# read-publish-checks — read the mirror pull request's check results
# BACK onto the publish packet, so the merge follows a judged reading
# instead of a red badge.
#
# THE GAP (backlog 321f1409, David 2026-09-19). The public mirror runs
# CodeQL on every publish PR, and its result never returned to the
# system of record: the check runs AFTER the publish packet's sign-off,
# on a surface only David sees, at merge time. PR #238 (publish/
# 2026-09-11) was red — "64 new alerts including 18 critical" — and was
# merged over the red by hand on 2026-09-12; PR #239 (publish/2026-09-19,
# 1319 files) stalled on "109 new alerts including 10 critical". Read
# from the pod over the public API, of the 100 annotations GitHub
# exposes for #239: 81 rust/cleartext-logging where the "sensitive"
# value is account_id / employee_id / current_uid (a name heuristic), 9
# hard-coded-cryptographic-value on #[cfg(test)] constants, 4 cleartext-
# transmission on in-cluster http URLs, 2 uncontrolled-path, 2
# uncontrolled-allocation, 1 SSRF, 1 sanitization — zero real findings,
# and the check's own footnote says a snapshot this large reads as
# all-new code. A check whose result never returns to the record is a
# check nobody reads (CLAUDE.md §Diagnosis).
#
# WHAT THIS DOES. publish-to-github v7's `read-checks` step is a MACHINE
# step that becomes ready when `open-pr` completes; the dispatcher rule
# read-publish-checks-on-read-checks-ready files an ops-request for the
# forge, and the root ops-runner runs THIS script with no arguments. It:
#
#   1. finds the one open publish-to-github packet whose `read-checks`
#      step is ready, and takes the PR's head sha and url off its
#      completed `open-pr` step — the sha publish-github-pr RECORDED
#      when it pushed (`snapshot_commit`), never re-derived;
#   2. reads GET /repos/<mirror>/commits/<head>/check-runs — the
#      mirror is public, so no credential — and WAITS until every
#      check-run on the head is completed (Analyze (rust) took 13 min
#      on both #238 and #239), polling every $POLL seconds up to
#      $DEADLINE, which is below the verb's allowlist timeout so the
#      script's own FAILED line is what the packet sees, never the
#      runner's kill;
#   3. reads the code-scanning check's annotations (every page the API
#      exposes; GitHub caps a check-run's exposed annotations, and the
#      reading records the declared count beside the read count);
#   4. PATCHes the reading onto the packet's metadata as
#      `code_scanning` — one entry per check with its conclusion, and
#      for the scanning check the counts by rule, by file and by level
#      — and completes `read-checks` with `conclusion`, `alerts` and
#      `rules`. The protocol's `judge-checks` step then asks the agent
#      for a disposition per rule; the merge on GitHub stays David's.
#
# THE WAIT BLOCKS THE FORGE'S OPS-RUNNER, and that is a stated cost, not
# an accident: the runner is a one-minute oneshot that answers requests
# serially, so for the length of this wait no other forge request is
# answered (a converge request is delayed, not lost — its own timers
# fire every ten minutes). The alternatives were measured and declined
# for now: a cadence rule that re-files the request every few minutes
# leaves a closed no-op packet per firing behind while a publish waits
# at approval (days, on 2026-09-17), and a detached unit that reports
# later has no rule that can alert on its failure. A publish happens
# at most once a day and its checks take ~15 minutes; revisit if the
# runner's queue shows the delay.
#
# Idempotent: re-running for the same packet re-reads and re-PATCHes
# the same keys; a step already completed is nothing to do.
#
# --check: tools and addresses, no network, exit 0/1 — what the gate
# lint runs, and the one argument the allowlist admits.
#
# Runs as root under the ops-runner with NO HOME: nothing reads $HOME.
set -euo pipefail

# THE MIRROR — owner/repo, from the one place it is spelled:
# infra/estate/estate.toml, rendered onto this host as /etc/boss/sor.env
# (infra/lib/sor.sh). An explicit BOSS_MIRROR_SLUG still wins and is
# taken FIRST, because sourcing sor.sh with BOSS_SOR_ENV named REPLACES
# what the environment carried — that is how the tests point this verb
# at a fixture. Until 2026-09-20 the default here was the slug spelled
# out, one of four copies of it owned by nothing (backlog f8af6040).
MIRROR_SLUG="${BOSS_MIRROR_SLUG:-}"
if [ -z "$MIRROR_SLUG" ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
    sor_require BOSS_MIRROR_SLUG
    MIRROR_SLUG="$BOSS_MIRROR_SLUG"
fi
GITHUB_API="${BOSS_GITHUB_API:-https://api.github.com}"
# The check whose annotations are the alerts. GitHub's code-scanning
# roll-up posts as one check-run named for the tool.
SCAN_CHECK="${BOSS_CODE_SCANNING_CHECK:-CodeQL}"
POLL="${BOSS_CHECKS_POLL_SECONDS:-60}"
DEADLINE="${BOSS_CHECKS_DEADLINE_SECONDS:-1500}"
# GitHub pages a check-run's annotations 100 at a time; ten pages is
# well past what it exposes for one run.
MAX_PAGES=10
ACTOR="${BOSS_OPS_ACTOR:-automation:read-publish-checks}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

me="read-publish-checks"
say() { echo "$me: $*"; }
refuse() { echo "$me: REFUSED — $*" >&2; exit 2; }
fail() { echo "$me: FAILED — $*" >&2; exit 1; }

workdir=$(mktemp -d) || { echo "$me: FAILED — no working directory under ${TMPDIR:-/tmp}" >&2; exit 1; }
trap 'rm -rf "$workdir"' EXIT

check_inputs() {
    local rc=0
    for tool in curl jq; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "$me: missing tool on PATH: $tool" >&2; rc=1
        fi
    done
    case "$MIRROR_SLUG" in
        */*) ;;
        *) echo "$me: BOSS_MIRROR_SLUG '$MIRROR_SLUG' is not owner/repo" >&2; rc=1 ;;
    esac
    case "$POLL$DEADLINE" in
        *[!0-9]*) echo "$me: BOSS_CHECKS_POLL_SECONDS ($POLL) and BOSS_CHECKS_DEADLINE_SECONDS ($DEADLINE) must be whole seconds" >&2; rc=1 ;;
    esac
    return $rc
}

if [ "${1:-}" = "--check" ]; then
    echo "$me --check"
    echo "  mirror     : $MIRROR_SLUG via $GITHUB_API (public, unauthenticated)"
    echo "  scan check : $SCAN_CHECK"
    echo "  wait       : every ${POLL}s up to ${DEADLINE}s for the head's check-runs to complete"
    echo "  jobs api   : ${BOSS_JOBS_URL:-<unset — the ops-runner pins it on its Exec line>}"
    if check_inputs; then
        echo "$me: --check ok"
        exit 0
    fi
    echo "$me: --check FAILED — see above" >&2
    exit 1
fi
[ $# -eq 0 ] || refuse "the only argument admitted is --check (got: $*)"

# ---------------------------------------------------------------------
# A run.
# ---------------------------------------------------------------------
[ -n "${BOSS_JOBS_URL:-}" ] || refuse "BOSS_JOBS_URL is not set; the ops-runner pins it on its Exec line and a hand run must name the system of record"
BASE="${BOSS_JOBS_URL%/}"
check_inputs || refuse "inputs incomplete (see above); nothing was read"

# 1. The packet: the open publish whose read-checks is ready or active
#    (a rule fired on readiness), and what its open-pr recorded.
if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=publish-to-github&status=open&limit=20" > "$workdir/jobs" 2> "$workdir/err"; then
    fail "jobs API unreachable at $BASE — $(cat "$workdir/err")"
fi
target=$(jq -c '
    # Slug first, title as fallback — the boss-step.sh idiom.
    def step($slug): (((.steps // []) | map(select(.spec_slug == $slug)) | .[0])
                      // ((.steps // []) | map(select(.title == $slug)) | .[0]));
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open"))
    | map({id, title, read: step("read-checks"), open: step("open-pr")})
    | map(select(.read != null and (.read.status == "ready" or .read.status == "active")))
    | .[0] // empty' "$workdir/jobs" 2>/dev/null || true)
if [ -z "$target" ]; then
    # The jq above is written once; a shape it cannot read is a
    # failure to say so, not "nothing to do".
    if ! jq -e '(if type == "object" and has("data") then .data else . end) | type == "array"' "$workdir/jobs" >/dev/null 2>&1; then
        fail "the jobs API answered something that is not a packet list: $(head -c 200 "$workdir/jobs" | tr '\n' ' ')"
    fi
    say "no open publish-to-github packet has its read-checks step ready — nothing to do"
    exit 0
fi
job_id=$(printf '%s' "$target" | jq -r '.id')
step_id=$(printf '%s' "$target" | jq -r '.read.id')
open_status=$(printf '%s' "$target" | jq -r '.open.status // "absent"')
head=$(printf '%s' "$target" | jq -r '.open.metadata.snapshot_commit // ""')
pr_url=$(printf '%s' "$target" | jq -r '.open.metadata.pr_url // ""')
[ "$open_status" = "completed" ] \
    || refuse "packet ${job_id:0:8}: read-checks is ready but its open-pr step is '$open_status' — the PR is not on the record yet"
case "$head" in
    *[!0-9a-f]* | "")
        refuse "packet ${job_id:0:8}: the open-pr step recorded no head sha (metadata.snapshot_commit is '${head:-empty}') — publish-github-pr writes it when it pushes; nothing was read" ;;
esac
[ -n "$pr_url" ] || refuse "packet ${job_id:0:8}: the open-pr step recorded no pr_url; nothing was read"
say "packet ${job_id:0:8} — reading the checks on $pr_url (head ${head:0:12})"

# 2. The check-runs, waited for. `SECONDS` is bash's own elapsed clock.
gh_get() {
    # One GET against the public API; the body to stdout, curl's words
    # to $workdir/err. No token: the mirror is public, and this verb
    # holds none.
    curl -fsS -H "accept: application/vnd.github+json" "$GITHUB_API/repos/$MIRROR_SLUG/$1" 2>"$workdir/err"
}
checks="$workdir/checks.json"
seen=0
note=""
SECONDS=0
while :; do
    if gh_get "commits/$head/check-runs?per_page=100" > "$checks.new"; then
        mv "$checks.new" "$checks"
        seen=1
        total=$(jq -r '.total_count // 0' "$checks")
        running=$(jq -r '[.check_runs[]? | select(.status != "completed") | .name] | join(", ")' "$checks")
        case "${total:-0}" in
            0) note="no check-runs registered on ${head:0:12} yet" ;;
            *) if [ -z "$running" ]; then break; fi
               note="$total check-runs on ${head:0:12}, still running: $running" ;;
        esac
    else
        note="GET commits/${head:0:12}/check-runs — $(head -c 200 "$workdir/err" | tr '\n' ' ')"
    fi
    if [ "$SECONDS" -ge "$DEADLINE" ]; then
        # Past the deadline: what was seen goes on the record as a
        # PARTIAL reading, and the step stays open — a check still
        # running is not a conclusion.
        if [ "$seen" -eq 1 ]; then
            jq -c --arg head "$head" --arg pr "$pr_url" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg note "$note" '
                {code_scanning: {
                    read_at: $at, head: $head, pr_url: $pr, complete: false, note: $note,
                    checks: [.check_runs[]? | {name, status, conclusion, title: .output.title,
                                              annotations: .output.annotations_count, url: .html_url,
                                              app: .app.slug}]}}' "$checks" > "$workdir/partial"
            curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
                ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
                --data-binary @"$workdir/partial" "$BASE/api/jobs/$job_id/metadata" > /dev/null 2>"$workdir/err" \
                || say "the partial reading could not be written onto ${job_id:0:8} — $(head -c 200 "$workdir/err" | tr '\n' ' ')"
        fi
        fail "$note after ${SECONDS}s — the reading is partial (code_scanning.complete=false on ${job_id:0:8}); re-file the request once the checks finish"
    fi
    say "not yet — $note; polling again in ${POLL}s"
    sleep "$POLL"
done
say "$total check-runs on ${head:0:12}, all completed"

# 3. The scanning check's annotations, every page.
scan_id=$(jq -r --arg n "$SCAN_CHECK" '[.check_runs[] | select(.name == $n)] | .[0].id // empty' "$checks")
ann="$workdir/annotations.json"
echo '[]' > "$ann"
if [ -n "$scan_id" ]; then
    page=1
    while [ "$page" -le "$MAX_PAGES" ]; do
        gh_get "check-runs/$scan_id/annotations?per_page=100&page=$page" > "$workdir/page" \
            || fail "GET check-runs/$scan_id/annotations page $page — $(head -c 200 "$workdir/err" | tr '\n' ' ')"
        n=$(jq 'length' "$workdir/page")
        jq -s '.[0] + .[1]' "$ann" "$workdir/page" > "$ann.new" && mv "$ann.new" "$ann"
        [ "$n" -eq 100 ] || break
        page=$((page + 1))
    done
    say "$SCAN_CHECK check-run $scan_id: $(jq 'length' "$ann") annotations read"
else
    say "no check-run named $SCAN_CHECK on ${head:0:12} — the reading says so"
fi

# 4. The reading, and the writes: the packet's metadata first, then the
#    step, so a step that reads done always has its reading beside it.
jq -n -c --slurpfile checks "$checks" --slurpfile ann "$ann" \
      --arg head "$head" --arg pr "$pr_url" --arg scan "$SCAN_CHECK" \
      --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
    ($checks[0].check_runs) as $runs
    | ($ann[0]) as $a
    | ([$runs[] | select(.name == $scan)] | .[0]) as $s
    | {code_scanning: {
        read_at: $at, head: $head, pr_url: $pr, complete: true,
        checks: [$runs[] | {name, status, conclusion, title: .output.title,
                            annotations: .output.annotations_count, url: .html_url, app: .app.slug}],
        alerts: (if $s == null then {check: $scan, conclusion: "absent", read: 0, by_rule: [], by_file: [], by_level: {}}
                 else {
                    check: $scan, conclusion: ($s.conclusion // "none"), title: $s.output.title,
                    url: $s.html_url,
                    # The check-run summary footnote: alerts not introduced by the PR may be
                    # reported because the change was too large — true of every week-scale snapshot.
                    caveat: (($s.output.summary // "") | test("too large")),
                    declared: ($s.output.annotations_count // 0),
                    read: ($a | length),
                    by_level: ($a | group_by(.annotation_level) | map({(.[0].annotation_level): length}) | add // {}),
                    by_rule: ($a | group_by(.title) | map({rule: .[0].title, count: length,
                                                          files: ([.[].path] | unique | length)})
                              | sort_by(-.count, .rule)),
                    by_file: ($a | group_by(.path) | map({path: .[0].path, count: length})
                              | sort_by(-.count, .path))
                 } end)}}' > "$workdir/reading"
conclusion=$(jq -r '.code_scanning.alerts.conclusion' "$workdir/reading")
alerts=$(jq -r '.code_scanning.alerts.read' "$workdir/reading")
rules=$(jq -r '.code_scanning.alerts.by_rule | length' "$workdir/reading")
files=$(jq -r '.code_scanning.alerts.by_file | length' "$workdir/reading")

curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
    ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
    --data-binary @"$workdir/reading" "$BASE/api/jobs/$job_id/metadata" > /dev/null 2>"$workdir/err" \
    || fail "writing the reading onto ${job_id:0:8} (PATCH /api/jobs/$job_id/metadata) — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
say "reading written onto ${job_id:0:8} as code_scanning"

# Merge, never replace: PUT swaps the step's metadata wholesale.
printf '%s' "$target" | jq -c --arg c "$conclusion" --arg a "$alerts" --arg r "$rules" --arg h "$head" '
    {status: "completed",
     metadata: ((.read.metadata // {})
                + {conclusion: $c, alerts: $a, rules: $r, head: $h, read_by: "read-publish-checks"})}' \
    > "$workdir/payload"
curl -fsS -X PUT -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
    ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
    --data-binary @"$workdir/payload" "$BASE/api/jobs/$job_id/steps/$step_id" > /dev/null 2>"$workdir/err" \
    || fail "the reading is on ${job_id:0:8} but completing read-checks failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); complete the step by hand with conclusion=$conclusion alerts=$alerts rules=$rules"

# The answer line, LAST: what the dispatcher rule
# complete-publish-read-checks-on-read-publish-checks-answered reads.
say "read $conclusion — $alerts alerts in $rules rules over $files files on ${head:0:12} ($pr_url)"
