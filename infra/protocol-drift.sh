#!/usr/bin/env bash
# protocol-drift.sh — what is live that the tree does not say, and what
# does the tree say that is not live: measured daily, filed as a packet.
#
# Backlog 19dec171 (car 1 of 8f4e9cc0; David, 2026-09-11: "we might need
# some sort of doc diff view for me to approve"). The comparison this
# files ALREADY EXISTED and was thrown away on every run of it:
# `infra/lint/the-live-protocols-are-the-authored-protocols.sh` computes,
# on every gate, the field-level drift between the authored bundle
# (`infra/platform/workflows/`) and the live registry (`GET /api/workflows`)
# and REPORTS it — into a gate log nobody keeps. The server cannot compute
# it (it has no tree), the tree cannot serve it (it has no page), and the
# one host with both — boss-gcp, which converges a checkout of main every
# half hour and already runs the daily codebase-metrics measurement this
# way — filed nothing. Measured 2026-09-15 against the system of record:
# 85 admitted kinds, 47 compared, 2 descriptions adrift
# (`maintenance-sweep` v2, `ship-a-change` v31), and no record of either
# anywhere a reader could find it the next morning.
#
# THE SHAPE IS infra/codebase-metrics.sh's, deliberately: same host, same
# unit/timer/role registration, same PATCH of the row onto the packet the
# unit's ExecStartPre opened, same body-as-a-file transport, same journal
# headline. The one thing this script adds is the one thing that was
# missing: it runs the lint in `--require-live --report-json` mode and
# files what the lint found. It does NOT re-derive the comparison — the
# lint is the comparator, its self-test proves the report carries the
# same facts as its text, and a second implementation here would be a
# second copy of the same judgement (CLAUDE.md §9a).
#
# WHAT THE ROW CARRIES
# --------------------
#   measured   at, the checkout's head + head_at (context — see below),
#              the surface read, the lint's exit code, how many kinds the
#              registry admits, how many the tree authors, how many were
#              compared, and the method.
#   drift      the findings, each named:
#                unauthored  live kinds no file authors — what is live
#                            that the tree does not say. The lint FAILS
#                            on these (exit 1); this row records them.
#                fields      per kind, per compared field: the live
#                            version, where the texts first differ, both
#                            lengths, and a 90-character window of each.
#                absent      fields the file makes no claim about.
#                pending     bundle kinds with no live row — what the
#                            tree says that is not live (the window
#                            between a protocol car's merge and the seed).
#                tenants     per tenant bundle, kinds not admitted here.
#
# `head` IS CONTEXT, NOT THE MEASUREMENT. The lint reads the working
# tree of the checkout it lives in; `head` says which commit that was,
# so a row from a stale checkout says how stale rather than looking
# current. If git cannot read the checkout, `head` is null and
# `head_why` says why — the comparison still happened, and refusing to
# file it over a missing sha would lose the only copy of a measurement
# that was taken (§Diagnosis). codebase-metrics.sh refuses in the same
# situation because there git IS its measurement; here it is not.
#
# EXIT STATUS — the vocabulary `infra/lint/lib/git-answer.sh` defines:
#   0  the lint compared, and here is (or here was filed) what it found
#   1  the lint compared and something else failed (filing, usually)
#   3  the lint produced NO report — the registry could not be read, or
#      the lint refused before comparing. An infrastructure refusal, not
#      a day with no drift: a row filed from no comparison would read
#      as "0 adrift", which is the confident wrong answer (§Doors).
#
# USAGE
#   protocol-drift.sh row   [--repo DIR]
#   protocol-drift.sh file  [--repo DIR]
#
# `row` prints the row. `file` computes it and PATCHes it onto the open
# `maintenance-protocol-drift` packet — the daily cadence's whole body
# of work. `--repo` is the checkout whose lint and bundle are read
# (default: the one this script is in). BOSS_JOBS_URL names the system
# of record, for the registry read AND the filing — one URL, so the row
# is filed where it was measured.

set -uo pipefail

export TZ=UTC

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lint/lib/git-answer.sh
. "$SELF_DIR/lint/lib/git-answer.sh"
# shellcheck source=infra/lib/jq.sh
. "$SELF_DIR/lib/jq.sh"

NAME="protocol-drift"
KIND="maintenance-protocol-drift"
LINT="infra/lint/the-live-protocols-are-the-authored-protocols.sh"

usage() { sed -n '/^# USAGE/,/^$/p' "$0" | sed 's/^# \{0,1\}//' >&2; }

CMD="${1:-}"
[ -n "$CMD" ] && shift
REPO="$SELF_DIR/.."
while [ $# -gt 0 ]; do
    case "$1" in
        --repo) REPO="${2:?--repo needs a directory}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "$NAME: unknown argument '$1'" >&2; usage; exit 2 ;;
    esac
done
case "$CMD" in
    row|file) ;;
    *) echo "$NAME: expected one of row|file" >&2; usage; exit 2 ;;
esac

for tool in jq python3 curl; do
    # The lint SKIPS (exit 0, bare) when curl or python3 is missing; under
    # --require-live it exits 75 and this script refuses below. Naming the
    # tool here is the shorter diagnosis.
    command -v "$tool" >/dev/null 2>&1 || {
        echo "$NAME: $LINT_CANNOT_ANSWER_MARKER — no $tool on PATH, so nothing was compared." >&2
        exit "$LINT_CANNOT_ANSWER"
    }
done

if ! cd "$REPO" 2>/dev/null; then
    {
        printf '%s: %s — --repo %s is not a directory this process can enter,\n' \
            "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$REPO"
        printf '  so no bundle was read. An INFRASTRUCTURE refusal (exit %s), not a\n' \
            "$LINT_CANNOT_ANSWER"
        printf '  registry that agrees with an empty tree.\n'
    } >&2
    exit "$LINT_CANNOT_ANSWER"
fi
REPO="$(pwd)"
[ -f "$LINT" ] || {
    echo "$NAME: $LINT_CANNOT_ANSWER_MARKER — $REPO/$LINT does not exist, so there is no comparator to run." >&2
    exit "$LINT_CANNOT_ANSWER"
}

if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$NAME: BOSS_JOBS_URL is not set, and there is no safe default — a drift measured against one instance and filed on another is a measurement of nothing (infra/boss-step.sh carries the argument)." >&2
    exit 78   # EX_CONFIG
fi

# ---------------------------------------------------------------------
# THE COMPARATOR IS THE LINT. Run in the mode for a caller with somewhere
# to put the answer: --require-live so an unreachable registry is 75 and
# never a silent 0, --report-json so the facts arrive as data. Its stderr
# is captured whole and printed ONLY on refusal — the quiet buys nothing
# on success and, on failure, the tail-of-a-log reduction is the defect
# class §Diagnosis records three times in one day.
# ---------------------------------------------------------------------
report_json() { # prints the report's path; the caller removes its dir
    local t rc
    t=$(mktemp -d "${TMPDIR:-/tmp}/protocol-drift.XXXXXX") || {
        echo "$NAME: $LINT_CANNOT_ANSWER_MARKER — no writable temp dir, so the lint's report had nowhere to land." >&2
        return "$LINT_CANNOT_ANSWER"
    }
    bash "$LINT" --require-live --report-json "$t/report.json" >"$t/out" 2>"$t/err"
    rc=$?
    case "$rc" in
        # 0 clean, 1 a failure of the tree (an unauthored live kind is one —
        # and is exactly a drift this row must carry), 2 field drift. In all
        # three the comparison RAN, and the report is the record of it.
        0|1|2) ;;
        75)
            {
                printf '%s: %s — %s produced no report (exit 75): the live registry could not be read,\n' \
                    "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$(basename "$LINT")"
                printf '  so nothing was compared and nothing is filed. A row from no comparison\n'
                printf '  would read as "0 adrift". The lint said:\n'
                sed 's/^/    /' "$t/err"
            } >&2
            rm -rf "$t"; return "$LINT_CANNOT_ANSWER" ;;
        *)
            {
                printf '%s: %s — %s exited %s before comparing, so there is no report to file. It said:\n' \
                    "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$(basename "$LINT")" "$rc"
                sed 's/^/    /' "$t/err"
                sed 's/^/    /' "$t/out"
            } >&2
            rm -rf "$t"; return "$LINT_CANNOT_ANSWER" ;;
    esac
    # An ABSENT report is exactly "wrote no readable report", and it is
    # the case `jq -e` alone calls a pass (d96e38ab).
    if ! jq_doc_file "$t/report.json" \
        || ! jq -e 'type == "object"' "$t/report.json" >/dev/null 2>&1; then
        {
            printf '%s: %s — %s exited %s but wrote no readable report to %s.\n' \
                "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$(basename "$LINT")" "$rc" "$t/report.json"
            printf '  That is a defect in the lint, not a fact about the registry. It said:\n'
            sed 's/^/    /' "$t/err"
        } >&2
        rm -rf "$t"; return "$LINT_CANNOT_ANSWER"
    fi
    printf '%s' "$t"
}

# The checkout's commit, as context. Null with the reason when git
# cannot answer — see the header for why that is not a refusal here.
head_json() {
    local err sha at
    err=$(mktemp) || { jq -n '{head: null, head_at: null, head_why: "no writable temp dir to keep git stderr"}'; return; }
    if sha=$(git rev-parse HEAD 2>"$err") && at=$(git log -1 --date=format-local:%Y-%m-%dT%H:%M:%SZ --format=%ad "$sha" 2>>"$err"); then
        jq -n --arg sha "$sha" --arg at "$at" '{head: $sha, head_at: $at, head_why: null}'
    else
        jq -n --arg why "git could not read $REPO: $(tr '\n' ' ' <"$err")" \
            '{head: null, head_at: null, head_why: $why}'
    fi
    rm -f "$err"
}

method_json() {
    jq -n '{
      comparator: "infra/lint/the-live-protocols-are-the-authored-protocols.sh --require-live --report-json — the same comparison every gate runs, in the mode for a caller with somewhere to put the answer. This script re-derives nothing; the self-test of the lint proves its JSON report carries the same kind, field, live version, excerpt, counts and verdict as its text.",
      fields: "label, description, category — the scalar strings an operator reads — and, since 2026-09-15, four step facets: steps.count, steps.titles (ordered), steps.<title>.required (the sorted required-field names) and steps.<title>.title_template; each compared between infra/platform/workflows/<kind>.toml and the ACTIVE live row of that kind. Steps were left out until the live ship-a-change v31 carried a settled step and a required proof field its file lacked while the measurement read one description adrift (0ccf23ec). Predicates, kinds, field types, subject_kinds, metadata_schema and entitlements are still not compared: they need the normalisation the publish path applies first.",
      direction: "`unauthored` is what is live that the tree does not say — a kind the registry admits and no file, tenant seed, Rust literal or migration authors; the lint FAILS on it (exit 1) and this row records it. `pending` is what the tree says that is not live — a bundle kind with no live row, the expected window between the merge of a protocol car and the seed behind it; reported, never failed on. `fields` is the third state: both exist and disagree.",
      windows: "tree_window and live_window are 90-character excerpts around the first differing character. Both full copies stay readable at their homes — the file in the tree at `head`, the row at GET /api/workflows — so the excerpt discards no only-copy.",
      head: "The checkout the bundle was read from. The lint reads its own working tree, which on boss-gcp is /opt/boss, fast-forwarded to forge main by boss-gcp-converge every half hour; a stale checkout therefore shows as a stale head, not as drift. Null when git could not read the checkout, with head_why saying so — the comparison still happened.",
      lint_exit: "The verdict of the lint itself on this run: 0 agreement, 1 a failure of the tree (an unauthored live kind, most often), 2 field drift under --require-live. Recorded so the row carries the judgement the gate would have made, not only the facts behind it."
    }'
}

row_json() {
    local t report head method
    t=$(report_json) || return $?
    report=$(cat "$t/report.json")
    rm -rf "$t"
    head=$(head_json) || return 1
    method=$(method_json) || return 1
    printf '%s' "$report" | jq --argjson head "$head" --argjson method "$method" \
        --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
        . as $r |
        {measured: {at: $at, head: $head.head, head_at: $head.head_at, head_why: $head.head_why,
                    target: $r.target, lint_exit: $r.verdict,
                    live_admitted: $r.live.admitted, authored: $r.authored.count,
                    fields_parsed: $r.fields.parsed, fields_compared: $r.fields.compared,
                    exempt: $r.exempt, method: $method},
         drift: {counts: {unauthored: ($r.unauthored | length), fields: ($r.fields.drift | length),
                          absent: ($r.fields.absent | length), pending: ($r.pending | length)},
                 unauthored: $r.unauthored, fields: $r.fields.drift, absent: $r.fields.absent,
                 pending: $r.pending, tenants: $r.tenants}}'
}

# ---------------------------------------------------------------------
# FILING — the packet is the record. The open packet was opened by this
# unit's ExecStartPre (boss-maintenance-wrap.sh); this writes the
# measurement onto it with PATCH /api/jobs/{id}/metadata, which MERGES
# top-level keys server-side. Never the full job PUT: that REPLACES, so
# every annotation would depend on reconstructing the whole envelope
# correctly, and a concurrent writer loses.
# ---------------------------------------------------------------------
API_CURL="$SELF_DIR/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh
BOSS_USER='{"id":"automation:protocol-drift","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

api() { # <method> <path> [body]
    local method="$1" path="$2" body="${3:-}"
    if [ -n "$body" ]; then
        # THE BODY TRAVELS AS A FILE. codebase-metrics.sh passed its row as
        # `-d "$body"` and died for two days on `Argument list too long`
        # (fdd10ec8, 436a2e91). This row is smaller, and the cap is the
        # same cap.
        local bodyfile
        bodyfile=$(mktemp -t protocol-drift-body.XXXXXX) || return 1
        printf '%s' "$body" > "$bodyfile"
        "$API_CURL" -fsS -X "$method" -H "x-boss-user: $BOSS_USER" \
            -H "content-type: application/json" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            --data-binary "@$bodyfile" "$BOSS_JOBS_URL$path"
        local rc=$?
        rm -f "$bodyfile"
        return $rc
    else
        "$API_CURL" -fsS -H "x-boss-user: $BOSS_USER" "$BOSS_JOBS_URL$path"
    fi
}

file_row() {
    local row
    row=$(row_json) || exit $?

    local open open_count job_id
    open=$(api GET "/api/jobs?kind=$KIND&status=open&limit=2") || {
        echo "$NAME: could not read the open $KIND packet; the measurement below was computed and NOT filed." >&2
        printf '%s\n' "$row" >&2
        exit 1
    }
    open_count=$(printf '%s' "$open" | jq '(.data // []) | length')
    if [ "$open_count" = "0" ]; then
        echo "$NAME: no open $KIND packet to file onto — ExecStartPre could not open one (the jobs API was unreachable then, most likely). The measurement is below; this run recorded nothing." >&2
        printf '%s\n' "$row" >&2
        exit 1
    fi
    if [ "$open_count" != "1" ]; then
        echo "$NAME: $open_count open $KIND packets — refusing to guess which one today's measurement belongs on." >&2
        exit 1
    fi
    job_id=$(printf '%s' "$open" | jq -r '.data[0].id')

    # Two top-level keys, merged server-side: `measured` is the headline
    # and the method, `drift` the named findings.
    local body
    body=$(printf '%s' "$row" | jq '{measured: .measured, drift: .drift}')
    api PATCH "/api/jobs/$job_id/metadata" "$body" >/dev/null || {
        echo "$NAME: filing the measurement onto ${job_id:0:8} failed. It is below rather than lost." >&2
        printf '%s\n' "$row" >&2
        exit 1
    }

    # THE JOURNAL GETS THE HEADLINE, not a digest of it: the full row is
    # on the packet; the four counts are here, where `systemctl status`
    # shows them, each finding named on its own line.
    printf '%s' "$row" | jq -r --arg id "${job_id:0:8}" '
        .measured as $m | .drift as $d
        | "protocol-drift: \($m.live_admitted) admitted kinds, \($m.authored) authored, \($m.fields_compared) compared at \($m.head // "unknown head" | .[0:8]) — " +
          "\($d.counts.unauthored) live kind(s) the tree does not author, " +
          "\($d.counts.fields) field(s) adrift, " +
          "\($d.counts.pending) authored not yet admitted (lint exit \($m.lint_exit))",
          ($d.unauthored[] | "  unauthored: \(.)"),
          ($d.fields[] | "  adrift: \(.kind).\(.field) — live v\(.live_version), first differs at \(.at) (\(.tree_len) vs \(.live_len) chars)"),
          ($d.pending[] | "  pending: \(.)"),
          "  filed on \($id)"'
}

case "$CMD" in
    row) row_json ;;
    file) file_row ;;
esac
