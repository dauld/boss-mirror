#!/usr/bin/env bash
# a-steps-read-failure-is-not-empty — no jobs handler under
# crates/core/boss-jobs/src/http/ answers a failed steps read with an
# empty list: `list_steps(..)` followed, in the same expression, by
# `.unwrap_or_default()`, `.unwrap_or_else(..)` or `.unwrap_or(..)`.
#
# WHY THIS EXISTS (backlog f6c97006, after c11e9d3c). An empty step list
# is a well-formed, confident claim — "this packet has no steps" — so a
# read that failed and was defaulted shrank whatever the handler counted
# or listed, under a 200. Measured on 2026-09-24 at nine sites in one
# crate: the station queue and load dropped the packet from every
# station whose predicate reads steps (c11e9d3c); the jobs list sent the
# row out with `steps: []`, which is the row the ops-runner skips as
# having no execute step; the detail and its stream drew the packet
# empty; a station claim answered 409 "packet is not at this station";
# the yard lost a stranded green (a gate-run's steps ARE its verdict)
# and called an unread converge "converging". Each was repaired by
# reading what the handler computes: a request that cannot be answered
# without the steps fails, naming the packet (`steps_unreadable` in
# http/mod.rs); a read whose partial answer is the design says which
# part is unread. This lint keeps the shape from coming back.
#
# WHAT IT CHECKS. Every *.rs directly under crates/core/boss-jobs/src/http/.
# From each `.list_steps(` call, the expression is read forward — across
# lines, as rustfmt wraps a long chain — up to the first `;` or `{`: a
# `;` ends the statement, and a `{` opens a `match` or `if let` that
# consumes the Result structurally, which is the repair. A defaulting
# `unwrap_or` in that span is refused, naming file:line of the call.
# Strings and `//` comments are removed first, so prose naming the shape
# (this crate's own doc comments do) is not a finding. `.ok()?` is not a
# default: it propagates the failure as `None`, which the readers that
# use it answer as `unread`.
#
# NO EXEMPTIONS. There is no allowlist and no marker: a handler that
# genuinely wants a partial answer handles the `Err` arm itself, where
# the reader of the code sees what "unread" becomes.
#
# EXIT STATUS: 0 clean, 1 a defaulted steps read (or the self-test
# failed, or nothing was scanned). Reads the working tree with `find`,
# never git.
#
# Usage:  infra/lint/a-steps-read-failure-is-not-empty.sh [--self-test]

set -uo pipefail

NAME="a-steps-read-failure-is-not-empty"
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

HTTP_DIR="crates/core/boss-jobs/src/http"

# The files under judgement: every .rs directly under $1. One definition,
# read by the scan and by the scanned count.
rust_files() { # dir
    find "$1" -maxdepth 1 -name '*.rs' -type f -print0
}

# Every finding under $1, one `<file>:<line>` per line, sorted. Empty
# output = clean.
scan() { # dir
    rust_files "$1" | xargs -0 -r env LC_ALL=C awk '
        FNR == 1 { collecting = 0 }
        {
            line = $0
            # Strings first (a "//" inside one is not a comment), then
            # comments.
            gsub(/"([^"\\]|\\.)*"/, "\"\"", line)
            sub(/\/\/.*$/, "", line)
            if (!collecting) {
                p = match(line, /\.list_steps[ \t]*\(/)
                if (p == 0) next
                collecting = 1; start = FNR; text = substr(line, p); extra = 0
            } else {
                text = text " " line; extra++
            }
            q = match(text, /[;{]/)
            if (q > 0 || extra >= 12) {
                if (q > 0) text = substr(text, 1, q - 1)
                if (text ~ /\.unwrap_or(_default|_else)?[ \t]*\(/) print FILENAME ":" start
                collecting = 0
            }
        }
    ' | LC_ALL=C sort -u
}

# ---------------------------------------------------------------------------
# Self-test — fixtures in a temp directory this run owns, never under
# crates/, where a file is discovered as real. Runs on every invocation:
# mawk (the gate image's awk) matches NOTHING for `\s` or an interval, and
# a scanner that matches nothing passes every tree.
# ---------------------------------------------------------------------------
self_test() {
    local tmp hits want
    tmp="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }

    # Accepted: the repairs (a match on the Result, `?`, `.ok()?`, an
    # `if let Ok`), the handler named list_steps and its route, prose
    # naming the shape in a doc comment and a trailing comment, the shape
    # inside a string, and an unwrap_or_default on the NEXT statement.
    cat > "$tmp/accepted.rs" <<'RS'
/// It used to answer `list_steps(..).unwrap_or_default()`.
pub(super) async fn list_steps<R>(state: State<R>) -> Response {
    let steps = match state.jobs.list_steps(&job.id).await {
        Ok(steps) => steps,
        Err(e) => return steps_unreadable(&job.id, &e),
    };
    let more = state.jobs.list_steps(&id).await?; // not .unwrap_or_default()
    let maybe = state.jobs.list_steps(&id).await.ok()?;
    if let Ok(steps) = state.jobs.list_steps(&id).await {
        let v = serde_json::to_value(&steps).unwrap_or_default();
    }
    let msg = "x.list_steps(&id).await.unwrap_or_default()";
    let steps = state.jobs.list_steps(&id).await?;
    let j = serde_json::to_value(job).unwrap_or_default();
    router.route("/steps", get(list_steps::<R, B>));
}
RS
    hits="$(scan "$tmp")"
    if [ -n "$hits" ]; then
        echo "$NAME: self-test FAILED — every accepted shape must pass; got:" >&2
        printf '%s\n' "$hits" >&2
        rm -rf "$tmp"; return 1
    fi

    # Refused, each on a known line: one line; rustfmt's wrapped chain;
    # unwrap_or_else to an empty Vec; .ok() then a default; unwrap_or.
    cat > "$tmp/refused.rs" <<'RS'
fn a() {
    let steps = state.jobs.list_steps(&job.id).await.unwrap_or_default();
    let steps = state
        .jobs
        .list_steps(&job.id)
        .await
        .unwrap_or_default();
    let steps = state.jobs.list_steps(&id).await.unwrap_or_else(|_| Vec::new());
    let steps = state.jobs.list_steps(&id).await.ok().unwrap_or_default();
    let steps = state.jobs.list_steps(&id).await.unwrap_or(vec![]);
}
RS
    hits="$(scan "$tmp")"
    want="$(printf '%s\n' "$tmp/refused.rs:2" "$tmp/refused.rs:5" "$tmp/refused.rs:8" \
        "$tmp/refused.rs:9" "$tmp/refused.rs:10" | LC_ALL=C sort -u)"
    if [ "$hits" != "$want" ]; then
        echo "$NAME: self-test FAILED — five refused shapes must be named by file:line; got:" >&2
        printf '%s\n' "$hits" >&2
        rm -rf "$tmp"; return 1
    fi
    rm -rf "$tmp"
    echo "$NAME: self-test ok — a match, ?, .ok()?, if-let Ok, the handler and its route, prose and a string pass; one-line, wrapped, unwrap_or_else, .ok() then default, and unwrap_or are each named by file:line"
}

self_test || exit 1
if [ "${1:-}" = "--self-test" ]; then exit 0; fi

# ---------------------------------------------------------------------------
# The tree.
# ---------------------------------------------------------------------------
[ -d "$HTTP_DIR" ] || { echo "$NAME: $HTTP_DIR does not exist" >&2; exit 1; }
hits="$(scan "$HTTP_DIR")"
lint_scanned "$NAME" "$(rust_files "$HTTP_DIR" | tr -cd '\0' | wc -c | tr -d ' ')" "Rust file(s) under $HTTP_DIR"
if [ -n "$hits" ]; then
    while IFS= read -r site; do
        [ -n "$site" ] || continue
        echo "$NAME: $site: a failed steps read is defaulted to an empty list" >&2
    done <<EOF
$hits
EOF
    cat >&2 <<'MSG'

  An empty step list says "this packet has no steps", so a defaulted
  read shrinks whatever the handler counts or lists and the 200 says
  nothing (backlog f6c97006). Handle the Err arm:

    when the answer needs the steps — fail the request naming the
      packet: `Err(e) => return steps_unreadable(&job.id, &e)`
      (crates/core/boss-jobs/src/http/mod.rs)

    when a partial answer is the design — say which part is unread
      (the yard's converge window answers None, and every row it would
      have decided reads `unread`)
MSG
    exit 1
fi

echo "$NAME: ok — no handler under $HTTP_DIR defaults a failed steps read to empty"
exit 0
